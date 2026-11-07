# TODO（既知の問題）

ippai / sake / koji / kura で分かっている問題と、本物の Windows との差分の一覧。
直したら該当する項目（と下の `known` ブロックの行）を消すこと。

- 「1. 未対応・バグ」は機能の穴と不具合。詳しい経過は GitHub の Issue にある。
- 「2. Windows との既知の差分 (winref)」は `tests/winref` のプローブで出る差分のうち、原因が分かっているもの。
  旧 `tests/winref/known.txt` はこの節に統合して廃止した。

## 1. 未対応・バグ

### 互換性（ゲーム・エンジン）

- [ ] Enigma Protector 付きの exe が 'Internal Protection Error' で止まる（Crowd / Anim）(#21)
- [ ] Softpal: 爆乳遊廓のタイトルでメニューのボタンが描かれない (#22)
- [ ] BGI: クダンノフォークロアのタイトルでクリックに反応しない (#23)
- [ ] RealLive: 3Ping Lovers! で「はじめから」の後に Enter が要り、確認の表示が見えない (#24)
- [ ] Unity 製の体験版が落ちる（VenusBlood、Icing）(#47)
- [ ] 起動後に終了する・黒画面になる体験版（神楽黎明記、CatSystem2 kokonatsu）(#48)
- [ ] ShiinaRio: インストール確認で起動を拒まれる (#49)
- [ ] ソフト電池・PlayDRM の付いたタイトルは扱わない（Wine 版 vnbox 側で動かす）

### Win32 API・ランタイム

- [ ] CRT: printf の位置指定引数（`%1$d`）(#43)
- [ ] CRT: x64 の `_create_locale`（x86 の構造体配置のまま）、`_set_se_translator`
- [ ] msvcp140: x64 で Artemis が使う分だけ。それ以外の関数は呼ぶと止まる。x86 の msvcp は thiscall の橋渡しが要る (#30)
- [ ] XAudio2 2.8 / 2.9（`xaudio2_9.dll` の `XAudio2Create`）(#27)
- [ ] D3D11 のコンピュートシェーダー（cs_5_0 の D3DCompile と Dispatch）(#50)
- [ ] D3D10: シェーダーのリフレクションと Effects 10
- [ ] icu.dll（Windows 10 の ICU: `ubrk_*` の行・文字の区切り）(#51)
- [ ] Win16: 16 ビット DLL、WIN87EM、MMSYSTEM の wave、NE リソースの LoadBitmap (#37)
- [ ] wmvcore は E_NOTIMPL のスタブ（KiriKiri Z の krmovie は読み込めるが WMV は再生しない）

### 動画・音声

- [ ] Media Foundation / DirectShow の動画再生 (#31)。いまは MFStartup が失敗して動画を飛ばし、DirectShow の EVR は何もしないフィルタ
- [ ] 変換後の .vnv を小さくし、VP9 にも対応する (#32)
- [ ] 動画の音声もブラウザ側でデコードする (#33)

### CPU（koji）

- [ ] AVX の 256 ビット（ymm）と未実装の SIMD 命令 (#36)
- [ ] x87 が IEEE double で計算する（80 ビット拡張精度と精度制御フィールドを無視）。下の winref 差分「x87 の精度」の原因

### グラフィックス（kura）

- [ ] D3D9: A8 / L8 / A8L8 / A4R4G4B4 / A2R10G10B10 / R32F / V8U8 のレンダーターゲット
- [ ] D3D11: MSAA（品質レベル 0 を返してゲームに諦めさせている）、動画・パレット形式
- [ ] GDI: ハッチブラシ（塗りつぶしで描く）、楕円・角丸矩形・多角形の塗り、太い/破線ペン（1 ピクセルの実線で描く）、AlphaBlend の丸め、StretchBlt/StretchDIBits の拡縮・反転の規則、TransparentBlt の拡縮、ROP3 の null ブラシ（`tests/diff/gdi-expected.txt`）
- [ ] WIC: CMYK の色変換（Windows の色プロファイル）、Cubic / Fant スケーラーの重み、16bppGray のエンコード
- [ ] Direct2D: アンチエイリアスの縁の被覆率、エイリアスの楕円の輪郭

### テスト・記録再生

- [ ] replay: 64 ビットのトレース、D3D8、Direct3D 11 (docs/replay.md)
- [ ] Android 実機での確認（sake-grow 版を含む）
- [ ] Firefox / Safari / iOS、DOS / ARM64 の exe は対象外（README の対応状況）

### その他

- [ ] SDRT (#7: 内容未整理)
- [ ] x86-64: 4GiB より上を指定して確保する、再配置できない高位の DLL が複数ある、といったプログラムは動かない（docs/sake.md）

## 2. Windows との既知の差分 (winref)

`tests/winref/compare.py` は `--known TODO.md` で、この節の ```` ```known ```` ブロックだけを読む。
1 行が 1 規則で、`<プローブ名の glob> <差分行への正規表現>`（`  #` 以降はコメント）。
差分行は `want <Windows の行>` / `got  <sake の行>` / `image <名前>: ...` の形。
規則に合う差分は失敗にならず、レポートに known として出る。

各項目の「区分」:

- **ランナー由来**: GitHub の Windows ランナー（en-US、Windows Server 2025、WARP、GDI Generic GL）の事情で、ゲームの動く環境とは違う。直さない。
- **仕様上の違い**: 実装に任された部分や、sake が意図して変えている部分。直さない。
- **sake の TODO**: sake 側を Windows に合わせるべきもの。直したら消す。

### ロケールと OS のバージョン（ランナー由来）

ランナーは en-US（LCID 409、コンソール UTF-8）で、Windows Server 2025 はマニフェストの無い exe に 6.2.9200 を返す。sake は日本語の Windows 7 として振る舞う。

```known
env-* ^(want|got) +(GetUserDefaultLCID|GetUserDefaultUILanguage|GetConsoleCP|GetVersionEx|GetSystemDefaultLangID|GetUserDefaultLangID|GetSystemDefaultUILanguage)  # runner locale/version
```

### シフトの未定義フラグ（仕様上の違い）

2 以上のシフトと SHLD/SHRD の OF/AF は Intel が CPU に任せている。

```known
host_intops-32 ^(want|got) +(shifts|dshift)
```

### 失敗した fseek の後の ftell（sake の TODO、優先度低）

msvcrt のバッファ付き ftell は、失敗した fseek の後にバッファの位置を返す。これに頼るゲームは無い。

```known
host_fileseek-32 ^(want|got) +(negative|origin)
```

### GDI の図形（sake の TODO）

形は合うがピクセル単位で合わない。Windows は楕円とベジェを独自の平坦化でパスにし、角丸矩形・多角形の領域の縁を独自に丸め、HALFTONE の縮小フィルタも独自。クリップ領域にした領域は外接矩形として持っている。

```known
gdi-* ^image (shapes|fills_nullpen|polyline_bezier|regions|pens|stretch_halftone|clip):
gdi-* ^(want|got) +GetClipBox
```

32 bpp ビットマップの 8 / 1 bpp の GetDIBits: Windows の既定のカラーテーブルへの最近色の選び方が違う。

```known
gdi-* ^(want|got) +GetDIBits (8|1)
```

### printf の `%c` で 0x80 以上の 1 バイト（仕様上の違い）

sake のコンソールは UTF-8 のテキスト（node の stderr）として出るので、0x80 以上の単独のバイトは通らない。

```known
crt_printf-* ^(want|got) +%c (-1|-42|255|2147483647)
```

### 数学関数（32 ビット: sake の TODO / 64 ビット: 仕様上の違い）

32 ビット: x87 の拡張精度。msvcrt の sin/cos/tan は大きな引数を独自に縮約し（sin(pi) が 3.2e-15）、pow / _hypot は内部で 64 ビット仮数を保つ。koji の x87 は IEEE double。

```known
crt_math-32 ^(want|got) +ulp 1 (sin|cos|tan)\((3.14159265358979|1.0000000000000001e\+300|10000000000)\)
crt_math-32 ^(want|got) +ulp 1 (pow\(100.5,3.14159265358979\)|_hypot\(2,100.5\))
```

64 ビット: msvcrt 独自の SSE2 libm は pow が正しい丸めから数 ulp ずれ、sin/cos は大きな引数を独自に縮約する（sin(pi) は 20 ビットしか残らない）。sake の x64 CRT は Rust のもの（正しい丸めか 1 ulp 以内）。

```known
crt_math-64 ^(want|got) +ulp 1 (sin|cos|tan)\((3.14159265358979|1.0000000000000001e\+300|10000000000)\)
crt_math-64 ^(want|got) +ulp 1 pow\(
```

### x87 の精度制御（sake の TODO: koji）

koji の x87 は精度フィールドに関係なく double で計算する（制御ワード自体は Windows と同じ値を読み返す）。単精度（`_PC_24`、または `D3DCREATE_FPU_PRESERVE` 無しの CreateDevice）では Windows は 1/3 を float に丸める。

```known
crt_math-32 ^(want|got) +pc 0: 1/3
d3d8-32 ^(want|got) +fpu default: 1/3
```

### Direct3D 9（ランナー由来 + sake の TODO）

ランナーの D3D9 は WARP で、上限と形式の一覧が独自（GPU ごとにも違う）。

```known
d3d9-* ^(want|got) +caps vs .* maxtex
d3d9-* ^(want|got) +caps blend src
```

sake の TODO: A8 / L8 / A8L8 / A4R4G4B4 / A2R10G10B10 / R32F / V8U8 のレンダーターゲットが無い（ページ上ではサンプリング結果としてのテクセルで持ち、読み戻しはその形式で返す）。

```known
d3d9-* ^(want|got) +format (A4R4G4B4|A8|L8|A8L8|A2R10G10B10|R32F|V8U8) texture 1 rendertarget
```

`D3DTEXF_NONE` の StretchRect: WARP は線形補間、GPU のドライバは最近傍（sake も最近傍）。

```known
d3d9-* ^image surfaces:
```

d3d9 プローブのトレースの再生（tests/replay）: 絵はプローブの順に r<呼び出し番号>。sampling (r307) と address2 (r443) はプローブ側では WARP の 1/16 フィルタ重みのため許容 10、再生側は 2。r1023 は上の surfaces と同じで、Present 時のバックバッファ (f0) も同じ絵。

```known
replay_d3d9-32-32 ^image (r307|r443|r1023|f0):
```

### Direct3D 11（ランナー由来 + 仕様上の違い + sake の TODO）

ランナーの D3D11 は WARP で、形式の一覧とフラグが独自。sake は MSAA が無く（品質レベル 0 でゲームはフォールバックする: TODO）、動画・パレット形式（100〜115）も無い（動画非対応のデバイスと同じく E_FAIL）。BC6H / BC7、3 チャンネル 32 ビット、4:2:2 パック、XR_BIAS はアップロード時にデコードまたは拡張する。

```known
d3d11-* ^(want|got) +format
```

WARP は浮動小数の非正規化数を毎回フラッシュする（abs(非正規化数) = 0、clamp で -0）。WebGPU は GPU に任せ、sake は log 以外でフラッシュしない。1 のすぐ下の log2 は仕様の 2^-21 の絶対誤差内で、プローブの ulp 指定の外。

```known
d3d11-* ^(want|got) +f0\.abs [08]0[0-7][0-9a-f]{5}
d3d11-* ^(want|got) +f0\.clamp \S+ \S+ [08]0[0-7][0-9a-f]{5}
d3d11-* ^(want|got) +ulp \d+ f0\.log2?:3f7fffff:
```

線とワイヤーフレーム: WebGPU は線のラスタライズ（Bresenham かダイヤモンド出口か）を GPU に任せる。

```known
d3d11-* ^image (lines|wireframe):
```

線形フィルタでの BORDER（シェーダーで境界色を混ぜる、dxwgsl::addr）: WARP はテクセルを 1/16 単位で重み付けし、GPU はもっと細かい。差は最大 1 レベル。

```known
d3d11-* ^image address: \d+ pixels off by more than 0 \(worst 1;
```

### WIC・GDI+ の画像（sake の TODO）

CMYK → sRGB は Windows では色プロファイルを通る（sake は測った 9 段の格子を補間: 3 レベル以内。sRGB → CMYK の向きはモデル化していない）。16bppGray のエンコードは 65535 から 1 以内、128 色インデックスのうち 4 色が違う（Windows 自身の最近色探索の同点）、Cubic / Fant スケーラーの重みが独自（最近傍と線形は完全一致）。

```known
imaging-* ^image (gdip_)?(jpeg_cmyk|png_gray16):
imaging-* ^(want|got) +gdip jpeg_cmyk: pixel
imaging-* ^image scale_[23]_
wicconv-* ^image cmyk_grid:
wicconv-* ^(want|got) +32bppCMYK>32bppBGRA:
wicconv-* ^(want|got) +32bppBGRA>(16bppGray|32bppCMYK|[248]bppIndexed):
wicconv-* ^(want|got) +w [23]
```

### Direct2D（sake の TODO）

WARP 上で、アンチエイリアスの縁は Windows 独自の被覆率（sake は 4x4 サンプル）、エイリアスの楕円の輪郭はテッセレーションした線（sake は輪）、アンチエイリアスのクリップはエイリアスの塗りの縁もぼかす（sake はピクセル中心で切る）。

```known
d2d-* ^image (fills_aa|outlines|transforms):
```

### OpenGL（ランナー由来）

ランナーの OpenGL は GPU ドライバではなく Microsoft の GDI Generic（ソフトウェアの GL 1.1）。ピクセル形式にアルファビットが無い、辺の上に中心があるピクセルを両方の三角形で塗る（sake は GPU の top-left 規則）、線形フィルタの GL_CLAMP は境界色を混ぜる（NVIDIA のドライバと sake は clamp-to-edge）、色の丸めが数レベル違う（formats / blend_depth は 4 以内）。

```known
gl-* ^(want|got) +pixel format:
gl-* ^image (raster|textures):
```

GDI Generic の演算は独自: RGB(A) テクスチャの GL_REPLACE の色は trunc(c * 255 / 256)、8 ビットより多い成分は 8 ビットに切り捨て（sake と GPU ドライバは最近接丸め: GL の仕様は実装任せ）。ゲームが動くドライバは GL 2 以上なのに GL 1.1 の規則のまま: 2 の冪でないサイズ不可、内部形式 5 で INVALID_ENUM、未定義レベルへの TexSubImage2D で INVALID_VALUE、要求した内部形式をそのまま読み返す（ドライバは実際の GL_RGBA8）。sake の unpack は crates/kura/src/gl で単独にテストしている。

```known
gltex-* ^(want|got) +tex (u?byte|u?short|u?int|float)
gltex-* ^(want|got) +(unpack|sub) \S+ rgb
gltex-* ^(want|got) +(TexImage2D width 3|TexImage2D internal 5|TexSubImage2D undefined level):
gltex-* ^(want|got) +level 0 after the errors 4 x 4 internal
```

### DirectDraw / Direct3D 7 の参照カウント（仕様上の違い）

Windows の Direct3D 7 デバイスは IDirect3D7 の参照を自分で複数取る（CreateDevice の後に合計 9）。sake のデバイスは 1 つだけ持ち、Windows と同じくデバイスと一緒に手放す。

```known
ddrefs-* ^(want|got) +refs d3d7 after CreateDevice
```

### ウィンドウマネージャの細部（ランナー由来 + 仕様上の違い）

ゲームからは見えない違い: クラスのアトムはセッションのアトム表から振られる（ランナーでは 0xC149..）、DWM が作成時に WM_DWMNCRENDERINGCHANGED (0x31f) を送る、ShowWindow の「表示されていた」の答えは文書化されていない非 0（24）、NCPAINT / ERASEBKGND はランナーでは ShowWindow の中で来るが sake は次のメッセージループで描く。

```known
user-* ^(want|got) +GetClassInfoA\(wc.hInstance, "ippai-winref", &got\)
user-* ^(want|got) +start:
user-* ^want +WM_(NCPAINT|ERASEBKGND)$
user-* ^(want|got) +ShowWindow\(h, SW_HIDE\)
```

### シェルのパス関数（ランナー由来）

ランナーの ANSI コードページは 1252 で、0x95 0x5c はそこでは 2 文字、sake の 932 では 1 文字（表）。

```known
shell-* ^(want|got) +(name|removespec|strip) .*(2e747874\]|955c|835c\]|835c5c95\])
```

### メニューのリソース（仕様上の違い）

ポップアップの wID はその HMENU で、ハンドルの番号が違う。

```known
res-* ^(want|got) +item 0: 1 id \d+ type 0 state 0 sub 1 text \[&File\] cch 5$  # a popup's wID is its HMENU, whose number differs
```

### DirectWrite のフォント（仕様上の違い）

sake の MS 名のフォントはその名前を付けた IPA フォント（tools/ci/sake-fonts.py）で、ゲームのストアの別々の .ttf か `ippai ttc` の msgothic.ttc 1 つ。デザインのメトリクスが違い、ストアに .ttf があるときはファイル、その種類、フェイス数も違う。

```known
dwfont-* ^(want|got) +(metrics em|face 00000000 type|local loader|analyze)
```

### DirectShow の VMR（ランナー由来）

ランナーの VMR-9（Direct3D 9）はウィンドウレスモードで接続しない（セッションにデバイスが無い）。RenderFile は別のレンダラーに行くので、VMR-9 のサイズと位置は空のままでグラフ自身のビデオウィンドウが答える。VMR-7 の行が基準。

```known
dshow-* ^(want|got) +vmr9: (GetNativeVideoSize 0|GetVideoPosition|SetVideoPosition src [08]|get_Visible|GetVideoSize|GetCurrentImage|RepaintVideo)
```

画面サイズ + 1: ランナーは 1024x768、sake は 1366x768。

```known
dshow-* ^(want|got) +vmr[79]: GetMaxIdealVideoSize 00000000 (1025|1367)x769$
```

## 3. ほかの差分リスト

機械が読む期待値の一覧はそれぞれのテストの隣にある（この TODO には載せない）。

- `tests/diff/gdi-expected.txt`: GDI のランダム描画の Wine との差分（上の「グラフィックス」の GDI 項目）
- `tests/diff/fpu-expected.tsv`: 浮動小数点の差分テスト
- `tests/winetest/expected-*.tsv`: Wine のテストの期待結果
- `tests/sst/expected.tsv`: SingleStepTests
- `tools/ci/mutants-known.txt`: 生き残っても問題の無い変異
