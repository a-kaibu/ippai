# sake と Wine の比較 (性能)

2026-11-02。同じ JIT (koji) のソースから両方をビルドして比べた (wine.wasm と sake.wasm は
koji を静的に含む)。互換性 (コーパスで何本動くか) は sake スレッドの担当で、
結果は `/mnt/project-files/sake/corpus.md` に出る予定。最終的な選択はそれと合わせて決める。

計測マシン: 4 コアのクラウドコンテナ、HeadlessChrome 153。「Android 相当」は
`--js-flags=--wasm-enforce-bounds-checks` (Android の V8 にはトラップハンドラが無く、
すべてのメモリアクセスに範囲チェックが入る)。数字は複数回の範囲か中央値。

## 結果

| 項目 | Wine | sake | 備考 |
|---|---|---|---|
| Reunion タイトル到達 (冷えた JIT) | 16.2〜16.9 秒 | 12.2〜13.1 秒 | 833184c、各 3 回 |
| Reunion 本編 CPU (40 秒、コア) | 0.357〜0.366 | 0.330〜0.334 | 同上。ネイティブ 0.18 |
| 同 Android 相当: タイトル | 17.5〜18.3 秒 | 13.3〜13.7 秒 | Wine 2 回、sake 3 回 |
| 同 Android 相当: 本編 CPU | 0.406〜0.412 | 0.356〜0.364 | |
| Reunion 2 回目 (保存した JIT): タイトル / CPU | 15.7 秒 / 0.346 | 11.9〜12.0 秒 / 0.316〜0.328 | fd3dc82 |
| flatbench (Chromium、1 回あたり中央値) | 3.18 秒 | 2.46 秒 | fd3dc82、各 5 回 |
| flatbench Android 相当 | 4.00 秒 | 3.76 秒 | |
| flatbench (node、3 回平均の 1 回あたり) | 4.40〜4.68 秒 | 3.47〜3.56 秒 | チェックサム一致 (ネイティブとも) |
| bench.exe 合計 (node) | 0.87〜1.08 秒 | 0.73〜0.84 秒 | 項目ごとのチェックサム一致 |
| HeapAlloc+HeapFree (node) | 122〜130 ns | 22〜24 ns | sake はヒープをゲストの x86 で持つ |
| malloc+free (node) | 172〜185 ns | 36〜49 ns | |
| 関数呼び出し (callb) | 4.6〜5.4 ns | 5.1〜8.6 ns | 同じ JIT なので差はノイズ |
| 配るファイル | wine.wasm 6.2 MB + wineunix.so 3.3 MB + Wine の PE DLL (ストア) | sake.wasm 7.0 MB (+ d3dx9 など一部の PE DLL) | 非圧縮 |

## 比べて見つかったこと (直したもの)

- sake の qsort が比較ごとにホスト呼び出しで、bench.exe の sort が 6 倍遅かった。
  strcspn/strspn/strpbrk が未実装で SQLite の LIKE の結果が 1 ずれていた。
  sake スレッドが C ライブラリの一部をゲストの x86 コードにして修正 (4f3189b)。
  修正前の node flatbench は sake 7.0〜7.9 秒 / Wine 4.1〜4.9 秒で、修正後に逆転した。
- 32bit の JIT 保存が 6194c79 (メモリ固定) 以降働いていなかった (64bit スレッドが 3efb3d5 で修正)。
  上の「冷えた JIT」の数字は保存の有無に関係しない (毎回新しい JIT 名)。

## 読み方

- 性能はどの項目でも sake が同じか速い。差の大半は API 側 (ヒープ、C ライブラリ、
  起動処理) で、ゲストのコードの速さは JIT が同じなので変わらない。
- タイトル到達が 3〜4 秒短いのは、Wine のプロセス起動 (wineserver、explorer、DLL の読み込みと
  再配置) が無いため。Android 相当でも同じ傾向。
- 選択の残りの条件は互換性。Wine はコーパス 240 本中 143 本がタイトルまで
  (`/mnt/project-files/vnvm-corpus/corpus.md`)。sake の数字が出たら同じ表に並べる。

## 結論 (2026-11-02)

こた: Wine は廃止予定 (64bit スレッドで)。性能でも sake が全項目で同じか速いので、以後は sake を
主にする。JIT の計測 (flatbench の 2 秒目標など) も sake 上で行う (Chromium で 2.46 秒が出発点)。
