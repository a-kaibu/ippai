# vnvm

ビジュアルノベルをブラウザの中で動かすためのリポジトリです。ゲームの x86 (i386) コードは
x86→wasm JIT (`crates/vnx86`) で動かし、Windows API は Rust で一から書いた Win32 層
sake (`crates/sake`、[docs/sake.md](docs/sake.md)) が受け持ちます。表示・音声・ファイル・
Direct3D/OpenGL (WebGPU) はブラウザ側のホスト `crates/vnhost` です。

Wine 11.20 の Unix 側を wasm にする Hangover 方式の経路は 2026-11-02 に廃止を決め、実装・設計
メモ (docs/hangover.md など)・実験結果ごと [a-kaibu/vnvm-wine](https://github.com/a-kaibu/vnvm-wine)
に移しました (履歴もそちらに全部あります)。sake と Wine の比較は [docs/engine-compare.md](docs/engine-compare.md)、
先行研究の調査は [docs/prior-work.md](docs/prior-work.md)、翻訳器は [docs/translator.md](docs/translator.md)、
実行ごとの詳細ログ (クラッシュの原因調査用) は [docs/crashlog.md](docs/crashlog.md) にあります。

以前の QEMU 版 (フルシステム QEMU で Ubuntu 26.04 + Wine を起動する方式) は
[a-kaibu/vnqemu](https://github.com/a-kaibu/vnqemu) に移しました。

## ディレクトリ

| パス | 内容 |
| --- | --- |
| `crates/vnx86` | i386 CPU コア: インタプリタと x86→wasm JIT (ネイティブ CPU との差分テスト付き) |
| `crates/vnhost` | ブラウザ側のホスト: メモリ、スレッド、fd、ファイルシステム、表示、音声、Direct3D 9/11・OpenGL (WebGPU) |
| `crates/sake` | Win32 層 (kernel32、user32、gdi32、d3d9 など)。`build.sh` が sake.js / sake.wasm を作る |
| `crates/vnvm-pack` | ディレクトリをチャンク (256KiB、zstd) のストアにする CLI (`tree` / `add-game` / `remove-game`) |
| `crates/vnvm-server` | web/dist とストアを COOP/COEP 付きで配信、JIT リージョンの保存、ゲーム一覧、セーブの S3 バックアップ、実行ログの保存 (`--logs`) |
| `host/` | emscripten の JS ライブラリ (`pre.js`、`vnfs.js`、`vnjit.js`、`vndl.js`)、vkd3d-shader のビルド (`build-vkd3d.sh`)、生成スクリプト、テスト用プログラム (`tests/`) |
| `web/` | Vite+ (`vp`) のフロントエンド (TypeScript): `index.html` (= `wine.html`、ゲーム一覧)、`game.html`、`bench.html`、`logs.html` (実行ログ) |
| `tests/` | ヘッドレス Chromium での確認 (`wine.mjs`)、Android 相当に遅くして測る (`android-like.sh`)、コーパス |

## ビルドと起動

```sh
host/build-vkd3d.sh               # vkd3d-shader (Wine のソースの libs/vkd3d) を wasm に
crates/sake/build.sh              # sake.js / sake.wasm と web/public/sake/
cargo build --release -p vnvm-pack -p vnvm-server
(cd web && vp install && vp build)
target/release/vnvm-pack --store store tree ~/prefix32 prefix32   # sake の C:\ (フォント)
target/release/vnvm-server --web web/dist --store store --listen 127.0.0.1:8080
```

sake は今のところ、Wine で作った 32bit prefix (`prefix32`、作り方は vnvm-wine の
`hangover/make-prefix32.py`) の `drive_c` をフォントのために使い、vkd3d-shader のソースも Wine の
ソースツリーから取ります。どちらもビルド済みのものが共有フォルダの `vnvm-env/` にあります。

## 制約

* SharedArrayBuffer が要るので COOP/COEP ヘッダ付き (cross-origin isolated) で配信すること。
* x86 は 32bit のみ。描画は WebGPU のみ (WebGL2/Canvas2D へのフォールバックはしません)。
* DRM (PlayDRM、ソフト電池など) の解析や回避は扱いません。
