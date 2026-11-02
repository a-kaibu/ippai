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
| `crates/vnvm` | `vnvm` 1本のCLI。`serve`: web/dist とストアを COOP/COEP 付きで配信、JIT リージョンの保存、ゲーム一覧、セーブの S3 バックアップ、実行ログの保存 (`--logs`)。`pack`: ディレクトリをチャンク (256KiB、zstd) のストアにする。`add-game` / `remove-game` / `meta`: ランチャーの一覧と、批評空間・VNDB からのブランド・発売日・評価・タグ・紹介文・カバー |
| `host/` | emscripten の JS ライブラリ (`pre.js`、`vnfs.js`、`vnjit.js`、`vndl.js`)、生成スクリプト、テスト用プログラム (`tests/`) |
| `web/` | Vite+ (`vp`) のフロントエンド (TypeScript、UI は React + Tailwind CSS v4 + shadcn/ui、`src/components/ui`): `index.html` (= `wine.html`、ゲーム一覧)、`game.html`、`bench.html`、`logs.html` (実行ログ) |
| `tests/` | ヘッドレス Chromium での確認 (`wine.mjs`)、Android 相当に遅くして測る (`android-like.sh`)、コーパス |

## ビルドと起動

```sh
crates/sake/build.sh              # sake.js / sake.wasm と web/public/sake/
cargo build --release -p vnvm
(cd web && vp install && vp build)
target/release/vnvm --store store pack ~/prefix32 prefix32   # sake の C:\ (フォント)
target/release/vnvm serve --web web/dist --store store --listen 127.0.0.1:8080
```

sake は今のところ、Wine で作った 32bit prefix (`prefix32`、作り方は vnvm-wine の
`hangover/make-prefix32.py`) の `drive_c` をフォントと、sake にまだない DLL (d2d1、dwrite、
d3dcompiler_47 など) のために使います。ビルド済みのものが共有フォルダの `vnvm-env/` にあります。
Direct3D 10/11 のシェーダーは DXBC から WGSL へ直接翻訳します (`crates/vnhost/src/d3d11/wgsl.rs`)。

## 制約

* SharedArrayBuffer が要るので COOP/COEP ヘッダ付き (cross-origin isolated) で配信すること。
* x86 は 32bit のみ。描画は WebGPU のみ (WebGL2/Canvas2D へのフォールバックはしません)。
* DRM (PlayDRM、ソフト電池など) の解析や回避は扱いません。
