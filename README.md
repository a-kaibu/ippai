# vnvm

ビジュアルノベルをブラウザの中で動かすためのリポジトリです。vnbox と同じ Wine 11.20
(vnbox のパッチ入り) を使い、Hangover と同じ考え方で

* Wine の Unix 側 (ntdll.so、win32u.so、wineserver など) は emscripten で wasm32 にビルドし、
* ゲームと vnbox の i386 PE DLL だけを x86→wasm JIT で動かします。

Linux カーネルや X11 は動かしません。設計と計測結果は [docs/hangover.md](docs/hangover.md)、
先行研究の調査は [docs/prior-work.md](docs/prior-work.md) にあります。

以前の QEMU 版 (フルシステム QEMU で Ubuntu 26.04 + Wine を起動する方式) は
[a-kaibu/vnqemu](https://github.com/a-kaibu/vnqemu) に移しました。

## ディレクトリ

| パス | 内容 |
| --- | --- |
| `crates/vnx86` | i386 CPU コア: インタプリタと x86→wasm JIT (ネイティブ CPU との差分テスト付き) |
| `crates/vnhost` | ブラウザ側のホスト: メモリ、プロセス、fd、ファイルシステム、表示、音声、Direct3D 9 (WebGPU) |
| `crates/vnvm-pack` | ディレクトリをチャンク (256KiB、zstd) のストアにする CLI (`tree` / `add-game` / `remove-game`) |
| `crates/vnvm-server` | web/dist とストアを COOP/COEP 付きで配信、JIT リージョンの保存、ゲーム一覧、セーブの S3 バックアップ |
| `hangover/` | Wine の Unix 側の wasm ビルド (`build.sh`)、Wine ルートと 32bit prefix の作成、テスト用プログラム (`tests/`) |
| `patches/wine-wasm.patch` | Wine を wasm32 向けにビルドするための変更 |
| `web/` | Vite+ (`vp`) のフロントエンド (TypeScript): `index.html` (= `wine.html`、ゲーム一覧)、`game.html`、`bench.html` |
| `tests/` | ヘッドレス Chromium での確認 (`wine.mjs`)、Android 相当に遅くして測る (`android-like.sh`) |

## ビルドと起動

手順の詳細は [hangover/README.md](hangover/README.md) にあります。

```sh
hangover/build.sh                 # wine.js / wineunix.so と web/public/wine/
cargo build --release             # vnvm-pack, vnvm-server
(cd web && vp install && vp build)
target/release/vnvm-pack --store store tree --follow ~/wineroot wineroot
target/release/vnvm-pack --store store tree ~/prefix32 prefix32
target/release/vnvm-server --web web/dist --store store --listen 127.0.0.1:8080
```

## 制約

* SharedArrayBuffer が要るので COOP/COEP ヘッダ付き (cross-origin isolated) で配信すること。
* x86 は 32bit のみ。描画は WebGPU のみ (WebGL2/Canvas2D へのフォールバックはしません)。
* DRM (PlayDRM、ソフト電池など) の解析や回避は扱いません。
