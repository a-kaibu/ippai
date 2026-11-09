# 実行ログ (クラッシュの原因調査)

本番のプレイでも、毎回の実行で詳細なログを取り、あとから原因が分かるようにしています。
特別な URL パラメータは要りません。

## どこに残るか

| いつ | 名前 | 保存先 |
| --- | --- | --- |
| 10 秒ごと (変化があれば) | `session-<id>.txt` | ブラウザの OPFS (`vnvm-logs/`) |
| クラッシュ時 | `<id>-crashN` | OPFS とサーバー |
| タブがバックグラウンドに回ったとき (固まったゲームから抜けたとき。起動中も。1 分に 1 回まで) | `<id>-hidden` | OPFS とサーバー |
| Ctrl+Shift+L、または 3 本指タップ | `<id>-manual` | OPFS とサーバー |
| ゲームが終了したとき (一覧に戻ったときも。理由に区別あり) | `<id>-end` | OPFS とサーバー |
| 動いている間、1 分ごと (同じ名前で上書き) | `<id>-live` | サーバー |
| 前のページがゲームの途中で消えていた (Android がメモリ不足でタブを落とした、閉じた) | `<id>-unclean` | 次にページを開いたときにサーバーへ |

`<id>` は `日付-時刻-ゲーム-乱数` です。サーバーは `ippai serve --logs <dir>` (既定 `logs/`、
worker2 では `/home/kota/vnvm/vnvm-bench/logs/`) に `<名前>.txt` で置き、新しい 2000 件を残します。

見るには:

* ブラウザの画面はありません (2026-11-07 に一覧の「ログ」画面は外しました)。OPFS に残ったログは
  次にページを開いたときにサーバーへ送られます。
* サーバー: `ls -t /home/kota/vnvm/vnvm-bench/logs/`、`GET /log-list` (JSON)、`/logs/<名前>.txt`。
  保存のたびにサーバーの標準エラーに `log <path>: <KB>, <reason>` が出ます。

## 中身

1. 実行の情報: ゲーム、exe、引数、マウント、環境変数、Wine / sake、x86 / x86_64、ビルド
   (wine.wasm の日付とサイズ、トランスレータの版)、ブラウザ、コア数、メモリ、画面、URL。
2. 理由: `wine: Unhandled page fault on write access to ...`、sake の致命的エラー、panic、
   wasm の abort / trap、Worker のエラー、捕まらなかった JS のエラーなど。
3. その時点の全ゲストスレッド: ゲストコード中か syscall 中か、レジスタ (x64 は r8-r15 と rip 上位も)、
   EIP のコード、スタック、EBP チェーン、スタック上のコードアドレス (`module+rva (export+off)` に解決)、
   直近の syscall / API 呼び出し、JIT の最近のリージョン間ジャンプ、モジュール一覧。
4. kura のイベント: ゲスト例外 (アクセス違反、不正命令、int3、ゼロ除算、hlt) は x86 コアが止まった
   時点で、Wine や sake の SEH に渡る前に、3 と同じ詳しさで記録します (スレッドごとに最初の 16 回と
   その後 100 回に 1 回。それ以外は 1 行)。それが JIT 中かインタプリタ中かも書きます。プロセスの生成と終了も。
5. Wine の stdout / stderr の全部 (err: / fixme: など。時刻とプロセス・スレッド付き)。
6. ページのログ: 状態表示、メモリ (wasm / JS ヒープ、30 秒ごと)、表示状態の変化、エラー。
   10 秒ごとの perf 行と、画面に出ているもの (`screen 960x540: 87% lit, mean …`、真っ黒なら
   `(black)`)。起動中 (wasm が動く前) は 10 秒ごとに `starting` 行とネットワークの数字。
   レポートを保存するときは、その時の画面の 160 ピクセル幅の JPEG (`screen image data:…`)。
7. 設定なしで常に出る要点 (stderr): スワップチェーン / Direct3D 9 デバイスの作成 (サイズ、形式、
   ウィンドウとそのクライアントサイズ)、1・60・600 回目の Present の行き先、DXGI サーフェスの
   D2D ビットマップ (最初の 16 個)、開けなかったファイル (最初の 64 個)。
8. 端末: user agent のほか、タッチ点数、userAgentData の platform / mobile、ポインタの種類
   (スマホが「PC 版サイト」で開くと Mac の user agent になるため)。

時刻はすべて UTC の `hh:mm:ss.mmm`。`p<pid>/t<tid>` は kura のプロセスとスレッドです
(Wine のスレッド ID は stderr 行の先頭の 4 桁 16 進)。

## しくみと負荷

wasm 側は `crates/kura/src/crashlog.rs`。イベント (512 KiB) と stdout/stderr (512 KiB) のリングバッファ、
スナップショット領域 (64 KiB) を静的メモリに持ち、ページ (`web/src/crashlog.ts`) が線形メモリから直接読みます。
wasm インスタンスが abort / trap しても読めます。書き込みは `fetch_add` で場所を取るだけなので、
スレッドが混ざっても行は崩れません。クラッシュ経路では確保をしません (別スレッドが malloc のロックを
持ったまま死んでいても固まらない)。

普段の負荷: syscall ごとに 4 ワードの記録、ゲストコードの出入りで 2 ストア、stderr の書き込みのコピー、
ページは 0.5 秒ごとにカウンタを 1 回読み、10 秒ごとに OPFS へ書くだけです (スナップショットは
保存のときだけ)。

確認用: `host/tests/crashtest.c` (null 書き込み、`ud`、`div`、`throw`、`handled`)。
