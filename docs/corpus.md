# ゲームコーパス検査 (vnbox で試したタイトルを vnvm で動かす)

2026-11-01〜02。vnbox で動作確認済みの体験版から、エンジン・描画 API・年代が散らばるように
240 タイトルを選び、vnvm (Hangover 方式、Chromium 上) でタイトル画面まで到達するかを調べた。
DRM 付き (PlayDRM・ソフト電池) は対象外。x64 専用の 85 本は別リスト
(`/mnt/project-files/vnvm-corpus/x64-only.tsv`)。

## やり方

- `tests/corpus-prep.sh`: 体験版をダウンロード → unar (cp932) で展開 → vnvm-pack 形式のストアにパック。
  起動用の自動クリック (`hangover/tests/autoclick.c`、`_vnvm_autoclick.exe`) を同梱する。
- `tests/corpus-run.py`: ダウンロードを並列に進めつつ 1 本ずつ vnvm-server + Chromium (`tests/corpus.mjs`) で実行。
  ゲームは autoclick 経由で起動 (`VNVM_ROOT_APP=0`、`VNVM_HELPER_EXE=_vnvm_autoclick.exe`)。
  autoclick はランチャーの「ゲーム開始」「OK」などを押し、ダイアログの文言を記録する。
- タイトル判定: 画面が 300 色以上になり、その後静止したらタイトル。続けて一定間隔でクリックし、本編の CPU 使用率
  (コア数) を測る。
- 1 周目のあと、失敗した 118 本を当日の修正を入れたビルドで再実行した。下の表は最新の結果。

## 結果

240 titles run: title 143, no title 83, download/extract failed 8, x64 (skipped) 6

年代別 (ダウンロード失敗・x64 を除く): 2000 年代 48/58、2010 年代 62/113、2020 年代 32/53。

描画 API 別 (exe の import から。動的ロードは実行時に D3D を使ったかで判定):
Direct3D 9 65/92、DirectDraw 6/6、Direct3D 8 4/4、OpenGL 4/4、Direct3D 11 0/8、GDI ほか 64/112。
Direct3D 11 は全滅 (d3d11.dll の vnhost 実装は未検証の段階)。

## コーパスで見つけて直したもの

| 症状 | 原因と修正 | コミット |
|---|---|---|
| msacm32 の DllMain がクラッシュ | PROT_NONE の再マップでファイル内容が残っていた。ゼロ埋めする | (初期) |
| 古い exe が落ちる | pushaw/popaw がインタプリタに無かった | (初期) |
| ゲームがデータを見つけられない | カレントディレクトリを exe の場所に (VNVM_CWD) | (初期) |
| 終了時 abort | wrap.rs の TLS デストラクタ | (初期) |
| D3D9 ゲームが起動を拒否 | caps (非 2 冪テクスチャ)、アダプタのモード一覧、モニタハンドル | (初期) |
| AdvHD が画面サイズ不足 | 画面を 1366x768 に | (初期) |
| Siglus「日本でしか動きません」 | Wine が UTC+9 を「韓国」と命名。ブラウザのタイムゾーンを TZ/TZDIR で渡す | 92b2cbf |
| 負荷時に「グラフィックドライバがない」 | explorer の起動待ちを 60 秒に | 3b049f5 |
| Artemis が ConcRT 例外で abort | wasm 版 Wine に CPU トポロジ (GetLogicalProcessorInformation) を実装 | 836f149 |
| Softpal (13 本以上) が黒画面のまま | 隠しウィンドウの裏で MsgWait(bWaitAll)。トップレベル作成時に WM_NULL を投げる | d3b2b5f |
| BGI がハング | GetRasterStatus が常に走査線 0。60Hz の時計から計算する | 390cde9 |
| GlobalMemoryStatus が空き 0・負荷 100% | wasm 版 Wine にメモリ統計が無かった。4GB / 空き 3GB と報告 | 390cde9 |
| QPC×1000 が桁あふれ、GetTickCount64 が 1.8e12 | emscripten の CLOCK_MONOTONIC がワーカーで Unix 紀元起点。起動 1 時間後に見えるよう基準を引く | 486f0d5 |
| QLIE (かみぱに！) が JIT 有効時だけ落ちる | VirtualProtect でコードを書き換えるのに JIT が古い翻訳を実行。JIT スレッドが無効化を実装 | 39274ab |
| Artemis (フユキス) が Aborted() | 最初の Present 前に全画像を読み込み、D3D9 バッチが 1GB に。64MB で区切る | 6ce41d5 |
| ランチャーがゲーム本体を起動できない (c0000018) | 全プロセスが 1 つのアドレス空間を共有。子がランチャーの領域を使う間、ランチャーを退避して止める | 8403f3f |
| Eushully などのインストーラが wasm メモリを使い切る | 準備時にインストーラを展開 (InstallShield ストリーム、MSI、入れ子の Inno)。BGI のインストール媒体マーカーを除く | faeef0b, d83d8bb |
| MSI 版がインストール先をレジストリから読めない | MSI の Registry テーブルを起動前に書き込む | bebaddc |
| AdvHD/WillPlus が 645MB 確保して abort | CPUID 0x80000000 が 0 を返していた。拡張リーフとブランド文字列 | b02b9aa |
| Waffle・Majiro で描画が止まる | 対象が消えた保留中のクリアでページが例外。破棄する | 02b1da6 |
| Direct2D を使う D3D11 タイトル (Silkys plus、Cyberworks、FG) | ID3D11Device1/2、DeviceContext1/2、デバイスコンテキスト状態を実装 | ca3b40b |
| Lump of Sugar が不正命令 | MASKMOVQ / MASKMOVDQU | c52d222 |
| ChakraCore (Cyberworks 2026) が fail-fast | 書き込みコピーのページを PAGE_READWRITE と報告 | 9a7683c |
| ASProtect の DLL (Eushully AGERC.DLL) が古い翻訳を実行 | W\|X のページと DLL の W\|X セクションは JIT しない | 9768e4d |
| QLIE が GetBackBuffer の参照数で失敗 | バックバッファ・自動深度・スワップチェーンを内部参照で保持 | 9768e4d |
| 天冥のコンキスタ (Eushully, D3D11) が C0000005 | 設定ファイル (SYS4REG.INI) を MSI の LocalAppData から配置、ID3D10Multithread、大きさの違う深度バッファに代替 | 084c468, fd3dc82 |
| 天冥の台詞が出ない | Wine の dwrite の Unix 側 (FreeType) が wasm に入っていなかった。Direct2D が使う IDXGISurface::Map | 8c8135e |
| QLIE barista_lab が黒画面 (0x0eedfade) | FBSTP が無く #UD を Delphi が握りつぶしていた (FBSTP/FBLD を追加)。FVF の要素がストライドを超えてパイプラインが無効 | 9958164 |
| 百千・戦女神VERITA (ASProtect で exe を自己復号) が JIT のときだけ落ちる | exe の W\|X ページで翻訳済みコードへの書き込みを検出し、翻訳を捨ててそのページは解釈実行 (翻訳器スレッド) | 6b2f6c5 |
| こすぷれえっち (Cyberworks) が消失点状に崩れる | 固定機能パイプラインの変換行列を転置して渡していた | e125e26 |

## 残っている失敗のグループ

2 周目 (2026-11-02、上の表の修正後) の時点。

- **AdvHD (6 本)**: 描画は進むが出力が真っ黒のまま (500 秒待ってもタイトルが出ない)。GetRenderTargetData で読み戻した
  画素も RGB が全部 0。毎フレームの読み戻しが 1 回 110ms かかり 2〜3fps しか出ない (このコンテナはソフトウェア GPU)。
- **FVP (いたずら極)**: 告知画面とタイトルは出るが、3D の本編に入ると黒画面。
- **ランチャーが 2 つ目のアプリプロセスを起動できない**: 8403f3f で大半は解消。
- **BGI のインストーラ版 (sekachu, sukerasparo など)**: 最初のフレームの直後に自分で終了 (インタプリタでも同じ)。未解明。
- **null 読み込みのページフォールト (silkys fuyukuru ほか)**: exe の 0x43xxxx で 0 を読む。
- **その他**: WolfRPG RJ01705134 (ウィンドウが出ない)、YU-RIS r1_yuris、debonosu VJ01000787 (解放済みメモリを読む)、
  rikuzendo (SRPG Studio の起動失敗)、engagekinks (動画、quartz のフィルタが無い)。
- **動画全般**: Media Foundation の動画は GStreamer が無いので再生されない (天冥は飛ばして進む)。WMA も同じ。

Eushully の 1600x900 ウィンドウは既定の 1366x768 画面に収まらないので `VNVM_SCREEN=1920x1080` で動かす。

## ベンチマーク

エンジンごとに 1 本ずつ、16 本。どれも初回起動 (JIT の保存済み翻訳なし)。
「Android 相当」は `tests/android-like.sh` (コアごとに同優先度の busy loop を 1 本走らせ、各スレッドの取り分を半分にする。
ユーザーの端末とこのコンテナの bench.html の比から決めた設定) の下で計測。本編 CPU は本編を自動クリックで進めている間の
Chromium 全体の平均使用コア数。

| エンジン | タイトル | タイトルまで (秒) | 本編 CPU (コア) | Android 相当: タイトルまで | Android 相当: 本編 CPU |
|---|---|---|---|---|---|
| Softpal | 美乳淫妹 遥「私にイヤらしいコスプレさせるなんて……この変態 | 6.3 | 0.08 | 17.3 | 0.17 |
| KiriKiri | 背徳の母娘丼～淫欲に沈むママといもうと～　体験版　Ver1. | 13.4 | 0.07 | 38.3 | 0.13 |
| QLIE | かみぱに！体験版 | 7.3 | 0.23 | 27.4 | 0.73 |
| NeXAS | angel breath - エンジェルブレス体験版 | 13.7 | 0.43 | 28.8 | 0.50 |
| YU-RIS | 息子の友達に堕とされて　体験版 | 9.5 | 0.24 | 30.7 | 0.69 |
| SiglusEngine | シオンの血族 | 17.8 | 0.27 | 43.2 | 0.31 |
| Artemis | フユキス　体験版 - Ver1.0.0 | 18.2 | 0.18 | 101.6 | 0.42 |
| CatSystem2 | 体験版 甘えてくれる恋人とのエッチなひととき編 | 19.1 | 0.97 | 58.6 | 0.80 |
| RealLive | 聖炎天使エレアノール 体験版 | 26.9 | 0.80 | 42.3 | 0.66 |
| ShiinaRio | 『種憑け村』序章体験版・導ノ儀 | 7.3 | - | 21.8 | 1.05 |
| EntisGLS | 狙われた優等生　身代わりの代償　体験版 | 45.5 | 0.33 | 34.1 | 0.39 |
| Kaguya | さらさらささら（体験版） | 15.6 | 0.42 | 29.9 | 0.76 |
| Bishop (BSA) | 姫奴隷 | 11.6 | 0.25 | 17.2 | 0.28 |
| LiveMaker | 都合のよいセックスフレンド？体験版 | - | - | - | - |
| Unity | アイシング-love coating- 体験版 Versio | 39.0 | 0.79 | 71.9 | 0.83 |
| Cyberworks (Arc0x.dat) | ぜったいマジラブ！あまイチャはぷにんぐ！体験版 Ver 1. | 32.5 | 0.14 | 47.4 | 0.15 |

- Android 相当ではタイトルまでの時間が 1.5〜3 倍 (Artemis は 5.6 倍の 102 秒)。Artemis は初回の翻訳量が多く、
  2 回目以降は保存済み翻訳 (vnvm-server --jit) で縮む。
- 本編の CPU は多くが 1 コア未満。ShiinaRio (Android 相当 1.05 コア)、CatSystem2・RealLive・Unity (0.7〜0.9) が重い。
- EntisGLS の通常速度 45.5 秒は外れ値 (1 周目の同じタイトルは 14.5 秒)。LiveMaker はこの回はタイトル判定に失敗。
- 実機 (Android) での最終確認はまだ。

### 重いタイトル: ドーナドーナ (alice_0053)

ユーザー指定の重めの体験版。初回起動 (保存済み翻訳なし)、main 9958164 時点。

| 条件 | タイトルまで (秒) | タイトル静止まで (秒) | CPU (コア) |
|---|---|---|---|
| 通常速度 | 43.0 | 52.5 | 0.51 (本編) |
| Android 相当 | 105.3 | 132.6 | 0.69 (メニュー。本編には入れず) |

## 全タイトル


| engine | run | title |
|---|---|---|
| KiriKiri | 23 | 22 |
| Softpal | 12 | 11 |
| - | 11 | 7 |
| NeXAS | 11 | 6 |
| BGI/Ethornell | 8 | 3 |
| AdvHD | 6 | 0 |
| Bishop (BSA) | 5 | 1 |
| CatSystem2 | 5 | 4 |
| Crowd/Anim (PCK/GAX/EOG) | 5 | 5 |
| Cyberworks (Arc0x.dat) | 5 | 2 |
| Escude | 5 | 5 |
| FVP | 5 | 2 |
| Interheart/CandySoft (FPK) | 5 | 5 |
| Kaguya | 5 | 3 |
| Lilim (AOS) | 5 | 5 |
| RealLive | 5 | 4 |
| Unity | 5 | 4 |
| Waffle (.pak) | 5 | 3 |
| YU-RIS | 5 | 4 |
| Yuka (YKC) | 5 | 3 |
| dat?* | 5 | 2 |
| tmrhiro | 5 | 5 |
| AdvWin32/WillPlus | 4 | 1 |
| EntisGLS | 4 | 4 |
| Eushully AGE | 4 | 0 |
| LiveMaker | 4 | 2 |
| Majiro | 4 | 3 |
| QLIE | 4 | 2 |
| ShiinaRio | 4 | 2 |
| SiglusEngine | 4 | 1 |
| AliceSoft System4 | 3 | 3 |
| Artemis | 3 | 2 |
| DxLib (MED) | 3 | 0 |
| NScripter | 3 | 1 |
| NW.js/Tyrano | 3 | 0 |
| WolfRPG | 3 | 1 |
| AnimeGameSystem (ags.exe, .pk) | 2 | 2 |
| BGI?* | 2 | 1 |
| NonColor (ACV1) | 2 | 0 |
| Silky's (AI6WIN) | 2 | 0 |
| System-NNN (BlackCyc) | 2 | 2 |
| WolfRPG* | 2 | 2 |
| advhd | 2 | 0 |
| sakanagl | 2 | 2 |
| AGE (Eushully) | 1 | 0 |
| AZ System (ARC) | 1 | 1 |
| HTML5 | 1 | 0 |
| KiriKiri2 | 1 | 0 |
| Musica | 1 | 0 |
| PONScripter | 1 | 0 |
| Pajamas (GAMEDAT PAC2) | 1 | 1 |
| Propeller (MPK) | 1 | 1 |
| Ren'Py | 1 | 0 |
| RenPy | 1 | 0 |
| TyranoScript | 1 | 0 |
| cmvs | 1 | 1 |
| debonosu | 1 | 0 |
| entis | 1 | 1 |
| livemaker | 1 | 0 |
| pak?* | 1 | 0 |
| propeller | 1 | 1 |
| qlie | 1 | 0 |
| shiina | 1 | 0 |
| siglus | 1 | 0 |
| unknown (Atelier GH .arc) | 1 | 0 |
| unknown (FG .arc + Loader*.dll) | 1 | 0 |
| unknown (SDTS/SRTS) | 1 | 0 |

| engine | year | title | id | graphics | result | title s | story cores | note |
|---|---|---|---|---|---|---|---|---|
| - | 2006 | 逃亡者毒島音露第３話（体験版） | s2006_waffle_tobosya3 |  | no title |  |  | launcher starts the game as a 2nd app process: no room in the shared address space |
| - | 2007 | 虐襲弐　～巫女ノ祭壇～　体験版 | s2007_anim_gyaku2 |  | title | 22 | 0.16 |  |
| - | 2008 | 処女狩～オトメガリ　- TRIAL VERSION - | s2008_anim_otomegari |  | title | 9 | 0.13 |  |
| - | 2008 | PrimaStella（体験版１） | s2008_kaguya_primastella | D3D (dynamic) | no title |  |  |  |
| - | 2009 | 義母散華 - TRIAL VERSION - | s2009_anim_hahasange |  | title | 13 | 0.20 |  |
| - | 2010 | 悪の女幹部　体験版 | s2010_lune_evilwoman_executive |  | title | 24 | 0.04 |  |
| - | 2012 | スペルマン（体験版） | s2012_kaguya_sperman |  | no title |  |  |  |
| - | 2016 | アイドルパフパフマネージメント（体験版） | s2016_kaguya_idol_pafupafu | D3D (dynamic) | title | 25 | 0.11 |  |
| - | 2017 | 人妻寝取り屋 Trial Version: 1.0.0.0 [data ver: | s2017_inter_heart_glossy_hitozuma_ntr | d3d9, d3dx9, dsound | title | 20 | 0.14 |  |
| - | 2019 | 風雷戦姫　神夢　体験版 | s2019_lilim_shinmu |  | title | 20 | 0.37 |  |
| - | 2021 | ふゆから、くるる。 体験版 | s2021_silkys_plus_fuyukuru |  | no title |  |  | wine: Unhandled page fault on read access to 00000000 at address 00432880 (threa |
| AGE (Eushully) | 2009 | 姫狩りダンジョンマイスター 体験版 | b2009_eushully |  | no title |  |  |  |
| AZ System (ARC) | 2006 | b2006_alcot | b2006_alcot |  | title | 11 | 0.07 |  |
| AdvHD |  | 有閑夫人倶楽部 体験版 | r1_advhd |  | no title |  |  |  |
| AdvHD | 2013 | お嬢様はご機嫌ナナメ 体験版 | b2013_ensemble |  | download/extract failed |  |  |  |
| AdvHD | 2015 | 淫らに喘ぐ七人の夜這いする孕女　体験版 | b2015_guilty |  | no title |  |  | launcher shows screen 0x0, warns "unsupported", OK greyed |
| AdvHD | 2015 | エッチでヘンタイ！ヤキモチお嬢様！！　ヘンタイ性癖解放Ver．体験版 | s2015_ensemble_sweet_yakimochi |  | download/extract failed |  |  |  |
| AdvHD | 2019 | 孕女～精子を欲しがる淫らな教え子～ 体験版 | s2019_guilty_harame | d3d11, dxgi, d3d9, d3dx9 | no title |  |  |  |
| AdvHD | 2026 | リビドーアバンちゅ～る TRIAL EDITION | s2026_ensemble_sweet_ribichu |  | x64 (skipped) |  |  |  |
| AdvWin32/WillPlus | 2006 | 遥かに仰ぎ、麗しの - 体験版 | s2006_pulltop_haruka |  | title | 7 |  |  |
| AdvWin32/WillPlus | 2010 | 恋神　-ラブカミ-（Web用体験版） | s2010_pulltop_lovekami |  | no title |  |  |  |
| AdvWin32/WillPlus | 2018 | 僕と彼女のご奉仕同棲　体験版 | b2018_tokohana | d3d11, dxgi, d3d9, d3dx9, dsound | no title |  |  |  |
| AdvWin32/WillPlus | 2025 | のーぶるバトラー TRIAL EDITION | b2025_ensemble_sweet |  | no title |  |  |  |
| AliceSoft System4 | 2012 | 母娘乱館 体験版 | b2012_alicesoft |  | title | 16 | 0.32 |  |
| AliceSoft System4 | 2013 | ランス01体験版 | b2013_alicesoft | d3d9 | title | 21 | 0.43 |  |
| AliceSoft System4 | 2013 | どらぺこ！体験版 | s2013_alicesoft_drapeko | d3d9 | title | 19 | 0.65 |  |
| AnimeGameSystem (ags.exe, .pk) | 2016 | お嬢様はイイナリみたいです！？ 体験版 | b2016_evoll | ddraw, dsound | title | 18 | 0.08 |  |
| AnimeGameSystem (ags.exe, .pk) | 2020 | はじカノ -君がいる部屋- 体験版 | b2020_caramel_box_strawberry | ddraw, dsound | title | 24 | 0.10 |  |
| Artemis | 2020 | まおかつ！　体験版 - Ver1.1 | s2020_wonder_fool_maokatsu | d3d9, dsound | title | 35 | 0.23 | fixed: CPU topology query unimplemented in wasm, ConcRT threw |
| Artemis | 2021 | フユキス　体験版 - Ver1.0.0 | s2021_giga_fuyukiss | D3D (dynamic) | title | 24 | 0.00 | fixed: D3D9 batch grew to 1 GB loading images (OOM); split at 64 MB (6ce41d5) |
| Artemis | 2023 | リアルエロゲシチュエーション！DT　体験版 - Ver1.0.0 | s2023_j_studio_reaero_dt |  | x64 (skipped) |  |  |  |
| BGI/Ethornell | 2006 | いつか、届く、あの空に。【体験版】 | b2006_lumpofsugar |  | title | 17 | 0.27 |  |
| BGI/Ethornell | 2010 | PrismRhythm-プリズムリズム- Web特別体験版 | s2010_lumpofsugar_prismrhythm |  | no title |  |  |  |
| BGI/Ethornell | 2011 | 学☆王 - THE ROYAL SEVEN STARS - 体験入学版 | s2011_lumpofsugar_gackoh | D3D (dynamic) | no title |  |  |  |
| BGI/Ethornell | 2013 | b2013_lumpofsugar | b2013_lumpofsugar |  | no title |  |  | BGI installer build: exits silently after its first frame (interpreter too) |
| BGI/Ethornell | 2014 | MeltyMoment 体験版 Ver1.00 | s2014_hooksoft_meltymoment | D3D (dynamic) | title | 49 | 0.43 |  |
| BGI/Ethornell | 2016 | AmenitysLife Ver1.00 | s2016_hooksoft_amenitys_life | D3D (dynamic) | title | 28 | 0.36 |  |
| BGI/Ethornell | 2019 | b2019_sukerasparo | b2019_sukerasparo | d3d9, dsound | no title |  |  | BGI installer build: exits silently after its first frame |
| BGI/Ethornell | 2022 | 放課後シンデレラ２ 体験版 Ver1.01 | s2022_hooksoft_houkago2 | d3d9, dsound | no title |  |  |  |
| BGI?* | 2015 | 根雪の幻影　白花荘の人々　体験版第2弾 | s2015_silkys_plus_neyuki |  | no title |  |  |  |
| BGI?* | ~2026 | 騎士見習いと村娘 | dl_RJ01720376 | D3D (dynamic) | title | 12 | 0.35 |  |
| Bishop (BSA) | 2007 | 姫奴隷 | b2007_bishop |  | title | 6 | 0.14 |  |
| Bishop (BSA) | 2008 | b2008_bishop | b2008_bishop |  | no title |  |  | launcher starts the game as a 2nd app process: no room in the shared address space |
| Bishop (BSA) | 2010 | 学園３～華麗なる悦辱～ 体験版 | b2010_bishop |  | no title |  |  |  |
| Bishop (BSA) | 2011 | 裏教師～背徳の淫悦授業～ 体験版 | b2011_bishop |  | no title |  |  |  |
| Bishop (BSA) | 2012 | b2012_bishop | b2012_bishop |  | no title |  |  |  |
| CatSystem2 | 2008 | スズノネセブン！体験版 | s2008_clochette_suzunone7 | D3D (dynamic) | title | 15 | 0.35 |  |
| CatSystem2 | 2011 | カミカゼ☆エクスプローラー！WEB体験版 | s2011_clochette_kamikaze | D3D (dynamic) | title | 27 | 0.33 |  |
| CatSystem2 | 2014 | 体験版 甘えてくれる恋人とのエッチなひととき編 | b2014_azarashi_soft | D3D (dynamic) | title | 20 | 0.85 |  |
| CatSystem2 | 2014 | サキガケ⇒ジェネレーション！ WEB体験版 | s2014_clochette_sakigake | D3D (dynamic) | title | 17 | 0.31 | launcher ウインドウ needs a click; then "当ゲームシステムはこのゲームでは動作出来ません" |
| CatSystem2 | 2015 | ここから夏のイノセンス！ WEB体験版 | s2015_clochette_kokonatsu | D3D (dynamic) | no title |  |  |  |
| Crowd/Anim (PCK/GAX/EOG) | 2007 | 必殺痴漢人　- TRIAL VERSION - | b2007_anim |  | title | 23 | 0.21 | "graphics driver missing": explorer start timed out under load; fixed (60 s) |
| Crowd/Anim (PCK/GAX/EOG) | 2009 | 愛娘という名の玩具　- TRIAL VERSION - | b2009_anim |  | title | 14 | 0.23 |  |
| Crowd/Anim (PCK/GAX/EOG) | 2011 | NTR彼女はいじめられっ娘 - TRIAL VERSION - | b2011_anim_team_mm | d3d9, d3dx9, dsound | title | 12 | 0.15 |  |
| Crowd/Anim (PCK/GAX/EOG) | 2019 | いつまでも息子のままじゃいられない！５ - 体験版 - | b2019_anim | d3d9, d3dx9, dsound | title | 44 | 0.24 |  |
| Crowd/Anim (PCK/GAX/EOG) | 2026 | ママラブ！２～俺の彼女は俺のママ☆世話焼き巨乳凛桜ママ～ - 体験版 - | b2026_anim | D3D (dynamic) | title | 39 | 0.25 |  |
| Cyberworks (Arc0x.dat) | 2012 | 籠女の繭 体験版 | b2012_tinkerbell | D3D (dynamic) | title | 14 | 0.10 |  |
| Cyberworks (Arc0x.dat) | 2013 | ぜったいマジラブ！あまイチャはぷにんぐ！体験版 Ver 1.0 | b2013_tinkerbell | D3D (dynamic) | title | 31 | 0.09 |  |
| Cyberworks (Arc0x.dat) | 2014 | こすぷれえっち 体験版 | b2014_wendybell | dsound | title | 31 |  | 本編まで (e125e26) |
| Cyberworks (Arc0x.dat) | 2021 | 淫妖蟲 穢 ～凌蝕桃源退魔録～体験版 Version01 | b2021_tinkerbell | d3d11, dsound | no title |  |  | wine: Unhandled page fault on read access to 00000000 at address 00443D63 (threa |
| Cyberworks (Arc0x.dat) | 2026 | 友恵の探偵物語～のぞき魔な私の天職～体験版 | b2026_wendybell |  | no title |  |  | 0068:err:seh:NtRaiseException Unhandled exception code c0000602 flags 1 addr 0x4 |
| DxLib (MED) | 2012 | つぼい君のスイッチ！体験版 | b2012_fudegaki_soft | dsound | no title |  |  |  |
| DxLib (MED) | 2012 | MonsterParkⅡ | b2012_trois | dsound | no title |  |  |  |
| DxLib (MED) | 2018 | 催眠学園１年生体験版 | b2018_fudegaki_soft | dsound | title | 41 |  | 本編まで (開始ボタン 1061,312) |
| EntisGLS | 2007 | リリカル♪りりっく体験版 | b2007_marmalade |  | title | 14 | 0.02 |  |
| EntisGLS | 2009 | キスと魔王と紅茶（体験版） | b2009_marmalade |  | title | 20 | 1.09 |  |
| EntisGLS | 2024 | 狙われた優等生　身代わりの代償　体験版 | b2024_syrup_mm | opengl, video | title | 14 | 0.22 |  |
| EntisGLS | 2025 | バカップル・サプリメント体験版 | s2025_marmalade_bakaple | D3D (dynamic) | title | 26 | 0.16 |  |
| Escude | 2008 | ヴェルディア幻奏曲体験版 | b2008_escude |  | title | 13 | 0.34 |  |
| Escude | 2009 | 乙女恋心プリスター体験版 | s2009_escude_prister |  | title | 23 | 0.23 |  |
| Escude | 2012 | 紅蓮華 - 体験版 | s2012_escude_gurenka |  | title | 17 | 0.12 |  |
| Escude | 2014 | Re;Lord ～ヘルフォルトの魔女とぬいぐるみ～ - 体験版 Ver. | b2014_escude | D3D (dynamic) | title | 10 | 0.25 |  |
| Escude | 2014 | 花嫁と魔王 - 体験版 Ver. | s2014_escude_hanaou | D3D (dynamic) | title | 15 | 0.25 |  |
| Eushully AGE | 2010 | 戦女神VERITA体験版 | s2010_eushully_ikusa_verita |  | title | 112 |  | 本編の台詞まで (Game Start 185,325)。ASProtect の自己復号は 6b2f6c5 で対応 |
| Eushully AGE | 2016 | 珊海王の円環体験版 | s2016_eushully_sankaio |  | no title |  |  |  |
| Eushully AGE | 2020 | 天冥のコンキスタ体験版 | s2020_eushully_tenmei |  | title | 66 | 0.32 | 本編の台詞まで (VNVM_SCREEN=1920x1080、8c8135e) |
| Eushully AGE | 2024 | 百千の定にかわたれし剋 体験版 | s2024_eushully_hyakusen |  | title | 106 |  | タイトルまで (解釈実行だけだと 349 秒)。6b2f6c5 |
| FVP | 2008 | いたずら極―リアル―　体験版 | b2008_interheart | d3d9, ddraw, dsound | title | 21 |  | 告知画面とタイトルまで。本編 (3D) は黒画面 |
| FVP | 2009 | らぶでれーしょん！ Ver1.00 | b2009_smee | d3d8 | title | 14 | 0.05 |  |
| FVP | 2016 | 小悪魔発育チュ～！（体験版） | b2016_kaguya | D3D (dynamic) | title | 19 | 0.08 |  |
| FVP | 2018 | 恋はそっと咲く花のように　体験版 | b2018_ensemble | d3d11, dxgi, d3d9, d3dx9, dsound | no title |  |  |  |
| FVP | 2025 | 虜ノ焔 ～淫欲の炎に満ちた地下闘技場～ 体験版 | b2025_guilty | d3d11, dxgi, d3d9, d3dx9 | no title |  |  |  |
| HTML5 |  | アイドルダークダンジョン | dl_RJ01720685 |  | download/extract failed |  |  |  |
| Interheart/CandySoft (FPK) | 2006 | 『オイラは番台２』体験版 | b2006_interheart |  | title | 7 | 0.81 |  |
| Interheart/CandySoft (FPK) | 2016 | はらかつ！ ～気になるあの子と子作りエッチ～ Trial Version: 1. | b2016_inter_heart_glossy | d3d9, d3dx9, dsound | title | 31 | 0.17 |  |
| Interheart/CandySoft (FPK) | 2017 | はらかつ２　～子作りエッチでお悩み解決～ Trial Version: 1.0. | b2017_inter_heart_glossy | d3d9, d3dx9, dsound | title | 21 | 0.19 |  |
| Interheart/CandySoft (FPK) | 2018 | ノットフィクション！ Trial Version: 1.0.0.0 [data  | b2018_hachimitsu_soft | d3d9, d3dx9, dsound | title | 7 | 0.14 |  |
| Interheart/CandySoft (FPK) | 2026 | どいなかガールズゆーとぴあ Trial Version: 1.0.0.0 [da | b2026_dessert_soft | D3D (dynamic) | title | 13 | 0.11 |  |
| Kaguya | 2006 | 淫皇覇伝アマツ（体験版） | b2006_kaguya |  | title | 9 | 0.06 |  |
| Kaguya | 2007 | でるた！（体験版） | b2007_kaguya | D3D (dynamic) | no title |  |  |  |
| Kaguya | 2009 | さらさらささら（体験版） | b2009_kaguya |  | title | 21 | 0.28 |  |
| Kaguya | 2010 | キラリ☆南国小麦色　体験版 | b2010_kaguya |  | title | 14 | 0.08 |  |
| Kaguya | 2013 | 肉牝Ｒ３０（体験版） | b2013_kaguya | d3d9, dsound | no title |  |  |  |
| KiriKiri | 2006 | 機械仕掛けのイブ　～Ｄｅａ　Ｅｘ　Ｍａｃｈｉｎａ～ | b2006_ninetail |  | title | 19 | 0.08 |  |
| KiriKiri | 2008 | ＷＬＯ-世界恋愛機構- web用体験版 | b2008_akabeesoft |  | title | 17 | 0.10 |  |
| KiriKiri | 2010 | Reunion～３日間だけのプチ同棲～　体験版 | b2010_grand_cru |  | title | 14 | 0.07 |  |
| KiriKiri | 2011 | しゅ～てぃんぐ妹スター☆ ～おにいちゃん、覚悟しててよね♪～　体験版 | s2011_grand_cru_imostar |  | title | 17 | 0.09 |  |
| KiriKiri | 2012 | Gears of Dragoon  -RPG体験版 ver.1.00- | s2012_ninetail_gears_of_dragoon |  | title | 19 | 0.12 |  |
| KiriKiri | 2013 | 妹のおかげでモテすぎてヤバい。 体験版 | b2013_hulotte |  | title | 12 | 0.09 |  |
| KiriKiri | 2014 | 奪われた学園～罠に落ちた優等生～　体験版 | b2014_undermoon |  | title | 17 | 0.40 |  |
| KiriKiri | 2015 | 極煌戦姫ミストルティア 体験版 | b2015_mille_feuille |  | title | 21 | 0.17 |  |
| KiriKiri | 2015 | 嫁探しが捗りすぎてヤバい。 体験版 | s2015_hulotte_yomeyaba |  | title | 21 | 0.25 |  |
| KiriKiri | 2016 | よめがみ My Sweet Goddess! リコＨシーン体験版 | b2016_alcot | D3D (dynamic) | title | 17 | 0.09 |  |
| KiriKiri | 2017 | 少女と年の差、ふたまわり。 | s2017_akabeesoft3_tonofuta | D3D (dynamic) | title | 22 | 0.08 |  |
| KiriKiri | 2017 | 背徳の母娘丼～淫欲に沈むママといもうと～　体験版　Ver1.00 | s2017_appetite_immorality_don |  | title | 17 | 0.09 |  |
| KiriKiri | 2017 | 春音アリス＊グラム　体験版 | s2017_nanawind_alice_gram | D3D (dynamic) | title | 19 | 0.06 |  |
| KiriKiri | 2018 | 彼女が野球部の性処理マネージャーに…～体育会系セックスで寝取られて～体験版 | s2018_pin_point_yakyurare |  | no title |  |  | launcher starts the game as a 2nd app process: no room in the shared address space |
| KiriKiri | 2019 | [2019/05/08 01][mode_title.ks - 164]ママの妹 | s2019_appetite_dt_kyoiku |  | title | 14 | 0.09 |  |
| KiriKiri | 2020 | 墓多 DYING ZOMBIES -体験版- | b2020_akabeesoft3 | D3D (dynamic) | title | 34 | 1.08 |  |
| KiriKiri | 2020 | リトルシックガール　～桃蜜は妹の香り～　体験版 | b2020_lass_pixy |  | title | 28 | 0.06 |  |
| KiriKiri | 2021 | 愛する彼女は画面の中、他の男の上で腰をふる 体験版 | b2021_at_sakura |  | title | 19 | 0.07 |  |
| KiriKiri | 2021 | VenusBlood HOLLOW International SRPG Tri | s2021_dualtail_vbhi | D3D (dynamic) | title | 23 | 0.07 |  |
| KiriKiri | 2021 | 花鐘カナデ＊グラム Chapter1 -小桜結- 体験版 | s2021_nanawind_kanade_gram_ch1 | D3D (dynamic) | title | 26 | 0.64 |  |
| KiriKiri | 2022 | ROLEPLAYER：とろろ姉妹の粘膜ポトレ ぐりぐちゃLIVE！ | s2022_akabeesoft2_roleplayer_tororo | D3D (dynamic) | title | 27 | 0.09 |  |
| KiriKiri | 2023 | 洗脳航路～エイリアンの苗床に導かれるエリート女軍人たち～　体験版　Ver1.00 | b2023_appetite |  | title | 14 | 0.08 |  |
| KiriKiri | 2026 | 姉妹X催眠２ - MOTION EDITION - 体験版 | s2026_poison_motion_sxs2_me | video | title | 13 | 0.07 |  |
| KiriKiri2 | ~2010 | KAG3 紹介デモ | r1_kag3 |  | no title |  |  |  |
| Lilim (AOS) | 2006 | とらぶるっ！ | b2006_lilim |  | title | 15 | 0.65 |  |
| Lilim (AOS) | 2007 | 看護戦隊ナースレンジャー | b2007_lilim |  | title | 19 | 0.37 |  |
| Lilim (AOS) | 2007 | b2007_lilim_darkness | b2007_lilim_darkness |  | title | 10 | 0.64 |  |
| Lilim (AOS) | 2009 | あい☆きゃん | b2009_lilim | d3d9, ddraw, d3dx9, dsound | title | 14 | 0.33 |  |
| Lilim (AOS) | 2021 | プリンセス☆シスターズ！～四姉妹は全員あなたの許嫁～　web体験版ver1.0 | b2021_princess_sugar |  | title | 17 | 0.34 |  |
| LiveMaker | 2008 | ダンジョンクルセイダーズ２（体験版） | s2008_kaguya_dc2 | D3D (dynamic) | no title |  |  |  |
| LiveMaker | 2011 | 都合のよいセックスフレンド？体験版 | s2011_waffle_sex_friend | D3D (dynamic) | title | 10 | 0.54 |  |
| LiveMaker | 2017 | 俺嫁！？　体験版Ver.2 | s2017_wendybell_oreyome | d3d11, dsound | no title |  |  | wine: Unhandled page fault on read access to 00000000 at address 0043AC93 (threa |
| LiveMaker | 2019 | わんこの嫁入り ～新たな二本のしっぽ～ 体験版 version 1.00 | s2019_alumi_soft_inuyome2 | D3D (dynamic) | title | 71 | 0.43 |  |
| Majiro | 2011 | Starless 体験版 | b2011_empress |  | title | 5 | 0.01 |  |
| Majiro | 2012 | LEWDNESS ～Vita sexualis～ | s2012_empress_lewdness | video | title | 7 | 0.13 |  |
| Majiro | 2015 | ClosedGAME体験版 | b2015_empress |  | no title |  |  | pageerror: Cannot read properties of null (reading 'end') |
| Majiro | ~2008 | あいれぼ 体験版 | r1_majiro |  | title | 5 | 0.00 |  |
| Musica |  | Wind RP 体験版 | r1_musica |  | download/extract failed |  |  |  |
| NScripter |  | ナルキッソス SIDE 2nd | r1_naru2 |  | title | 6 | 0.03 |  |
| NScripter | ~2008 | 彼岸花の咲く夜に お試し版 | r1_higanbana |  | no title |  |  | launcher starts the game as a 2nd app process: no room in the shared address space |
| NScripter | ~2015 | トライアンソロジー 体験版 | r1_trt | dsound | no title |  |  |  |
| NW.js/Tyrano | ~2018 | 雪音の仇討ち物語 | dl_RJ01612664 |  | no title |  |  |  |
| NW.js/Tyrano | ~2018 | 秘密 女騎士NTR | dl_RJ01686418 |  | no title |  |  |  |
| NW.js/Tyrano | ~2018 | ショタおねRPG 爆乳お姉さんたちと催〇でドスケベ三昧 爆乳(クソデカボイン)の | dl_RJ01706991 |  | no title |  |  |  |
| NeXAS | 2006 | angel breath - エンジェルブレス体験版 | s2006_giga_ab | D3D (dynamic) | title | 15 | 0.51 |  |
| NeXAS | 2006 | 妹巫女・萌 | s2006_lune_maimiko |  | title | 10 | 0.03 |  |
| NeXAS | 2007 | 姫さまっ、お手やわらかに！ -体験版- | s2007_giga_himesama | d3d9, ddraw, dsound, video | title | 13 | 0.30 |  |
| NeXAS | 2010 | JINKI EXTEND Re:VISION -SECOND IMPACT- | b2010_giga | d3d9, d3dx9, dsound, video | title | 21 | 0.22 |  |
| NeXAS | 2011 | おっぱい学園マーチングバンド部！　体験版 | s2011_marine_marching | d3d9, dsound | no title |  |  |  |
| NeXAS | 2012 | デカくてエッチな俺の姉　体験版 | s2012_marine_deka_ane | D3D (dynamic) | no title |  |  |  |
| NeXAS | 2013 | キスアト体験版 | b2013_giga | d3d9, d3dx9, dsound, video | title | 21 | 0.09 |  |
| NeXAS | 2014 | リベンジルーム　体験版 | s2014_lune_revenge_room | d3d9, dsound | no title |  |  |  |
| NeXAS | 2016 | リプキス　体験版 | b2016_giga | dsound, video | title | 21 | 0.20 |  |
| NeXAS | 2016 | お嬢様学園こすぷれハーレム部！「今日はどの服でエッチしよっか」 | s2016_marine_cospure_harem | D3D (dynamic) | no title |  |  |  |
| NeXAS | 2019 | 巨乳女戦士・土下座催眠「ちくしょうっ……アタシはお前の思い通りになんか、ならない | s2019_lune_dogeza_saimin | D3D (dynamic) | no title |  |  |  |
| NonColor (ACV1) | 2015 | b2015_minato_carnival | b2015_minato_carnival | d3d9 | no title |  |  | launcher 起動 needs a click |
| NonColor (ACV1) | 2018 | b2018_mirai | b2018_mirai | D3D (dynamic) | no title |  |  |  |
| PONScripter | ~2019 | うみねこのなく頃に咲 体験版 | r1_saku | D3D (dynamic) | no title |  |  |  |
| Pajamas (GAMEDAT PAC2) | 2006 | ななついろ★ドロップス-体験版 | b2006_unisonshift | ddraw, dsound | title | 29 | 0.21 |  |
| Propeller (MPK) | 2007 | 『Bullet Butlers』Web体験版　ver1.1 | b2007_propeller |  | title | 12 | 0.08 |  |
| QLIE | 2007 | かみぱに！体験版 | b2007_clochette | ddraw, dsound | title | 11 | 0.36 | fixed: game patches its code with VirtualProtect; JIT now drops stale translations (39274ab) |
| QLIE | 2009 | シグナルハート 体験版 Ver.01 | b2009_purplesoftware |  | title | 15 | 0.69 |  |
| QLIE | 2020 | アナベル・メイドガーデン体験版 | b2020_barista_lab |  | title | 18 |  | 本編まで (9958164) |
| QLIE | ~2021 | 天翼のアスクレイン 体験版 | r1_qlie | d3d9 | no title |  |  |  |
| RealLive |  | 5 -ファイブ- 体験版 | r1_reallive_five |  | download/extract failed |  |  |  |
| RealLive | 2007 | ハーレム×すくらっち体験版 | b2007_hadashi |  | title | 18 |  |  |
| RealLive | 2007 | 聖炎天使エレアノール 体験版 | b2007_sagaplanets |  | title | 35 | 0.85 |  |
| RealLive | 2008 | Coming×Humming!! -カミング・ハミング- （体験版） | s2008_sagaplanets_cominghumming |  | title | 27 | 0.37 |  |
| RealLive | 2010 | キサラギGOLD★STAR | s2010_sagaplanets_kisaragi | d3d9, dsound | title | 19 | 0.21 |  |
| Ren'Py | ~2026 | The Question | r1_renpy |  | x64 (skipped) |  |  |  |
| RenPy | ~2025 | 魔法少女ネム&モコ 悪墜ち魔法少女の誘惑 | dl_RJ01746340 |  | x64 (skipped) |  |  |  |
| ShiinaRio | 2008 | エンゲージリンクス　体験版 | s2008_alcot_engagekinks |  | no title |  |  | opening video: quartz filters fail (no GStreamer), game exits |
| ShiinaRio | 2009 | 花と乙女に祝福を　体験版 | s2009_ensemble_hanaotome |  | title | 20 | 0.26 |  |
| ShiinaRio | 2011 | 『種憑け村』序章体験版・導ノ儀 | b2011_guilty | ddraw, opengl, dsound | title | 7 | 0.94 |  |
| ShiinaRio | 2017 | b2017_onomatope_raspberry | b2017_onomatope_raspberry |  | no title |  |  |  |
| SiglusEngine | 2011 | シオンの血族 | b2011_catwalk | d3d9, dsound | title | 18 | 0.00 | fixed: Wine named UTC+9 "Korea" (TZ), Siglus refused to start |
| SiglusEngine | 2015 | ろーらいず!!! 体験版 | b2015_hamham | d3d9, dsound | no title |  |  |  |
| SiglusEngine | 2017 | 金色ラブリッチェ体験版 | s2017_sagaplanets_kinkoi | d3d9, dsound | no title |  |  |  |
| SiglusEngine | ~2015 | 【限定版】魔法少女コノカ －異種触手・無限煉獄－ | dl_VJ01005112 | d3d9, dsound | no title |  |  |  |
| Silky's (AI6WIN) | 2014 | b2014_silkys_plus | b2014_silkys_plus |  | no title |  |  |  |
| Silky's (AI6WIN) | 2016 | b2016_silkys_sakura | b2016_silkys_sakura |  | no title |  |  |  |
| Softpal | 2009 | 美乳淫妹 遥「私にイヤらしいコスプレさせるなんて……この変態！」 | b2009_crossover | ddraw, dsound | title | 6 | 0.08 |  |
| Softpal | 2011 | b2011_marine | b2011_marine | D3D (dynamic) | no title |  |  |  |
| Softpal | 2014 | 逆転魔女裁判－痴女な魔女に裁かれちゃう－　【体験版】 | s2014_erectlip_gyokumajyo | D3D (dynamic) | title | 29 | 0.12 |  |
| Softpal | 2014 | 魔女こいにっき 体験版 (第一弾) | s2014_qoobrand_majokoi | D3D (dynamic) | title | 15 | 0.12 |  |
| Softpal | 2015 | 巨乳大魔王のドスケベクエスト ～完全敗北した少年勇者クンＵＣ～　【体験版】 | b2015_erectlip | D3D (dynamic) | title | 29 | 0.11 |  |
| Softpal | 2018 | 爆乳催眠女教師　【体験版】 | b2018_erectlip | D3D (dynamic) | title | 15 | 0.13 |  |
| Softpal | 2020 | 爆乳淫辱霊媒師　【体験版】 | b2020_erectlip | D3D (dynamic) | title | 16 | 0.13 | launcher OK needs a click; then stalls in PAL.dll MsgWaitForMultipleObjects loop (black) |
| Softpal | 2020 | 緊縛性奴会長 ～カレに知られたワタシの秘密～　【体験版】 | s2020_amuse_craft_erotica_kinbaku | D3D (dynamic) | title | 16 | 0.14 | launcher OK needs a click |
| Softpal | 2022 | つまもみ ～欲求不満の奥さんとなし崩しオイルマッサージ～　【体験版】 | s2022_amuse_craft_erotica_tsumamomi | D3D (dynamic) | title | 17 | 0.15 |  |
| Softpal | 2023 | 巨乳オナホ妖怪と田舎ライフもHも満喫生活　【体験版】 | s2023_milimili_onaho_yokai | D3D (dynamic) | title | 15 | 0.18 |  |
| Softpal | 2024 | ちょっとナマイキなトイレの花子さん ～えっちぃ学園のおばけとエロエロ搾精性活～  | b2024_milimili | D3D (dynamic) | title | 17 | 0.27 |  |
| Softpal | 2026 | ディメンション凸ラバース!! 【体験版】 | b2026_crystalia | D3D (dynamic) | title | 16 | 0.14 |  |
| System-NNN (BlackCyc) | 2006 | EXTRAVAGANZA～蟲愛でる少女～体験版 | b2006_blackcyc |  | title | 6 | 0.17 |  |
| System-NNN (BlackCyc) | 2016 | b2016_blackcyc | b2016_blackcyc |  | title | 27 | 0.50 |  |
| TyranoScript |  | 転生した世界で… 体験版 | r1_tyrano |  | x64 (skipped) |  |  |  |
| Unity | 2017 | VenusBlood-BRAVE- ADV体験版 | s2017_dualtail_vbb | opengl | title | 33 | 0.51 |  |
| Unity | 2018 | VenusBloodLagoonSlgTrial | s2018_dualtail_vbl | opengl | title | 17 | 1.55 |  |
| Unity | 2020 | アイシング-love coating- 体験版 Version: 1.0.0.1 | b2020_hachimitsu_soft | D3D (dynamic) | title | 47 | 0.50 |  |
| Unity | 2024 | VenusBlood RAGNAROK International_EN Adv | b2024_ninetail | D3D (dynamic) | title | 56 | 1.02 |  |
| Unity | 2025 | VenusBloodVALKYRIE_SLGTrial | s2025_dualtail_vbv |  | no title |  |  | wine: Unhandled page fault on write access to 00000000 at address 4F0B9C65 (thre |
| Waffle (.pak) | 2010 | b2010_waffle | b2010_waffle |  | no title |  |  |  |
| Waffle (.pak) | 2012 | b2012_waffle | b2012_waffle |  | no title |  |  | pageerror: Cannot read properties of null (reading 'end') |
| Waffle (.pak) | 2014 | b2014_waffle | b2014_waffle | D3D (dynamic) | title | 14 | 0.46 |  |
| Waffle (.pak) | 2016 | b2016_waffle | b2016_waffle | D3D (dynamic) | title | 16 | 0.20 |  |
| Waffle (.pak) | 2017 | b2017_waffle | b2017_waffle | D3D (dynamic) | title | 13 | 0.22 |  |
| WolfRPG | ~2013 | 人狼さんはちんちんがお好き。 | dl_RJ01729227 | D3D (dynamic) | no title |  |  |  |
| WolfRPG | ~2026 | リノ&ハミー | dl_RJ01705134 | dsound | no title |  |  |  |
| WolfRPG | ~2026 | ポンコツ魔法使いセリスのHな冒険記 ～Hな呪いで精液を求める身体になった魔法使い | dl_RJ01724124 | dsound | title | 28 | 0.57 |  |
| WolfRPG* | ~2021 | 精霊商店アルワール | dl_RJ01598948 | D3D (dynamic) | title | 25 | 0.88 |  |
| WolfRPG* | ~2022 | ドレスアップナイト -True Knight’s Refrain- | dl_RJ365650 | D3D (dynamic) | title | 41 | 0.48 |  |
| YU-RIS | 2007 | b2007_whirlpool | b2007_whirlpool |  | title | 16 | 0.23 |  |
| YU-RIS | 2013 | 息子の友達に堕とされて　体験版 | b2013_anim | d3d9, dsound | title | 14 | 0.37 |  |
| YU-RIS | 2014 | 鯨神のティアスティラ 体験版 | s2014_whirlpool_tearstilla |  | title | 20 | 0.80 |  |
| YU-RIS | 2020 | あやかし遊郭～花魁アマビエ民譚集～体験版 | s2020_casket_ayakashi03 | d3d9, dsound | title | 24 | 0.35 |  |
| YU-RIS | ~2025 | YU-RIS サンプル | r1_yuris | d3d9, dsound | no title |  |  | launcher starts the game as a 2nd app process: no room in the shared address space |
| Yuka (YKC) | 2007 | あかね色に染まる坂 | b2007_feng | d3d8 | title | 13 | 0.05 |  |
| Yuka (YKC) | 2008 | FairlyLife WEB体験版02 H_Only Ver1.00 | b2008_hook | d3d8 | title | 13 | 0.06 |  |
| Yuka (YKC) | 2008 | 晴れハレはーれむ 体験版Ver1.00 | b2008_smee | d3d8 | title | 14 | 0.06 |  |
| Yuka (YKC) | 2011 | StrawberryNauts Ver1.00 | b2011_hook | D3D (dynamic) | no title |  |  |  |
| Yuka (YKC) | 2013 | PriministAr（体験版） Ver1.00 | b2013_hooksoft | d3d9 | no title |  |  |  |
| advhd | 2014 | b2014_ensemble_sweet | b2014_ensemble_sweet |  | download/extract failed |  |  |  |
| advhd | 2014 | b2014_guilty | b2014_guilty |  | download/extract failed |  |  |  |
| cmvs | 2011 | 未来ノスタルジア・web体験版v2　- タイトルメニュー - | s2011_purplesoftware_miranosu | d3d9 | title | 8 | 0.14 |  |
| dat?* | 2006 | 放課後２-白濁のレッスン-体験版 | s2006_bishop_hkg2 |  | title | 13 | 0.13 |  |
| dat?* | 2022 | 恥辱の制服2 体験版 | s2022_bishop_cs2 |  | no title |  |  |  |
| dat?* | 2023 | あねつまみ２ ～エッチなお姉さん人妻にたっぷり甘えるめちゃシコLoveLife～ | s2023_anim_anetumami2 | D3D (dynamic) | title | 39 | 0.21 |  |
| dat?* | 2026 | 超狂指導日誌 体験版 | s2026_bishop_tks |  | no title |  |  |  |
| dat?* | ~2026 | 【DLsite限定版】友恵の探偵物語 ～のぞき魔な私の天職～ | dl_VJ01005807 | d3d11, dsound | no title |  |  | 0068:err:seh:NtRaiseException Unhandled exception code c0000602 flags 1 addr 0x4 |
| debonosu | ~2023 | 神楽黎明記 ～紫の章～ 参 | dl_VJ01000787 |  | no title |  |  | launcher starts the game as a 2nd app process: no room in the shared address space |
| entis | 2017 | s2017_marmalade_mashimaro | s2017_marmalade_mashimaro | opengl | title | 27 | 0.83 |  |
| livemaker | 2006 | b2006_pulltop | b2006_pulltop |  | download/extract failed |  |  |  |
| pak?* |  | 政治家寝取られ | dl_RJ01701406 |  | x64 (skipped) |  |  | x64 (UE4 Win64 build) |
| propeller | 2011 | すきま桜とうその都会　体験版２ | s2011_propeller_sukima |  | title | 6 | 1.00 |  |
| qlie | 2026 | s2026_lump_of_sugar_tayutama_zero | s2026_lump_of_sugar_tayutama_zero |  | no title |  |  |  |
| sakanagl | ~2024 | 異世界娘と秘密のコンカフェえっち | dl_VJ01002468 | D3D (dynamic) | title | 11 | 0.22 |  |
| sakanagl | ~2025 | 俺の電脳SEX性活 ‐ビッチな君となんちゃって恋愛‐ | dl_VJ01004064 | D3D (dynamic) | title | 13 | 0.26 |  |
| shiina | 2008 | 輪罠Ⅱ 体験版 | s2008_guilty_wana2 |  | no title |  |  |  |
| siglus | 2023 | s2023_hadashi_harem_shangrila | s2023_hadashi_harem_shangrila | D3D (dynamic) | no title |  |  |  |
| tmrhiro | 2006 | ソウルイーター　体験版　Game Edition | b2006_lune |  | title | 7 | 0.04 |  |
| tmrhiro | 2006 | 人妻搾乳飯店－体験版－ | b2006_marine |  | title | 26 | 0.05 |  |
| tmrhiro | 2006 | くのいち・咲夜－体験版－ | s2006_lune_sakuya |  | title | 9 | 0.05 |  |
| tmrhiro | 2007 | 風輪奸山　体験版 | b2007_lune |  | title | 9 | 0.05 |  |
| tmrhiro | 2008 | びんかんアスリート | b2008_marine |  | title | 15 | 0.06 |  |
| unknown (Atelier GH .arc) | 2012 | 学園ＮＴＲ -僕の知らない彼女の淫顔- 体験版 | b2012_atelier_gh |  | no title |  |  |  |
| unknown (FG .arc + Loader*.dll) | 2023 | 夜勤病棟 リメイク 体験版 | b2023_fg_remake | d3d11, dxgi | no title |  |  | wine: Unhandled page fault on read access to 00000000 at address 00429335 (threa |
| unknown (SDTS/SRTS) | 2020 | ティーリオ傭兵団の初仕事 体験版 v1.2.0 | b2020_rikuzendo | dsound | no title |  |  | "ゲームの起動に失敗しました。B-Phese 2" |
