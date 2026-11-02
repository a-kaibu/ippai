# sake: Rust で一から書いた Win32 層 (旧名 vnwin)

Wine を使わずに、ゲームの x86 コードを vnhost の JIT で動かし、Windows API を Rust
(crates/sake、wasm) で実装する。Wine 経路 (hangover/) と同じページ・同じ JIT・同じ
WebGPU D3D9 を使うので、A/B 比較ができる。`wine.html` / `game.html` に `&engine=sake`。

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
  d3d9 (vnhost の WebGPU D3D9 に橋渡し)、CryptoAPI のハッシュ、DxDiag、レジストリ、
  shell32/shlwapi の一部、Uniscribe の一部 (usp.rs)、DirectShow のフィルタグラフ (dshow.rs: 動画は
  すぐ終わったことにする)。
- DirectDraw: サーフェスはゲストメモリの画素と、その上に被せた GDI ビットマップ。Blt/BltFast
  (カラーキー、拡大縮小、塗りつぶし)、フリップ、Lock、GetDC。プライマリに描いたとき・
  Unlock・ReleaseDC・Flip でウィンドウに BitBlt する。Direct3D は出さないので、
  ゲームはソフトウェア描画に回る。
- D3D11 / DXGI (d3d11.rs): vnhost の WebGPU D3D11 (Wine 経路と同じ実装) に橋渡し。vtable の
  各メソッドはトラップで、`vnhost::d3d11::call` に引数を渡すだけ。メソッド表は
  `hangover/gen-d3d11-stubs.py` が d3d11_methods.rs として生成する。
- specs.txt にない DLL (d2d1、dwrite、d3dcompiler_47 など) はプレフィックスの
  `C:\windows\system32` から Wine の PE ビルドを x86 として読む。Direct2D はこの Wine の d2d1 が
  D3D11 の上で描く。
- Wine の unix 側呼び出し (unixlib.rs): `__wine_unix_call_dispatcher` と
  `NtQueryVirtualMemory(MemoryWineUnixFuncs)` を受けて、dwrite のフォント処理 (アウトライン、
  ビットマップ、メトリクス) を ttf-parser と ab_glyph_rasterizer で返す。フォント一覧はレジストリの
  Fonts キー、無い書体は Wine の Fonts\Replacements で MS ゴシック系に置き換える。
- WH_CBT フック (HCBT_CREATEWND): MFC はこれでウィンドウを結び付けるので、無いと LiveMaker が
  ウィンドウを作れずに終わる。

## A/B (Chromium、Reunion、KiriKiri)

| | Wine | sake |
|---|---|---|
| タイトルまで (game.html) | 10.1 s | 8.0 s |
| 本編の CPU | 0.26 コア | 0.24 コア |

数字は 868d37e のときのもの。差が出た主な理由は、JIT が GUEST_LO (0x10000) の上 32 KB に
触るコードをインタープリタに回していたこと (sake は 0x30000 より下を使わない) と、
トラップを x86 スタブに置き換えたこと。

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
  `VNVM_SCREENSHOT=out.ppm`、`VNVM_INPUT="秒:種類,x,y,データ;..."` (1 移動、2/3 ボタン、
  5/6 キー)。ボタンはホバーの移動を先に送らないと反応しないゲームが多い。
- ページ: `vnvm-pack tree <dir> <name>` してから
  `wine.html?engine=sake&autostart&run=Z%3A%5Cvnvm%5Cgame%5C<exe>&mount=/vnvm/game=<name>`。
  `tests/wine.mjs` でヘッドレスのスクリーンショット。
- 調べるとき: `SAKE_WATCH=<16進番地>` (API 呼び出しごとにその番地の値を見て、変わったら表示)、`VNVM_D3D_TRACE=1` (描画ごとの頂点も表示)、`SAKE_TRACE=1` (+ `SAKE_TRACE_FILTER=a,b`、`SAKE_TRACE_AFTER=ms`)、
  `SAKE_TRACE=3` (落ちたときにスレッドごとの直前の呼び出し)、`SAKE_API_STATS=秒`、
  `SAKE_TRACE_FAULT`、`SAKE_TRACE_FILES`、`SAKE_TRACE_LOAD`、`SAKE_TRACE_MSG`、
  `SAKE_TRACE_INPUT`、`SAKE_TRACE_FONT`、`VNVM_JIT_STATS` (JIT が実行ループに戻る場所)、
  `SAKE_WINEDEBUG=<flags>[:chan,...]` (Wine の PE DLL のデバッグ出力。1 FIXME、2 ERR、4 WARN、8 TRACE)、
  `VNVM_D3D_SPV=<dir>` (D3D11 シェーダの SPIR-V を書き出す)。
- 確認用の小さなテスト: `ENGINE=sake node tests/gfx.mjs` (d3d11test、d2dtest、ddrawtest が通る。
  d3d8test は sake に D3D8 が無いので落ちる)。
