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
- 制限: wasm のフレームをまたぐ x86 例外の巻き戻し、wasm 内の `__TRY` は何も捕まえない、
  setjmp/longjmp は未対応。
- 次: gdi32 など、wasm からの直接システムコール (今は x86 の ntdll/win32u スタブを経由)。
