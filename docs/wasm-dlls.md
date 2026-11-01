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

- 直接システムコール: ntdll/win32u の syscall スタブ (`mov $id,%eax; mov $helper,%edx; call *%edx;
  ret $n`) を呼ぶインポートは、vnwasm がスタブのバイト列から番号を読み、`vn.syscall(id, argp)` を
  呼ぶ関数に置き換える。signal_wasm.c の `wasm_syscall` が x86 のスタブが呼んだときと同じ形
  (戻り番地 0xffff0020) を作って dispatch_syscall を呼ぶ。APC などでよそへ行ったときだけ x86 の
  実行ループを回す。NtContinue / NtRaiseException / NtSetContextThread は x86 のスタブのまま。
- 境界は JS を通らない: 入口 (`__vn_in<k>(slot, argp, ecx, edx, sp)`) は main モジュールの
  関数テーブルの予約スロット (crates/vnhost/src/wasmdll_slots.rs のダミー関数、全 worker で同じ
  番号) に各スレッドが自分のインスタンスのものを置き、vnhost が関数ポインタで呼ぶ。`vn.x86_call`
  と `vn.syscall` は main モジュールの関数をそのままインポートする。入口ごとのロックもない
  (プロセスの世代番号を atomic で見るだけ)。
- x86 の DLL にしかない関数 (アセンブリ、外したソース) を wasm から呼ぶときは x86 の
  エクスポートの元のアドレス、またはシンボル表 (COFF) のアドレスを呼ぶ。転送エクスポートは
  転送先のインポートを使う。どれでもなければ trap するスタブ (警告を出す)。
- x86 側の大域関数は wasm に飛ばす: Wine の DLL はシンボル表を残しているので、wasm 側にも
  ある大域関数 (`_name@N` / `@name@N` / `_name`) の先頭に `jmp 0xfe000000+スロット` を書く
  (メタデータの patches)。x86 のまま残るアセンブリから呼ばれても C の x86 版は走らず、静的
  データが二重にならない。static 関数は regparm なので対象外。
- モジュールをまたぐ間接呼び出しは型を確かめる: スロットごとの型タグ (vnwasm の type_tag) を
  プロセスごとの配列 (`vn.tags`) に置き、icall はタグが合うときだけテーブルを直接呼ぶ。合わない
  とき (DLL によってプロトタイプが違う。kernel32 の void EnterCriticalSection → ntdll の NTSTATUS
  RtlEnterCriticalSection など) は x86 の呼び方で入口を通る。
- ntdll: データのエクスポート (`__wine_syscall_dispatcher`、`__wine_unix_call_dispatcher`、
  `LdrSystemDllInitBlock`、`Wow64Transition`、`Nls*`) は x86 イメージのものを使う (unix 側が
  そこに書く)。winecrt0 はリンクしない。`build_ntdll_module` は `__ImageBase` を使う (関数の
  アドレスはテーブルのスロットなので)。KiUserCallbackDispatcher は x86 のまま (その
  NtCallbackReturn でコールバックの実行ループが終わる)。`__wine_push_frame`/`__wine_pop_frame`
  は本物で、`__TRY` だけがフレームを作らない。

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
Wine 側の変更 (patches/wine-wasm.patch): NtCurrentTeb、`__ASM_DEFINE_FUNC` と
`__ASM_DEFINE_POINTER` を空に、`__TRY` は SEH フレームを作らない、x86 の組み込み関数と
i386 専用アセンブリ (user32、ntdll の x87/fxsave) を使わない、ntdll のデータエクスポートを extern に。
compiler-rt の 64/128 ビット演算と musl の libm は build-wasm-dll.py がコンパイルして入れる。
ntdll、kernelbase、kernel32、user32、gdi32、dsound がビルドできる (msvcrt/ucrtbase はまだ:
cpp.c、except_i386.c、math.c の x87 アセンブリ)。

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
- ntdll まで (ntdll、kernelbase、kernel32、user32、gdi32、dsound の 6 つ、直接システムコール、JS を
  通らない境界、x86 関数の飛ばし先の書き換え): node では本編 100 秒の CPU が 9.5〜10.0 秒 (wasm なし
  18.0〜18.2 秒) だが、Chromium (Android 相当、同じ手順) では本編 0.274〜0.331 コア (平均 0.293、
  6 回) に対し wasm なし 0.264〜0.294 (平均 0.282、4 回)、タイトルは 17.0〜33.1 秒 (中央値 19.6) に
  対し 18.5〜21.8 秒 (19.8)。user32+dsound だけのとき (+11%) より差は縮んだが、良くはならない。
  本編 1 分の境界は入口 320 万回、x86 呼び出し 6.5 万回、直接システムコール 35 万回 (node)。
  既定はオフのまま (ストアに i386-wasm を入れたときだけ使う)。
- 制限: wasm のフレームをまたぐ x86 例外の巻き戻し、wasm 内の `__TRY` は何も捕まえない、
  setjmp/longjmp は未対応。
- 次: msvcrt/ucrtbase。Reunion のように処理の大半がゲーム本体のゲームでは上限は数 %。
