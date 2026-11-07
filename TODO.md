# TODO（既知の問題）

ippai / sake / koji / kura で分かっている問題と、本物の Windows との差分の一覧。
直したら該当する項目を消すこと（winref の差分なら tests/winref/known.txt の行も）。

- 「1. 未対応・バグ」は機能の穴と不具合。詳しい経過は GitHub の Issue にある。
- 本物の Windows との細かい差分（テストが許容しているもの）は「2. 差分リスト」の各ファイルに、理由をコメントで書いてある。

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
- [ ] x87 が IEEE double で計算する（80 ビット拡張精度と精度制御フィールドを無視）（winref の crt_math-32 / d3d8-32 の差分の原因）

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

## 2. 差分リスト

テストが読む既知の差分の一覧はそれぞれのテストの隣にある（この TODO には載せない）。

- `tests/winref/known.txt`: sake と本物の Windows の差分（winref）。原因が分かっていて失敗扱いにしないもの
- `tests/diff/gdi-expected.txt`: GDI のランダム描画の Wine との差分（上の「グラフィックス」の GDI 項目）
- `tests/diff/fpu-expected.tsv`: 浮動小数点の差分テスト
- `tests/winetest/expected-*.tsv`: Wine のテストの期待結果
- `tests/sst/expected.tsv`: SingleStepTests
- `tools/ci/mutants-known.txt`: 生き残っても問題の無い変異
