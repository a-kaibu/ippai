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
  `tools/win32md` (Microsoft の Win32 メタデータ) が d3d11_methods.rs として生成する。
- D3D10 / 10.1 (d3d10.dll、d3d10_1.dll): D3D11 の同じオブジェクトの別の面 (kura の d3d11/d3d10.rs、
  インターフェース 95〜111)。メソッドは D3D11 のデバイス・コンテキスト・オブジェクトの呼び出しにして、違う
  構造体 (D3D10_BLEND_DESC、深度ステンシルビューの記述、整数のビューポート、バッファの記述、Map の結果)
  だけ変換する。D3D10CompileShader は hlslc。D3D10ReflectShader は D3DReflect と同じオブジェクトを
  D3D10 のインターフェース (短い記述の構造体) で返す (winref d3d10refl)。Effects 10 は未対応。テストは
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
- DLL はゲームが同梱しているもの (exe のフォルダ、カレントディレクトリ、SetDllDirectory) と sake 自身のものだけ。
  C:\windows\system32 (SysWOW64) からは読まない: そこに古い Wine prefix の PE DLL が残っていても使わない。
  sake が実装していない DLL は、それが無い Windows と同じく LoadLibrary が失敗する。
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
- ローダー (pe.rs): ImageBase が 4GB より上で再配置のない exe / DLL (0x140000000 など) は下位半分
  (0x40000000) に置き、上位半分は koji の `rip_hi` が持つ。その確保 (mem.rs の Region) に上位半分を
  覚えさせ (`mem::set_high`)、プログラムに返すその範囲のアドレスは `mem::widen` で上位を戻す。
  下位半分どうしが重ならない限りいくつでも置ける (2026-11-13、#62)。DIR64 再配置、8 バイト IAT、TLS、.pdata。
- 4GB より上を指定した VirtualAlloc / VirtualAllocEx / NtAllocateVirtualMemory (#62): 下位半分が空いていれば
  そこに確保して `set_high` し、指定どおりのアドレスを返す (k32.rs `alloc64`)。空いていなければ
  ERROR_INVALID_ADDRESS (Windows なら成功するところ: `sake: VirtualAlloc above 4G ...` をログに出す)。
  VirtualFree / VirtualProtect / VirtualQuery は上位半分も照合し、下位半分に別の確保 (上位の違うもの、
  普通の下位のもの) がある番地は空き (MEM_FREE) として答える。host/tests/highmem.c (exe と DLL 2 つを
  4GB より上に固定、確保と解放、呼び合い) と mem.rs の PBT。
- アドレスの扱いの検討 (2026-11-07、レビューの P1。2026-11-13 に下の「ALIAS を表に」を実施): いまのゲストのアドレスは 4GB で折り返し、4GB より上は
  再配置のない exe 1 つ分だけ (`pe::ALIAS` + `rip_hi`)。sake が返すメモリ (VirtualAlloc、ヒープ、
  スタック、DLL) は全部下位 4GB なので、64 ビットのポインタでも上位 32 ビットは 0 で、普通のプログラムは
  これで足りる (Artemis / SakanaGL / Godot / Unity 2019 は本編まで)。壊れるのは (1) 再配置のない
  4GB 超えの PE が 2 つ以上 (2) 上位ビットを使うポインタ (タグ付き、`0x7ff...` 固定の番地を前提にする
  JIT やランタイム) (3) 4GB を超えて使うもの (Unity 6 の 256MB 単位の予約が積み重なる)。案と判断:
  - ALIAS を小さな表 (画像ごとに lo/hi/size) にする: (1) だけ直る。必要なゲームが出たら。
  - wasm memory64: (3) まで直るが、全アクセスが i64 になり上限比較も消えない (v8-wasm-shape.md:
    trap handler があっても比較が 1 つ要る)。Android は trap handler なしで今も比較がある。全体が遅く
    なるので、4GB を超えるゲームのための別ビルドとしてだけ検討する。
  - ソフトのページ表 (上位ビットで引く): 全アクセスに表引きが入る。採らない。
  結論: いまは 4GB 折り返しのまま (こた 2026-11-07「互換性壊していいから直せ kiss」)。(1)〜(3) に当たる
  ゲームが出たら、まず ALIAS の表、次に memory64 の別ビルドを計測してから決める (当たったことを
  クラッシュログで見分ける印はまだない: 次に x64 を触るときに足す)。
- 呼び出し規約: 引数は rcx/rdx/r8/r9 とスタック (`Ctx::a64`)、浮動小数は xmm0-3、戻り値は
  rax/xmm0。ゲストへの呼び出しは `call64` (16 バイト境界、ホーム領域)。COM の vtable は 8 バイト
  (`api::vtable`)。TEB64/PEB64、MSG/WNDCLASS(EX)/CREATESTRUCT/OVERLAPPED などの x64 配置。
- 例外 (seh64.rs): .pdata/UNWIND_INFO による仮想巻き戻し、例外ディスパッチ (VEH → フレームの
  ハンドラ)、RtlUnwindEx (STATUS_UNWIND_CONSOLIDATE、STATUS_LONGJUMP)、RtlCaptureContext、
  RtlLookupFunctionEntry、RtlVirtualUnwind、RtlAddFunctionTable、setjmp/longjmp。
- C/C++ ランタイム (2026-11-06、#29): x86 と同じ crt.rs が x64 も受け持つ (msvcrt、ucrtbase、
  vcruntime140(_1)、api-ms-win-crt-*)。Wine の x86_64 PE DLL (`C:\windows\system64`) は使わない。
  - 書式・変換の中身は crates/crt (ゲストのメモリに触らない純粋な Rust): printf (UCRT の
    `__stdio_common_*` のオプションビット、旧 msvcrt の書式)、scanf (_s のサイズ引数)、
    strto*/ato*/_itoa*、strftime/mktime/gmtime、_s 文字列関数。crt.rs は引数の取り出し
    (x86 のスロット / x64 の位置、`Args`) とゲストメモリへの読み書きだけ。
  - strtod の ERANGE は Windows に合わせる: 結果が 0 に丸まったときと無限大だけ。非正規化数は
    エラーにしない (C11/glibc は不正確な非正規化数でも ERANGE。Wine の ucrtbase テストが Windows の値)。
  - C++ 例外 (cxx64.rs): _CxxThrowException、__CxxFrameHandler3/4 (FH4 の圧縮 FuncInfo)、
    __C_specific_handler、catch funclet の呼び出しと継続、再送出、std::uncaught_exceptions。
    Wine の except.c / handler4.c の移植。
  - msvcp140 (msvcp.rs、x64 のみ): locale の最小限 (_Locimp "C"、_Lockit、_Locinfo、
    ctype<char> の tolower/toupper/widen/narrow（単文字・範囲、独自 facet の仮想呼び出し）)、_Xbad_alloc/_Xlength_error/_Xout_of_range/_Xregex_error (std の例外を
    throw)。host/tests/msvcpchar.c は全256バイト・空範囲・境界・独自facetを実EXEで検証。iarsys64.dll (Artemis) が使う分。それ以外の msvcp140 の関数は未実装で呼ぶと止まる。
  - _create_locale: msvcrt / msvcr* は threadlocinfo (x64 はポインタ幅で並べ直す)、UCRT は
    __crt_locale_data (先頭が pctype / mb_cur_max / コードページ)。winref crtlocale (#58)。
  - _set_se_translator (cxx.rs / cxx64.rs): try ブロックを持つ関数のフレームハンドラが構造化例外 (/EHs
    でないもの) を受けると、スレッドの translator に飛ぶ (戻り先は TRANSLATE_TRAP)。x64 も x86 も、
    translator が投げた C++ 例外は呼んだフレームでだけ catch を探し、無ければ C++ 例外を捨てて元の
    例外の探索を続ける (上の try ブロックを持つフレームでまた translator が呼ばれる。Windows の
    vcruntime140 は catch するフレームまでの try ブロックを持つ関数の数だけ呼ぶ)。x64 は seh64 の walk
    が TRANSLATE_TRAP を通ったら Dispatch.guard にそのフレームを覚え、それより内側のハンドラは呼ばず、
    外側に出たら捨てる。x86 は MSVC の TranslatorGuardHandler と同じ形。テスト: host/tests/msvcseh.cpp
    (x64 と x86、winref host_msvcseh-32/64 は Windows の答えと一致)。
  - 未対応: printf の位置指定引数 (%1$d)。x86 の msvcp は thiscall の橋渡しが要るので #30 で。
  - テスト: host/tests/x64crt.cpp (clang の x86_64-pc-windows-msvc、ucrtbase/vcruntime140 を
    import する /MD 相当のプログラム。printf/scanf/変換/_s/時刻/C++ 例外/SEH)、
    host/tests の crtstdio・crtfiles・fileseek・cmp64・x64seh を x86-64 でもビルドして
    x86 と同じ期待値で照合 (tests/host-tests.sh の `x64/<name>`)。crates/crt は glibc との
    差分 PBT (tests/glibc.rs)、cargo-fuzz の 5 ターゲット (crates/crt/fuzz、fuzz/run.sh と Fuzz ワークフローが回す)、Kani
    (_s 関数を全入力のモデルと照合、memcpy_s/memmove_s、_itoa/_itoa_s)。
- 動画: Media Foundation は MFStartup が失敗する (動画を飛ばす)。DirectShow のグラフに足される
  EVR (CLSID_EnhancedVideoRenderer) は何もしないフィルタ。
- 確認: Artemis x64 の 3 本 (遥かなるニライカナイ、GALS FICTION、セレクトオブリージュ) が
  Chromium でタイトル → 本編。SakanaGL x64 (でか乳巫女) もタイトル (約 30 秒) → 本編。
  2026-11-06 に自前の x64 CRT (プレフィックスはフォントだけ) で 4 本とも本編まで再確認。
- SakanaGL の sakanagl.dll は /arch:AVX2。koji のインタプリタが VEX の 128 ビット形
  (AVX/AVX2)、FMA3、SSE3/SSSE3/SSE4.1 を実行する (interp/vex.rs: VEX のコードを名前で
  旧来のコードに写し、NDS は Cpu::vex_tmp (xmm16) に src1 を写してから実行)。
- 256 ビット (2026-11-07): ymm の上半分は Cpu::ymmh。VEX.128 は書いた先の上半分を 0 にする
  (インタプリタと 64 ビット JIT)。VEX.256 は interp/avx.rs: レーンごとの命令は VEX.128 の形を
  2 回 (2 回目は上半分を xmm に入れ替え、m256 は +16、レーンごとの即値は次のビット)、
  レーンをまたぐもの (broadcast、vperm*、vinsert/vextract128、vpmovsx/zx と cvt の拡大・縮小、
  movmsk、vptest) は直接。AVX2 の gather 全種、BMI1/BMI2 (interp/bmi.rs)、XGETBV (XCR0 = 7)。
  64 ビット JIT もレーンごとの VEX.256 (移動、算術、論理、per-lane のシャッフル・pack・unpack・
  ブレンド、即値シフト、同じ幅の cvt) を同じやり方で訳す (jit/long/sse.rs の sse256: 両レーンの
  MXCSR ガードを先に済ませ、途中でインタプリタに戻るときは入れ替えを戻す)。レーンをまたぐもの
  (vinsert/vextract128、vperm2f128/i128、vpermq/pd、vpermd/ps、broadcast 全種、vpermilps/pd の両形、
  vpblendd、可変シフト vpsllv/vpsrlv/vpsrav、YMM への vpmovzx/sx、YMM の movmsk、vzeroall) は
  jit/long/avx.rs が直接訳す (2026-11-13、#36: 結果を L_V0/L_V1 に作ってから書くので、行き先が
  元と同じでもよい)。FMA、gather、vptest、vmaskmov、cvt の拡大・縮小、xmm の個数のシフトはまだインタプリタ。確認は tools/jit64test (ymm の形もインタプリタと照合) と
  host/tests/avx256.c (ネイティブの x86-64 と同じチェックサム)。CPUID は既定では AVX 系を報告しない
  (CPUID で経路を選ぶ memcpy やコーデックがインタプリタの命令に入るため)。IPPAI_CPUID_AVX を付けると
  AVX/AVX2/FMA/BMI1/BMI2/LZCNT/OSXSAVE と leaf 7 を報告する (koji::CPUID_AVX)。
  確認は crates/koji/tests/diff_isa.rs: MMX〜SSE4.2、AVX、AVX2、FMA3、BMI、POPCNT、LZCNT の
  iced の Code 全部 (1078) を各 48 回 (IPPAI_ISA_CASES で増やせる)、NaN や非正規数を含む値で
  実 CPU と比べる (YMM 全体、MMX、汎用レジスタ、フラグ、MXCSR の例外フラグまで、メモリ)。これで見つかった
  既存のずれ: HADD/HSUB/ADDSUB と DPPS/DPPD の NaN と丸めモード、ROUND* が SNaN を quiet に
  しない、FMA の NaN の優先順 (式の順 x*y+z)、MXCSR の例外フラグ (FMA は一度だけ丸めた結果が
  正確かを整数で確かめる、0*inf+QNaN は IE なし。DPPS/DPPD は各積と和。VCMP の述語 8〜31)。DPPS/DPPD の NaN の中身はレーンごとに違う
  (Intel の内部の順序) ので NaN であることだけ見る。
- 64 ビットの JIT (koji jit/long.rs、long/sse.rs) は SSE〜SSE4.1 と VEX.128 の主な形を wasm SIMD
  に訳す (VEX はインタプリタと同じ表で旧来の形に写し、NDS は src1 を読むだけ)。CL 指定の
  シフト (32/64 ビット)、bsf/bsr、bt/bts/btr/btc も訳す (未定義のフラグはインタプリタと同じく
  そのまま)。SakanaGL の起動 (最初の描画) は 113 秒 → 8 秒。`tools/jit64test` は xmm も比べ、
  SSE/VEX の iced の Code 578 種をランダムに混ぜる。
  `tools/jit64test native` で実 CPU と照合済み (rcp/rsqrt の近似値を除き一致)。
  SSE4.1 の dpps/dppd、mpsadbw、phminposuw と SSE4.2 の pcmpXstrX も実行する
  (host/tests/sse4.c: native と一致、-mavx の VEX 形も)。
- 64 ビットの速さ (2026-11-07): x86 と同じ bench.exe が 40.5 秒 → 1.82 秒 (x86 は 1.0 秒)。
  - JIT (long.rs): リージョン内の直接 call は goto、ret はリージョン内の戻り先 (最大 8) と比べて
    goto。リージョンの出口はハッシュを引いて、同じリージョンならレジスタを local に置いたまま
    ループの先頭へ、別のリージョンなら return_call_indirect で直接つなぐ (jit_budget で打ち切り、
    ring に記録)。`cmp + ja` に続く `jmp` のジャンプテーブル (lea rip + movsxd + add、
    jmp [base+idx*8]) は表の行き先を全部ブロックにする。最大 256 ブロック / 1000 命令。
  - kura: x64 の merge_hot64 は一番出口の多いブロックに、そこからよく行く先をまとめて足す
    (ジャンプテーブルの基底が外にあってテーブルを読めないときの代わり)。
  - ネイティブ化した命令: adc/sbb、1 オペランドの mul/imul/div/idiv (0 除算・桁あふれは
    インタプリタ)、tzcnt/lzcnt/popcnt、rol/ror (即値)、bswap。
  - sake: x64 のゲスト CRT (guest/crt.c を -m64 -mabi=ms でもビルド、guest_crt64.bin) が
    EnterCriticalSection/LeaveCriticalSection、TlsGetValue、Get/SetLastError、HeapAlloc/HeapFree、
    malloc/free を trap なしで済ませる。WaitForMultipleObjects / MsgWaitForMultipleObjects(Ex) の
    ハンドル配列は x64 で 8 バイト刻み (これまで threads の結果が違っていた)。
  - テスト: tools/jit64test に新命令とリージョン連鎖 (1〜3 ブロックの小さいリージョンを全命令で
    先に作る) のケース、CI の jit64 ジョブが毎回別の seed で 2 万件。host/tests/fastpaths.c
    (CS 4 スレッド、TLS、ヒープ、errno など) は x86 と x64 で同じ期待値。
  - lock 付きの add/sub/and/or/xor/inc/dec/xadd/cmpxchg とメモリとの xchg は、4/8 バイト境界なら
    wasm の atomic 命令 1 つ (境界を外れるとインタプリタ)。sync_lock 202 → 35 ms (x86 29 ms)。
  - kura の jit::run が入るたびに環境変数を 2 つ読んでいた (std::env::var はロックを取って環境を
    なめる) のを 1 回だけに。strings は x86 も x64 も 3 割ほど速くなった。
  - 例外: RtlUnwindEx はホストで再帰しない (各フレームのハンドラは UNWIND_TRAP へ戻り、
    `unwind_go` が続ける)。ハンドラの中で投げた例外は、RtlUnwindEx の呼び出し元より上の
    フレームへ進む。MinGW の libgcc は後始末 (デストラクタ) のたびに unwind ハンドラから
    STATUS_GCC_UNWIND を投げるので、これまで x64 の g++ の例外は落ちていた
    (host/tests/gxxunwind.cpp: x86 と x64 で同じ結果)。
  - MMX (movq/movd/movntq/emms、レーンごとの演算、シフト、punpckl/h、pack) も x64 の JIT で訳す
    (MMX レジスタは v128 の下半分、jit/packed.rs と同じ)。SDL 1.2 の MMX ブリットが解釈器で
    動いていて、ONScripter x64 の花びら演出は 1 フレーム 159 → 17 ms。x86 の JIT にも movntq
    を足した (SDL_BlitCopy が解釈器だった): x86 版 65 → 16 ms (host/tests/mmxblit.c、実機と同じ値)。
  - フラグの記録 (koji::lazy::Lazy) は a/b/r が u64、サイズ 3 = 64 ビット。32 ビットも 64 ビットも
    同じ記録で、64 ビットの ADD/SUB/LOGIC/INC/DEC/MUL も出口で計算せず記録だけ残す
    (EFLAGS にするのはホスト)。64 ビットのシフトだけは出口で計算する。
  - 32 ビット幅の演算 (add/sub/and/or/xor/imul/シフト、32 ビット先の lea、符号付き比較) は
    i32 で出す (wrap → i32 演算 → extend_u)。i64 の `& 0xffffffff` や shl/shr_s の符号拡張は
    V8 が消してくれない。符号拡張は i64.extend32_s など。条件分岐はフラグを両方の行き先の前で
    Cpu へ書かず、読む行き先 (と出口) だけが書く。cpu.exe yuv 120 → 51 ms (x86 61 ms)、
    mix 458 → 342 ms、unfilter 610 → 526 ms。
  - 32 ビットの JIT にも同じ差分テスト `tools/jit32test` (CI の jit32、seed は run 番号): ランダムな
    整数・MMX・SSE・x87 のコードをインタプリタと、koji のディスパッチャ経由の JIT (リージョン 1/2/3
    ブロック/無制限、関数モード、範囲チェックの巻き上げとスタックスロットの有無) で走らせて比べ、
    違えば命令を削って縮める。最初に見つかったバグ: ループをまたぐ x87 キャッシュが、1 周ごとに
    TOP が動く (積むだけ・降ろすだけの) ループで空きビットを壊していた。TOP が動くループでは
    ループ全体のキャッシュを使わない。
    `jit32test native` は同じ生成器のプログラムをインタプリタとこのマシンの CPU (gcc -m32 の
    フリースタンディングな実行器 native32.S) で走らせて比べる (CI でも毎回 3000 件)。x87 の
    丸め (RC) と精度 (PC 53/24)、MXCSR.RC は毎回ランダム。未定義フラグ (div/bsf/bt/mul/imul、
    2 ビット以上のシフトの OF) は直後の命令で定義し直す。ここで見つかって直したもの: x87 と
    SSE が丸めモードを無視していた (koji::round で全演算を正しく丸める。x87 の JIT は通常の
    演算のあと、ブロックごとに 1 回読んだモードを見て、最近接以外ならインラインで誤差を出して
    丸め直す。import 呼び出しは V8 が周りの値を退避して transform が 2 倍遅くなったのでやめ、
    命令ごとのモード読み出しも 30% 遅かったのでブロックごとにした。SSE は MXCSR.RC が最近接の
    ときだけ JIT で走らせ、それ以外はインタプリタ)、PC24 の x87 を f32 の指数範囲で丸めていた、x87 の非正規化数になる
    積・商の二重丸め (レジスタで 53 ビット、ストアで f64)。
  - x87 の 80 ビット拡張精度 (2026-11-14、#52): 既定の x87 は f64 で計算する (速さのため。PC64 も 53 ビット)。
    IPPAI_X87_80=1 を付けると koji::f80 (SoftFloat の roundPackToExtF80 と同じ丸め、x86 の NaN 規則、
    後丸めの tininess) で 64 ビット仮数・15 ビット指数のまま計算し、PC 24/53/64 と 4 つの RC を守る
    (interp/x87.rs の x87_precise。ロード・ストア・四則と整数オペランド・fsqrt・frndint・比較・fcmov・
    fsave/frstor・fxsave/fxrstor。超越関数などは f64 のまま)。このモードでは x87 の JIT を使わない。
    確認は TestFloat 3e (crates/koji/tests/testfloat_f80.rs、extF80 の四則・sqrt・変換 240 万件と
    命令経由 26 万件)、実機の x87 との差分 (tests/diff_x87_80.rs、ランダムな命令列 4 万件)、
    host/tests/x87ext.c (ネイティブと同じチェックサム)。crt_math の known.txt の差は x87 ではなく
    sake の libm と msvcrt の libm の違いなので、このモードでは変わらない。
  - リージョン間の移動 (2026-11-07、Godot 4 の Forgotten Village の起動): merge_hot64 は「ring を
    8 回続けて一番多く出ていく」で統合していた。ring は 64 回の移動ごと (次の miss や API 呼び出しで)
    読まれるので、起動中のコードは数千回の移動で統合され、タイトルまでに約 1300 回、大きくなった
    リージョンを丸ごと V8 にコンパイルし直していた (しかも直後は Liftoff からやり直し)。いまは ring に
    占める割合から推定した移動回数が 2^18 (IPPAI_JIT_MERGE_HOPS) に達したら統合する: タイトルまで
    約 70 秒 → 38〜45 秒 (4 回。統合しなければ 40 秒だが、bench.exe の sort が 2 倍、cxx の virtual が 3 倍遅く
    なる)。リージョンは自分のコードが読み書きするレジスタだけを Cpu と出し入れする (入口で読むのは
    名前を出すもの、出口で書くのは書き換えるもの。ほかは Cpu にあるまま): 1024 個の関数を呼び分ける
    ベンチで 1〜2 割速い。
    調べて捨てたもの: 移動が遅いのは行き先が別の wasm モジュールだから (リージョンごとに 1 モジュール;
    perf では行き先の先頭命令にサンプルが集まる。128 個までなら 1 回 25 ns、1000 個を超えると 100 ns
    超)。熱いリージョンを 1 モジュールにまとめ直すとベンチは 2 倍速いが、このゲームでは再コンパイルが
    移動の損 (メインスレッドで約 3 秒) より高くつき、タイトルが 85〜100 秒になった。
    テスト: host/tests/hops.c (64 個ずつ 4 種類の関数を表から呼ぶ。x86/x64/インタプリタ/統合を最初の
    移動でする設定 (IPPAI_JIT_MERGE_HOPS=1 IPPAI_JIT_RECLAIM=1) で同じ結果)、fuzz の x86_jit64
    (1〜4 ブロックのリージョンをモジュール 1 つずつ、移動だらけで解釈器と比べる。全レジスタを出し入れする
    DEBUG_ALL_REGS とも)、long.rs の splice_regs の PBT、jit64test (JALLREGS で全レジスタ)。
    「書くレジスタしか読み込まない」壊し方は jit64test と x86_jit64 の両方が見つける。
    自己書き換え (#80): 書き換えられうるブロック (jit::set_watch、exe の W|X ページ) では、メモリに書く
    命令 (iced の used_memory の書き込み) の後で、書いた先頭と末尾のバイトのページに翻訳済みコードが
    あるか (Cpu::jit_code_pages) を見て、あれば Cpu::jit_smc を立てて次の命令でリージョンを抜ける
    (kura の smc_hit が翻訳を捨て、そのページをインタプリタに回す)。インタプリタに回した命令の書き込みは
    従来どおり interp_write。call の push は見ない (packed.rs と同じ)。テスト: jit64test の
    self_modifying_code_in_a_region (見張りなしだと古い翻訳を実行することも確かめる)、乱数プログラムに
    自分のコードへの `add byte [rip+..]` を混ぜる (見張りの有無を 2 件ずつ交互)、x86_jit64 は全ストアを
    見張って比べる (regressions/x86_jit64/add-rip-rewrites-next)。
  - 残り: script 1.5 倍、C++ の std::map 2〜4 倍、x87 と minss の JIT。
- gl.rs: x64 では引数を位置で並べる ('d'/'f' は xmm、他は a64 の下位)。glShaderSource の
  ポインタ配列は 8 バイト刻み。拡張関数名は ARB/EXT を外した名前でも探す。
- user: DPI API (GetDpiForWindow など 96 固定)、ShowWindow(SW_MAXIMIZE) で画面いっぱいに広げる。
- 構造体: PAINTSTRUCT、BITMAP、DIBSECTION、SYSTEM_INFO、MEMORY_BASIC_INFORMATION は x64 の
  形で書く (host/tests/struct64.c)。
- XAudio2 2.7 (xaudio2.rs): 参照カウントはポインタの後、XAUDIO2_BUFFER / VOICE_SENDS /
  VOICE_STATE はポインタ幅の配置、SetVolume / SetFrequencyRatio の float は xmm から
  (host/tests/xaudio2.c: x86 と x64 で同じ行)。
- d3d9 (d3d9.rs): vtable は 8 バイト、引数は 8 バイトの枠で kura に渡す (kura は下位半分を読む)。
  SetNPatchMode の float は xmm1 から。D3DLOCKED_RECT の pBits は 8 バイト目。winref の d3d9
  プローブ (`BROWSER64=1 SYSTEM64=<x64 CRT> node tests/winref/run-browser.mjs`) は x64 でも
  32 ビットと同じく既知の差だけ。

## どこでもセーブ (2026-11-07)

実行中のプログラムをそのまま保存・復元する（ゲーム右上のボタン）。全スレッドを安全な位置で止めてメモリ全体を zstd で保存し、GPU の中身はメモリに読み戻して再構築する。ブラウザ（OPFS）と ippai serve（S3）に保存されるので別の端末からも続けられる。詳細は docs/savestate.md。

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
  `play.html?engine=sake&autostart&run=Z%3A%5Cvnvm%5Cgame%5C<exe>&mount=/ippai/game=<name>`。
  `tests/play.mjs` でヘッドレスのスクリーンショット。
- 調べるとき: `SAKE_WATCH=<16進番地>` (API 呼び出しごとにその番地の値を見て、変わったら表示)、`IPPAI_D3D_TRACE=1` (描画ごとの頂点も表示)、`SAKE_TRACE=1` (+ `SAKE_TRACE_FILTER=a,b`、`SAKE_TRACE_AFTER=ms`)、
  `SAKE_TRACE=3` (落ちたときにスレッドごとの直前の呼び出し)、`SAKE_API_STATS=秒`、
  `SAKE_TRACE_FAULT`、`SAKE_TRACE_FILES`、`SAKE_TRACE_LOAD`、`SAKE_TRACE_MSG`、
  `SAKE_TRACE_INPUT`、`SAKE_TRACE_FONT`、`IPPAI_JIT_STATS` (JIT が実行ループに戻る場所)、
  `IPPAI_D3D_SPV=<dir>` (D3D11 シェーダの SPIR-V を書き出す)。
- 確認用の小さなテスト: `ENGINE=sake node tests/gfx.mjs` (d3d11test、d2dtest、ddrawtest が通る。
  d3d8test は sake に D3D8 が無いので落ちる)。
