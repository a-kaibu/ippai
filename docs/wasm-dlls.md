# Wine の PE DLL を直接 wasm にする (AOT)

Wine の PE 側 DLL (user32 など) を、x86 として JIT するのではなく clang で直接 wasm32 に
コンパイルしておく。ARM64EC/CHPE と同じ考え方で、ゲームの x86 コードとだけ JIT が働く。

## 仕組み

- x86 の DLL (vnbox の Wine 11.20 そのもの) は今まで通り Wine のローダーがマップする。
  インポート、IAT、リソース、遅延ロードはそのまま使う。
- 同じ DLL を wasm にしたもの `lib/wine/i386-wasm/<name>.wasm` (例 `user32.dll.wasm`) が
  あれば、マップした直後 (virtual.c の virtual_map_image → signal_wasm.c の
  `wasm_dll_mapped`) にエクスポート表とエントリポイントを `0xfe000000 + スロット` に書き換える。
  この範囲は x86 コアのトラップ範囲 (0xfe000000〜0xffff1000) で、x86 から呼ぶとコアが止まり
  vnhost (crates/vnhost/src/wasmdll.rs の `vnhost_wasm_enter`) が x86 スタックから引数を
  読んで wasm の関数を呼ぶ。サイズとタイムスタンプが x86 DLL と合わなければ使わない。
- wasm 側の関数ポインタは x86 アドレスと同じ値 (`__table_base` = 0xfe000000 + スロット)。
  `call_indirect` はすべて「wasm DLL のスロットなら table 経由、そうでなければ x86 を呼ぶ」に
  書き換える。コールバック (WndProc など) もこれで両方向に通る。
- インポートした関数は IAT の値を読んでそこを呼ぶ。x86 を呼ぶときは引数を x86 スタックに
  積んで `wasm_call_x86` (signal_wasm.c) が x86 コアを入れ子で回し、戻り番地
  0xffff0020 のトラップで戻る。
- 呼び出し規約は .spec から: stdcall (既定)、cdecl/varargs、thiscall/fastcall (ECX/EDX)、
  値渡しの構造体 (x86 スタック上のコピーへのポインタ)、浮動小数の戻り値 (ST0)。
- wasm のスタックは x86 のスタック (入るときに `__stack_pointer` = ESP & ~15)。
  `NtCurrentTeb()` は `vn.teb` グローバル。
- スレッド (worker) ごとに funcref テーブルとインスタンスを持つ。スロットはプロセスごとに
  割り当て、各スレッドは wasm に入るときに足りないモジュールを作る。データの再配置と
  コンストラクタは最初の呼び出しで 1 回だけ。

## ビルド

```
cargo build --release -p vnwasm
python3 hangover/build-wasm-dll.py --out /home/user/work/wasm-dlls user32
cp /home/user/work/wasm-dlls/user32.dll.wasm /opt/wine-vnbox/lib/wine/i386-wasm/
```

build-wasm-dll.py は PE ビルドツリー (`$WINE_TOOLS`) の Makefile と compile_commands.json
から DLL のソースと静的ライブラリを集め、`--target=wasm32-unknown-unknown -D__i386__
-D__vnwasm_pe__` でコンパイルして `wasm-ld -shared` でリンクし、crates/vnwasm で後処理する
(インポートの差し替え、エントリの生成、メタデータ節 `vnwasm`)。`__vnwasm_pe__` のときの
Wine 側の変更 (patches/wine-wasm.patch): NtCurrentTeb、`__ASM_DEFINE_FUNC` を空に、
`__TRY` は SEH フレームを作らない、user32 の i386 専用アセンブリを使わない。

環境変数: `VNVM_WASM_DLLS=0` (使わない) / `user32,gdi32` (これだけ使う)、`VNVM_WASM_DIR`、
`VNVM_WASM_STATS=1` (wasm への入口と x86 呼び出しの回数)、`VNVM_TRACE_WASM=1`。

## 現状 (2026-11-01)

- user32: cmd、GUI テスト (hangover/tests の wintest)、Reunion の本編まで node で動く。
  Reunion のタイトルまで 18.4〜19.5 秒 (wasm なし 19.7〜20.6 秒、node、2 回ずつ)。
- dsound: Reunion の本編まで動き、ストリームに書かれる音の大きさ (`VNVM_AUDIO_STATS=1`、5 秒ごとの
  RMS) は x86 版と同じ。x86 版が gcc の組み込みや x87 で済ませていた数学関数 (exp2 など) は
  emscripten 同梱の musl から必要な分だけリンクする。
- どこに効くか (Reunion、node、`VNVM_JIT_PROFILE` の 64KB ごとの集計を DLL ごとに): タイトルまでは
  ゲーム本体 95%、Wine の DLL は kernelbase 1.7%・ntdll 1.6% など合計 4% ほど。本編 100 秒では
  ゲーム本体 85%、wuvorbis (ゲームのプラグイン) 10%、dsound 2.2%、ntdll 1.0%、kernelbase 0.8%、
  user32 0.1%。user32+dsound を wasm にしても node の CPU 時間 (100 秒、本編を進める) は
  18.8〜19.5 秒で変わらない (なし 18.8〜18.9 秒)。この種のゲームでは差が出ない。
- Chromium (tests/android-like.sh で Android 相当、web/game.html、60 秒の本編、2 回ずつ × 2):
  user32+dsound を wasm にすると悪くなる。本編の CPU 0.286〜0.330 コア (平均 0.314) に対し
  wasm なし 0.275〜0.285 (平均 0.282)、タイトルは 21.5〜33.4 秒に対し 19.1〜21.9 秒。
  dsound のミキサーのスレッドは 0.02 コアのままで、ゲームのスレッドが 0.19 → 0.22 に増える。
  原因は境界のコスト: 本編 1 分で x86→wasm の入口 13 万回、wasm→x86 の呼び出し 26 万回
  (x86 の入れ子の実行ループ、入口ごとのロック)。加えて各 worker でモジュールをコンパイルする。
  このため既定ではオフのまま (ストアに i386-wasm を入れない)。効果を出すには、境界を
  ほとんど越えない単位 (ntdll まで含めて丸ごと、直接システムコール) が必要で、Reunion の
  ような処理の 9 割がゲーム本体のゲームでは、それでも数 % の上限しかない。
- 制限: wasm のフレームをまたぐ x86 例外の巻き戻し、wasm 内の `__TRY` は何も捕まえない、
  setjmp/longjmp は未対応。
- 次: gdi32 など、wasm からの直接システムコール (今は x86 の ntdll/win32u スタブを経由)。
