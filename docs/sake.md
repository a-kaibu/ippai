# sake: Rust で一から書いた Win32 層

ゲームの x86 コードを koji の JIT で動かし、Windows API を Rust (crates/sake、wasm) で実装する。

## 仕組み

- PE ローダー (pe.rs): exe と、ゲームに同梱された DLL は x86 としてマップする。システム DLL
  (specs.txt にあるもの) のインポートはトラップ番地 `0xfe000000 + 4*i` に解決し、x86 が
  そこを呼ぶと Rust のハンドラ (`api::implement` で登録) が動く。引数のバイト数は Wine の
  .spec から取った specs.txt。msvcrt の変数 (`_iob` など) はデータとして渡す。
- 速い経路 (fast.rs): 呼び出しの多い関数は x86 のスタブをゲストメモリに置き、よくある場合は
  JIT のコードの中で済ませて、残りだけトラップに飛ぶ。Enter/LeaveCriticalSection、
  TlsGetValue、Get/SetLastError、IsDBCSLeadByte、RECT 関数、msvcrt の `_errno`、
  小さいヒープブロック (下記)。`SAKE_NO_FAST=1` で全部トラップに戻る。
- ヒープ (mem.rs): dlmalloc。512 バイト以下は 16 バイト刻みのクラスごとに空きリストを
  ゲストメモリに持ち、HeapAlloc/HeapFree/HeapReAlloc と msvcrt の malloc/free の x86
  スタブがスピンロックの下で取り出し・戻しをする。空のときだけ Rust が数十個まとめて補充する。
  クラス内に収まる HeapReAlloc はその場で済ませる。
- 実装しているもの: kernel32 (ファイル、スレッド、同期、SRW/条件変数、.ini)、user32
  (ウィンドウ、メッセージ、ダイアログ、Button/Static/Edit コントロール、入力)、gdi32 (DIB、文字は
  FreeType ではなく ab_glyph と ttf-parser、AddFontResource)、msvcrt (crt.rs)、dsound
  (Rust でミックス)、DirectInput 7/8、DirectDraw 1〜7 (ソフトウェア、ddraw.rs)、
  d3d9 (kura の WebGPU D3D9 に橋渡し)、CryptoAPI のハッシュ、DxDiag、レジストリ、
  shell32/shlwapi の一部、Uniscribe の一部 (usp.rs)、DirectShow のフィルタグラフ (dshow.rs: 動画は
  すぐ終わったことにする)。
- DirectDraw: サーフェスはゲストメモリの画素と、その上に被せた GDI ビットマップ。Blt/BltFast
  (カラーキー、拡大縮小、塗りつぶし)、フリップ、Lock、GetDC。プライマリに描いたとき・
  Unlock・ReleaseDC・Flip でウィンドウに BitBlt する。Direct3D は出さないので、
  ゲームはソフトウェア描画に回る。
- D3D11 / DXGI (d3d11.rs): kura の WebGPU D3D11 (Wine 経路と同じ実装) に橋渡し。vtable の
  各メソッドはトラップで、`kura::d3d11::call` に引数を渡すだけ。メソッド表は
  `host/gen-d3d11-methods.py` が d3d11_methods.rs として生成する。
- D3D10 / 10.1 (d3d10.dll、d3d10_1.dll): D3D11 の同じオブジェクトの別の面 (kura の d3d11/d3d10.rs、
  インターフェース 95〜111)。メソッドは D3D11 のデバイス・コンテキスト・オブジェクトの呼び出しにして、違う
  構造体 (D3D10_BLEND_DESC、深度ステンシルビューの記述、整数のビューポート、バッファの記述、Map の結果)
  だけ変換する。D3D10CompileShader は hlslc。リフレクションと Effects 10 は未対応。テストは
  host/tests/d3d10test.c (tests/gfx.mjs)。
- d3dcompiler_33〜47 (d3dcompiler.rs, d3dreflect.rs): 自前の HLSL コンパイラ (crates/hlslc:
  プリプロセッサ、構文解析、関数はすべてインライン展開、SM4/5 の DXBC を出す) で D3DCompile、
  D3DPreprocess、署名ブロブ、D3DReflect。出力は dxwgsl (crates/dxwgsl) の DXBC→WGSL がそのまま読む。
  テストは `cargo test -p hlslc` (WGSL まで通して naga で検証) と host/tests/hlsltest.c。
  D3D9 向けの ps_2_0〜3_0 / vs_2_0〜3_0 も出せる (hlslc の sm3.rs: int と bool は float で扱い、
  アセンブリを hlslc::d3d9asm で組み立てて CTAB を付ける。ビット演算・switch・continue・
  テクスチャのオフセットはエラー)。d3dx9_24〜43 の D3DXCompileShader(FromFile)・
  D3DXGetShaderConstantTable・ID3DXConstantTable (d3dx9shader.rs) がこれを使う。
  テストは host/tests/d3dx9shader.c (定数の書き込みを偽のデバイスで表示)。
  D3DXCreateEffect(Ex/FromFile) にエフェクトのソース (.fx) が来たときは hlslc の fx.rs で fx_2_0 の
  バイナリにしてから d3dx9fx.rs に渡す (technique/pass、レンダー・テクスチャステージ・サンプラー
  ステート、sampler_state、アノテーション、`compile vs_2_0 f(定数)`、asm {}、VertexShader 変数)。
  プリシェーダーは作らない (定数でない式のステートはエラー)。テストは host/tests/d3dx9effect.c。
- Direct2D (d2d.rs) と DirectWrite (dwrite.rs) は自前。D2D の描画は WebGPU: 変換・クリップ・
  文字の配置は CPU で決めて、矩形・楕円・グリフを kura::d3d11::d2d の Item として集め、EndDraw/Flush
  で 1 回のインスタンス描画にする (グリフは R8 のアトラス、グラデーションは 256 texel のランプ
  テクスチャ)。確認は tests/gfx.mjs の d2dtest。
- specs.txt にない DLL は C:\ の `windows\system32` にあれば x86 として読む。
- フォント一覧はレジストリの Fonts キー (DirectWrite のシステムコレクションが読む)。
- WH_CBT フック (HCBT_CREATEWND): MFC はこれでウィンドウを結び付けるので、無いと LiveMaker が
  ウィンドウを作れずに終わる。

## V8 向けの調整 (2026-11-02、docs/v8-wasm-shape.md を受けて)

- メモリは 65535 ページ固定 (`INITIAL_MEMORY=4294901760`、伸長なし)。JIT のモジュールの
  import (最小 = 最大 = `MEMORY_MIN_PAGES`) と同じでないと JIT のモジュールが link できない。
  Android ではメモリサイズの読み直しが関数ごとに 1 回になる。4GB を最初からコミットするので、
  実機で確保できるかは最後の確認項目 (docs/translator.md)。
- ウィンドウの面は表示側 (kura display) が直接読む (`vnhost_window_draw_shared`)。
  前は 1 フレームごとに面を表示側のバッファへコピーしていて、Reunion の本編でゲームスレッドの
  17% が memmove だった。
- ハンドルの表 (ウィンドウ、GDI オブジェクトなど) は SipHash ではなく Fx ハッシュ (lib.rs `Fx`)。
- Reunion の本編 (Chromium、perf): ゲームスレッドの 57% がゲストのコード、sake の API 実装は数 %。
  BGI のタイトルでは sake 側は 1% 未満で、残りは JIT とその補助関数。範囲チェック強制
  (`--js-flags=--wasm-enforce-bounds-checks`、Android と同じ) で本編の CPU は約 11% 増える。
- 測り方: `perf record -a` + `CHROME_ARGS="--no-sandbox --no-zygote --js-flags=--perf-basic-prof"`、
  名前付きの sake.wasm は `SAKE_LDEXTRA=--profiling-funcs bash crates/sake/build.sh`。
  game.html の CPU (コア) は待ちの外の時間で、`secs` が違うと場面が違うので比べられない。

## 64 ビット (PE32+、2026-11-02)

- sake.wasm は 1 つのまま。exe が PE32+ なら起動時に `mem::set_x64()` で x64 モードにし、koji を
  long mode で動かす。ゲストのアドレスは 32 ビットのまま (メモリは 4GB で折り返す)。
- ローダー (pe.rs): ImageBase が 4GB より上で再配置のない exe (0x140000000 など) は下位半分
  (0x40000000) に置き、上位半分は koji の `rip_hi` が持つ (`pe::ALIAS`)。プログラムに返す
  その範囲のアドレスは `mem::widen` で上位を戻す。DIR64 再配置、8 バイト IAT、TLS、.pdata。
- 呼び出し規約: 引数は rcx/rdx/r8/r9 とスタック (`Ctx::a64`)、浮動小数は xmm0-3、戻り値は
  rax/xmm0。ゲストへの呼び出しは `call64` (16 バイト境界、ホーム領域)。COM の vtable は 8 バイト
  (`api::vtable`)。TEB64/PEB64、MSG/WNDCLASS(EX)/CREATESTRUCT/OVERLAPPED などの x64 配置。
- 例外 (seh64.rs): .pdata/UNWIND_INFO による仮想巻き戻し、例外ディスパッチ (VEH → フレームの
  ハンドラ)、RtlUnwindEx (STATUS_UNWIND_CONSOLIDATE、STATUS_LONGJUMP)、RtlCaptureContext、
  RtlLookupFunctionEntry、RtlVirtualUnwind、RtlAddFunctionTable、setjmp/longjmp。
- C/C++ ランタイム (msvcrt、ucrtbase、msvcp*/vcruntime*) は crt.rs ではなく、プレフィックスの
  `C:\windows\system64` に置いた Wine の x86_64 PE ビルドを読む。system32 にない DLL も
  system64 から読み、アーキテクチャの違うファイル (ゲスト同梱の 32 ビット d3dcompiler_47 など)
  は飛ばす。
- 動画: Media Foundation は MFStartup が失敗する (動画を飛ばす)。DirectShow のグラフに足される
  EVR (CLSID_EnhancedVideoRenderer) は何もしないフィルタ。
- 確認: Artemis x64 の 3 本 (遥かなるニライカナイ、GALS FICTION、セレクトオブリージュ) が
  Chromium でタイトル → 本編。SakanaGL x64 (でか乳巫女) もタイトル (約 30 秒) → 本編。
- SakanaGL の sakanagl.dll は /arch:AVX2。koji のインタプリタが VEX の 128 ビット形
  (AVX/AVX2)、FMA3、SSE3/SSSE3/SSE4.1 を実行する (interp/vex.rs: VEX のコードを名前で
  旧来のコードに写し、NDS は Cpu::vex_tmp (xmm16) に src1 を写してから実行)。ymm (256 ビット)
  は #UD、CPUID は AVX を報告しない。
- 64 ビットの JIT (koji jit/long.rs、long/sse.rs) は SSE〜SSE4.1 と VEX.128 の主な形を wasm SIMD
  に訳す (VEX はインタプリタと同じ表で旧来の形に写し、NDS は src1 を読むだけ)。CL 指定の
  シフト (32/64 ビット)、bsf/bsr、bt/bts/btr/btc も訳す (未定義のフラグはインタプリタと同じく
  そのまま)。SakanaGL の起動 (最初の描画) は 113 秒 → 8 秒。`tools/jit64test` は xmm も比べ、
  SSE/VEX の iced の Code 578 種をランダムに混ぜる。
  `tools/jit64test native` で実 CPU と照合済み (rcp/rsqrt の近似値を除き一致)。
  SSE4.1 の dpps/dppd、mpsadbw、phminposuw と SSE4.2 の pcmpXstrX も実行する
  (host/tests/sse4.c: native と一致、-mavx の VEX 形も)。
- gl.rs: x64 では引数を位置で並べる ('d'/'f' は xmm、他は a64 の下位)。glShaderSource の
  ポインタ配列は 8 バイト刻み。拡張関数名は ARB/EXT を外した名前でも探す。
- user: DPI API (GetDpiForWindow など 96 固定)、ShowWindow(SW_MAXIMIZE) で画面いっぱいに広げる。
- 構造体: PAINTSTRUCT、BITMAP、DIBSECTION、SYSTEM_INFO、MEMORY_BASIC_INFORMATION は x64 の
  形で書く (host/tests/struct64.c)。未対応: XAudio2 (32 ビットのみ)、d3d9 ブリッジ。

## 16 ビット (Win16)

- NE 形式の exe (Windows 3.x) は `w16::start` が読む (w16.rs)。koji のインタプリタに 16 ビット
  保護モード (`Cpu::mode16`) があり、セグメントレジスタの値とベース (`sreg`/`sbase`)、far
  call/jmp/retf/iret、lds/les を扱う。セレクタは `Cpu::ldt` の表 (セレクタ番号ごとに u32 のベース)
  の添字で、ベースはゲストメモリのブロックなので far ポインタの線形アドレスはベース + オフセット。
  16 ビットのコードは JIT にかけない (インタプリタだけ)。
- インポートはトラップ範囲 (0xffc00000) をベースにしたサンクセグメントへの far ポインタ。呼ばれると
  pascal の引数を 16 ビットのスタックから読み (型は Wine の .spec から作った specs16.txt)、
  ほとんどは引数を変換して sake の 32 ビットのハンドラを直接呼ぶ (w16api.rs の `SIMPLE` 表)。
  16 ビットのハンドルは 0x10000 以上の sake のハンドルに小さい番号を割り当てる (`h16`/`h32`)。
- ウィンドウプロシージャ: RegisterClass の 16 ビットのプロシージャは 32 ビットのトラップ (host_fn)
  に包み、`call16` で 16 ビットのスタックに積んで呼ぶ。メッセージは `to16`/`to32` で変換する
  (CREATESTRUCT、MINMAXINFO、NCCALCSIZE、WINDOWPOS、WM_COMMAND などの詰め直し)。Win16 の
  プログラムの中では `crate::call` は 32 ビットのコードを走らせず、トラップのハンドラを直接呼ぶ。
- KERNEL: InitTask、Global/Local メモリ (Global はセレクタ、64KB を超えるものは __AHINCR=8 刻み)、
  ファイル (_lopen など)、DOS3Call / INT 21h の主な機能。USER: ウィンドウ、メッセージ、メニューと
  ダイアログのリソース (NE の形式から 32 ビットに作り直す)、wsprintf。GDI: 文字、DIB、パレット。
- 確認: Vector の Win3.1 フリーソフト「ガルシアの髑髏」(ハイパーリンク・ゲームブック、
  SHOW.EXE + GALCIA.IV) が Chromium でタイトル → 本編。

## 動作確認したゲーム (体験版)

| ゲーム (エンジン) | 状態 | 確認した場所 |
|---|---|---|
| Reunion (KiriKiri) | 本編まで | node、Chromium |
| 裏教師 (Bishop) | 本編まで | node |
| 痴覚過敏 (EntisGLS) | 本編まで | node |
| ヴェルディア幻奏曲 (Escude) | 本編まで | node |
| こんな安楽椅子探偵は嫌だ (YU-RIS) | 本編まで | node、Chromium (GDI で描く) |
| 爆乳遊廓 (Softpal) | タイトル (メニューのボタンが描かれない。押すことはできる) | Chromium (D3D9) |
| アマカノ (CatSystem2) | 本編まで (名前入力ダイアログ → マップ) | Chromium (D3D9) |
| つぼい君のスイッチ (DxLib) | 本編まで | Chromium (D3D9)。D3D9 の SHADEMODE flat に対応して表示が正しくなった |
| ナルキッソス SIDE 2nd (NScripter) | プロローグまで | node |
| ろーらいず!!! (SiglusEngine) | 本編まで | Chromium (D3D9)。日本語版 Windows とタイムゾーンの確認、静的リンクの D3DX フォント用に Uniscribe (usp.rs) |
| 3Ping Lovers! (RealLive) | 本編まで (「はじめから」の後に Enter が要る。確認の表示が見えない) | node、Chromium (GDI。画面が 1280x720 以外だと rl_d3d.dll の D3D9 で拡大する) |
| クダンノフォークロア (BGI) | タイトル (クリックに反応しない) | Chromium (D3D9)。待機可能タイマー、D3DXCreateEffect のため Wine の d3dx9_43.dll (PE) をゲームの横に置く。体験版の印のファイル KudanNoFolkloreTV を消す (インストール後の状態) |
| 俺嫁 (LiveMaker、D3D11 + Direct2D) | 本編まで | Chromium。注意画面、タイトル、本編の文字 (DirectWrite) |
| WolfRPG | 起動しない | 保護付き exe。アンチデバッグの後で止まる。追っていない |

node では D3D9 は描かれない (WebGPU がない) ので、D3D9 のゲームは Chromium で見る。

## 動かし方

- ビルド: `bash crates/sake/build.sh` → node 用 `/home/user/work/sake-out/sake.js` と
  ページ用 `web/public/sake/` (dist に使うときは `web/dist/sake/` にコピー)。
- node: ゲームのフォルダで `SAKE_C=<prefix>/drive_c node sake.js /abs/game.exe`。
  `IPPAI_SCREENSHOT=out.ppm`、`IPPAI_INPUT="秒:種類,x,y,データ;..."` (1 移動、2/3 ボタン、
  5/6 キー)。ボタンはホバーの移動を先に送らないと反応しないゲームが多い。
- ページ: `ippai pack <dir> <name>` してから
  `play.html?engine=sake&autostart&run=Z%3A%5Cvnvm%5Cgame%5C<exe>&mount=/vnvm/game=<name>`。
  `tests/play.mjs` でヘッドレスのスクリーンショット。
- 調べるとき: `SAKE_WATCH=<16進番地>` (API 呼び出しごとにその番地の値を見て、変わったら表示)、`IPPAI_D3D_TRACE=1` (描画ごとの頂点も表示)、`SAKE_TRACE=1` (+ `SAKE_TRACE_FILTER=a,b`、`SAKE_TRACE_AFTER=ms`)、
  `SAKE_TRACE=3` (落ちたときにスレッドごとの直前の呼び出し)、`SAKE_API_STATS=秒`、
  `SAKE_TRACE_FAULT`、`SAKE_TRACE_FILES`、`SAKE_TRACE_LOAD`、`SAKE_TRACE_MSG`、
  `SAKE_TRACE_INPUT`、`SAKE_TRACE_FONT`、`IPPAI_JIT_STATS` (JIT が実行ループに戻る場所)、
  `SAKE_WINEDEBUG=<flags>[:chan,...]` (Wine の PE DLL のデバッグ出力。1 FIXME、2 ERR、4 WARN、8 TRACE)、
  `IPPAI_D3D_SPV=<dir>` (D3D11 シェーダの SPIR-V を書き出す)。
- 確認用の小さなテスト: `ENGINE=sake node tests/gfx.mjs` (d3d11test、d2dtest、ddrawtest が通る。
  d3d8test は sake に D3D8 が無いので落ちる)。
