# 起動の速さと CPU: Windows 実機と masu (2026-11-14)

同じ体験版 (VN Mirror Archive、`tests/corpus/winspeed.tsv` の 28 本) を、Windows 実機と masu で同じ判定で動かした結果。#89 #217-2 #222 #225 #227 #228 と Unity の遅い起動の、「Windows でも同じか」の答え。

- Windows: 808 PC (Core i7-11700 8C/16T、RTX 2080 Ti、2560x1440、日本語 Windows、ACP 932)。`python tests/corpus/winspeed.py WORK` (中で `tests/corpus/winplay.py`)、各 2 回。下の表は 2 回のうち速い方の title と、高い方の storyCores。
- masu: main 8cbb4524 (`tools/ci/build.sh`)、クラウドの 4 コア VM のヘッドレス Chromium、`tests/corpus-run.py` で 1 本ずつ、`MASU_SCREEN=1920x1080`、保存 JIT なしの初回。括弧内は同じストアで保存 JIT ありの 2 回目。
- 判定はどちらも corpus.mjs と同じ: title = 中央 1/3 が 300 色を超えた最初のフレームの秒 (起動から)、story = タイトルの手順のあと 2 秒ごとに下の方をクリックする 30 秒、その間の CPU コア数 (Windows は GetProcessTimes、masu は vnhost_load_busy)。手順は既定 (`key:Return wait:3`)。
- 「本編/タイトル」は story の 30 秒で画面がタイトルから変わったか (leftTitle)。タイトルのままの行は、その画面に置いたときの CPU。

| 作品 | ファイル | Windows title 秒 | masu title 秒 | 倍 | Windows コア | masu コア |
|---|---|---|---|---|---|---|
| 女体汚辱 (#217-2) | `VJ011293_trial.zip` | 26.0 | 57.6 (47.5) | 2.2 | 0.13 (タイトル) | 0.44 (タイトル) |
| appetite 先輩の… (#89) | `apt246_tr.zip` | 3.1 | 16.4 | 5.3 | 0.01 (本編) | 0.03 (本編) |
| apt087 | `apt087_tr.zip` | 5.1 | 14.7 | 2.9 | 0.01 (本編) | 0.03 (タイトル) |
| apt124 | `apt124_tr.zip` | 5.1 | 13.7 | 2.7 | 0.01 (本編) | 0.02 (タイトル) |
| apt139 | `apt139_tr.zip` | 5.1 | 13.7 | 2.7 | 0.01 (本編) | 0.03 (タイトル) |
| apt215 | `apt215_tr.zip` | 3.1 | 13.0 | 4.2 | 0.01 (本編) | 0.03 (本編) |
| apt285 | `apt285_tr.zip` | 3.1 | 13.2 | 4.2 | 0.01 (本編) | 0.02 (タイトル) |
| 寮母さん | `apt278_tr.zip` | 3.1 | 13.0 | 4.2 | 0.01 (本編) | 0.03 (本編) |
| 黒ギャル娘 | `apt117_tr.zip` | 5.2 | 14.1 | 2.7 | 0.01 (本編) | 0.02 (タイトル) |
| 娘の親友JK | `apt021_tr.zip` | 5.1 | 14.1 | 2.8 | 0.04 (本編) | 0.07 (タイトル) |
| クズ親父 | `apt229_tr.zip` | 3.1 | 12.0 | 3.8 | 0.01 (本編) | 0.03 (本編) |
| 妻の母 | `apt012_tr.zip` | 5.1 | 16.9 | 3.3 | 0.04 (本編) | 0.09 (タイトル) |
| 最強の人妻 | `aptd027_tr.zip` | 2.1 | 14.4 | 7.0 | 0.02 (本編) | 0.03 (タイトル) |
| 清楚でビッチな人妻 (#225-4) | `aptd008_tr.zip` | 5.1 | 16.9 | 3.3 | 0.01 (本編) | 0.05 (本編) |
| こいのす | `setup_koicha_trial2.zip` | - | 16.4 | - | - (タイトル) | 0.89 (本編) |
| 催●学級 (#227) | `ggs_0150t.zip` | 20.5 | 17.2 | 0.8 | 0.99 (本編) | 0.87 (本編) |
| 新妻ふわトロ (kaguya GAME_SYS) | `niidumafuwatoro_trial.zip` | - | - | - | - (タイトル) | - (タイトル) |
| VenusBlood (VBB.exe #198) | `VBB_ADVTrial.zip` | 6.2 | 41.4 (39.1) | 6.7 | 0.26 (本編) | 0.92 (本編) |
| GEARS of DRAGOON 2 | `GoD2ADVtrial_web.zip` | 10.3 | 43.6 (45.2) | 4.2 | 0.16 (本編) | 0.95 (本編) |
| 純情化憐フリークス！ | `freaks_trial02.zip` | 5.2 | 38.1 (14.4) | 7.4 | 0.07 (タイトル) | 0.07 (タイトル) |
| ヤキモチストリーム (#228) | `VJ013905_trial.zip` | 14.5 | 27.5 | 1.9 | 0.07 (タイトル) | 0.06 (タイトル) |
| 魔法少女が全裸で土下座 (#225-4) | `dogema_trial.zip` | 6.2 | 15.2 | 2.4 | 0.04 (タイトル) | 0.11 (タイトル) |
| 僕のメイド (Unity, S) | `RJ01203511_trial.zip` | 4.1 | 89.4 (78.3) | 21.6 | 0.06 (タイトル) | 0.81 (タイトル) |
| ニャンコイ (Unity, S) | `RJ436887_trial.zip` | 8.5 | 181.3 | 21.4 | 0.14 (本編) | 0.66 (本編) |
| BLACK-MARKET (Unity, S) | `RJ286256_trial.zip` | 16.5 | 106.0 | 6.4 | 0.42 (本編) | 0.76 (本編) |
| 上倉雛 (Unity, S) | `VJ013432_trial.zip` | 7.4 | 67.4 | 9.1 | 0.13 (タイトル) | 1.52 (タイトル) |
| 妹ちゃんとくらそう (Unity, S) | `RJ01302433_trial.zip` | 9.2 | 76.9 | 8.4 | 0.09 (タイトル) | 0.45 (タイトル) |
| 艶嬢学園2 (Unity, S) | `astronauts_0020t.zip` | 5.3 | 81.8 | 15.5 | 0.16 (本編) | 1.60 (本編) |

注:
- 艶嬢学園2・上倉雛 (Astronauts、Unity 2017.3): ロゴは左クリック待ち (Return/Space は効かない。Windows で確認、項目 S の IL 調査と一致)。両方とも PRE=0.5,0.5 (3 秒ごとに中央クリック) で計測。艶嬢学園2 の title は WARNING 画面 (300 色超) で、表の「本編」コアは実際にはタイトル画面。
- こいのすはインストーラー (setup_koicha_trial2.exe) で、Windows では UAC が出るので測っていない。masu は SAKE_AUTO でインストールして起動。
- 新妻ふわトロは start.exe の起動設定ダイアログで、Windows ではマウスもキーもダイアログに効かず (SendInput、原因未調査) 両方とも測れていない。
- Windows は 1 回目だけ最初の窓が 2〜4 秒 (展開直後の Defender 走査とみられる)。title は 2 回目もほぼ同じ。
- masu の計測中、server が起動直後に何も出さず終わり ERR_CONNECTION_REFUSED になる回が 28 本中 18 本の初回にあった (corpus-run.py が続けて server を立て直すとき)。1 本ずつ別ポートで流し直した。

## 分かったこと
- **1 コア張り付きが Windows でも同じ**: 催●学級 (ggs_0150t) は Windows でも本編 0.99 コア、放置中 0.99 コア。ゲーム自体のビジーループで、masu の不具合ではない。
- **appetite 系 吉里吉里の本編 1 コアは、もう再現しない**: 14 本とも masu の本編/タイトルで 0.02〜0.09 コア (Windows 0.01〜0.04)。以前の 0.95〜1.0 (part 12/14/18/19/22 のメモ) は、その後の main のどこかで解消している (どのコミットかは確かめていない)。
- **Windows と違う 1 コア張り付き (#122 の候補)**: VenusBlood VBB (Windows 0.26 / masu 0.92)、GEARS of DRAGOON 2 (0.16 / 0.95)、Unity 全般 (僕のメイド 0.06 / 0.81、上倉雛 0.13 / 1.52、艶嬢学園2 0.16 / 1.60、BLACK-MARKET 0.42 / 0.76、妹ちゃん 0.09 / 0.45、ニャンコイ 0.14 / 0.66)。
- **起動は全部 masu が遅い**: 吉里吉里 (appetite) で 2.7〜7 倍 (Windows 2〜5 秒 / masu 12〜17 秒)。女体汚辱は Windows でも 26 秒 (ロゴの演出) で masu は 2.2 倍 (保存 JIT ありで 1.8 倍)。Unity は 6〜22 倍。純情化憐は保存 JIT で 38 → 14 秒 (Windows 5 秒)。保存 JIT の効果は作品による (女体汚辱・VBB・GoD2・僕のメイドはほぼ変わらない)。

## 同じ画面を測っているか (2026-11-14 のレビューから)

上の表の title は「中央 1/3 が 300 色を超えた最初のフレーム」なので、艶嬢学園2 のように WARNING 画面をタイトルとして測った行がある。また「本編/タイトル」が両側で違う行 (apt087 / apt124 / apt139 / apt285 / 黒ギャル娘 / 娘の親友JK / 妻の母 / 最強の人妻 / こいのす) は、コア数が別の画面のもの。これらの行の倍率とコア比較は、そのままでは使えない。

そのための仕組み:
- **画面シグネチャ**: `tests/corpus/shots.py sig` と `tests/corpus/sig.mjs` (同じ整数計算。`shots_test.py` が乱数画像で両者の一致を確かめる)。8x6 マスの平均色を 4 ビットずつ、16 進 144 桁。`sig_same` は平均差 1 段以内、3 段を超えて違うマスが 4 個以内なら同じ画面 (点滅カーソルや光るメニュー項目は許し、警告画面や次の画面は別)。
- **作品ごとの期待画面**: `winspeed.tsv` の `title_sig` / `story_sig` 列 (空欄はまだ未定義)。人が確認した Windows の title.png から `python3 tests/corpus/shots.py sig title.png` で作る。入っている行は、winplay.py と corpus.mjs の両方で「その画面になった最初のフレーム」が title になり、結果に `titleMatch` / `storyMatch` が入る。どの行でも結果に `titleSig` と `storySigs` が残る。
- **突き合わせ**: `python3 tests/corpus/winspeed_check.py WIN_OUT MASU_OUT` が、両側の title 画面が同じか (と `title_sig`)、両側とも本編に進んだか (と `story_sig`) を行ごとに出し、同じ画面の行だけ倍率を出す。本編の画面が違う行のコアには「(別画面)」が付く。違う行があれば終了コード 1。

次に測り直すときは、まず Windows 側の title.png を見て `title_sig` を埋め、両側をこの判定で流し直す (上の表はまだ旧判定のまま)。

## Windows では暇なのに masu では 1 コア使う作品の内訳

VBB、GoD2、Unity 各作品 (上の「Windows と違う 1 コア張り付き」) は、JIT の最適化より先に、どこで CPU を使っているかを測る (docs/translator.md の記録では CPU 状態のメモリアクセスを大きく減らしても 3〜7% しか速くならなかった)。

- `SAKE_ENV=SAKE_API_STATS=5` を付けて corpus-run.py で流すと、5 秒ごとに API ごとの呼び出し数と中で使った時間が console.log に出る (crates/sake/src/api.rs)。
- `python3 tests/corpus/cpu_attrib.py console.log --cores <storyCores> --from 30` が、wait (WaitFor* / Sleep / GetMessage)、poll (PeekMessage / timeGetTime / QueryPerformanceCounter / GetAsyncKeyState …)、present (Present / Flip / BitBlt / StretchDIBits / SwapBuffers …)、sound、file、sync、other ごとの毎秒の呼び出し数と ms を出し、残り (ゲーム自身のコードと API 外の sake: 翻訳済み x86、JIT) を出す。短い wait が毎秒 200 回を超えるもの (Sleep(0) 回し) は「poll している wait」として名前を出す。
- 読み方: poll が多い → ポーリングループ (Windows でも同じループだが 1 回が sake では重い: その API を軽くする)。present が多い → 描画の待ち方。どれも少なく guest が大きい → ゲーム自身の計算か JIT (そこで初めて translator)。

注意: Windows はデスクトップ PC、masu はクラウドの 4 コア VM のヘッドレス Chromium なので、倍率の全部が互換レイヤーの重さではない。Android は実機ではなく Chromium のエミュレーションで測る (プロジェクトの方針)。
