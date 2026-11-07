# x86 翻訳器の作り直し (2026-11-01〜)

本編の CPU は bench.exe のようなループ中心のコードでは直接 wasm の 1.1〜1.3 倍まで来たが、
スクリプト VM のような「フラット」なコード (小さな関数・分岐・呼び出しが平らに広がる) では
桁違いに遅い。ゲームの起動・シーン切り替え・スクリプトの多い場面はこちらの形をしている。

## 計測: flatbench (Lua 5.4 + SQLite)

`flatbench.c` (Lua のスクリプト 5 種: 再帰 fib、表、文字列、比較関数つきソート、オブジェクト生成と、
SQLite のメモリ上 DB に 3 万行入れて検索・集計) を同じソースから 3 通りにビルドした。1 回あたり:

| | 秒 |
|---|---|
| ネイティブ (vnbox の Wine、32bit、このマシン) | 0.73 |
| 直接 wasm (emcc -O2、node) | 1.0 |
| JIT (node) | 8.0〜9.0 |
| JIT (Chromium 153 headless) | 7.8〜9.1 |

目標 (こた、2026-11-01): Chromium で 1 回 2 秒以内。

ゲームのスレッドの 80% が翻訳したコードの中 (Wine の wasm 側は 10%)。

### 何に時間を使っているか

- 制御の移り方 (`IPPAI_JIT_COUNT=<秒>` のカウンタ、1 秒あたり): 翻訳したブロックの実行が約 1.7 億
  (1 ブロック平均 2〜3 命令、1 ブロック約 6ns、ネイティブは約 1.5ns)、リージョンへの入口 2,200 万、
  ハッシュ表を引く間接ジャンプ・戻り 1,500 万、リージョン間の乗り換え 1,300〜2,700 万。
- V8 の出した機械語のサンプル (perf + `--perf-prof`、翻訳したコードの分だけ):
  - レジスタ間の mov/lea 40%
  - スタックへの退避・読み戻し 23%
  - 比較・分岐 18%
  - ゲストのメモリの読み書き 6%
  - 関数の入口 (先頭 0x60 バイト) 16%
- 上限を測った実験 (どれも同じ条件で A/B):
  - 範囲チェックを全部外す (`IPPAI_JIT_NOCHECK=1`、測定専用): -7%。
  - 保存リージョン (1 モジュール) だけで走らせる: 差なし。
  - リージョンの大きさの上限を 150〜3000 命令に: 差なし。
  - call/ret を wasm の call/return にする (`IPPAI_JIT_CALLS=1`、下記): 差なし。bench.exe の sort は
    0.9 → 2.7 秒 (qsort ↔ 比較関数がマージされなくなる)。
  - 出口の多いリージョンをその後走ったブロック込みで作り直す: 約 200 回作り直して遅くなった (外した)。
- マイクロベンチ (/tmp の hop、node): ゲストの call+ret 1 組は、今のリージョン間の末尾呼び出し
  (16 引数の return_call_indirect + ハッシュ表) で 16ns、wasm の call で 3.7ns、別モジュールへの
  call_indirect で 4.7ns。呼び出し先のモジュールが多いほど遅い (Chromium: 1 個 5ns、16 個 22ns、
  1024 個 96ns)。ただし flatbench では 1 モジュールにしても変わらなかったので、主因ではない。

結論: 遅さの主因は乗り換えの回数ではなく、**出している wasm の形**。リージョンは x86 の 8 レジスタ・
遅延フラグ 5 個・予算・Cpu・入口番号などが全部生きたままの大きな関数で、ブロックの境目ごとに
それらが合流する。TurboFan のレジスタ割り当てがこれをさばけず、移動と退避が仕事の 10 倍になる。
直接 wasm の C の関数は小さく、生きている値が少ないので速い。

## このコミットで入れたもの (既定の動作は変わらない)

- リージョン関数の型を「16 引数 → 16 戻り値」に (結果の並びは引数と同じ、先頭が状態)。 (2026-11-06: `IPPAI_JIT_CALLS` と一緒に戻して、戻り値は状態だけ)
  `RET_VALUES` を返したら状態は戻り値にある (EIP は 2 番目)。それ以外は今まで通り Cpu にある。
  ディスパッチャは `RET_VALUES` を Cpu に書いて続ける。
- `IPPAI_JIT_CALLS=1` (直接 call)、`=2` (間接 call も): 他のリージョンへの CALL を呼び出し先の
  リージョンの call_indirect にし、RET は状態を戻り値で返す (`RET_VALUES`)。差が出なかったので
  2026-11-06 に削除 (関数単位の翻訳が同じことをする)。
- `jmp [index*4 + 表]` の表の飛び先 (走ったもの) をリージョンに入れる (switch、インタプリタの
  オペコード分岐)。
- `IPPAI_JIT_COUNT=<秒>`: 生成コードが数える制御の移り方のカウンタ (call、ret、ハッシュ表、入口、
  ブロック、トップ/サイドのディスパッチ)。`IPPAI_JIT_STATS=1` ではリージョン間の乗り換えの回数と
  多い組 (`vnjit hop pairs`) も出る。
- `IPPAI_JIT_NOCHECK=1`: 範囲チェックを出さない (測定専用、安全でない)。

## 新しい翻訳器 (方針)

こたの指示 (2026-11-01): 「V8 に最適化された wasm を出力する Rust の wasm コンパイラを作成せよ。
パフォーマンス第一、徹底的にチューニング」。

1. **x86 → SSA の中間表現**。ゲストのレジスタ・フラグは IR の値で、ブロック引数で合流する。
2. **最適化**: フラグの生存解析を関数全体で (使われないフラグは計算しない、CALL/RET では死んで
   いるとみなすのは手書き asm に備えて選べるように)、定数・コピーの伝播、冗長なロードと範囲チェックの
   削除 (同じ基底の近いアクセスは 1 回)、ESP 相対のスタックの値をレジスタに (別名が無いと分かる範囲)。
3. **V8 向けの出力**: 関数単位 (ゲストの関数 = wasm の関数、call は wasm の call)、生きている値だけを
   ローカルに、構造化した制御 (トップのディスパッチを経ない)、関数の入口で受け渡すのは必要なレジスタ
   だけ。合流点で生きている値を最小にする。
4. **段階**: まず IR とその上の出力を、今の翻訳器と同じ入口・出口の約束で 1 リージョン単位に差し替えて
   flatbench と bench.exe で比べる。次に関数単位の呼び出し。各段で V8 の機械語の移動・退避の割合を
   測って次を決める。

## 関数単位の翻訳 (`IPPAI_JIT_FN`、2026-11-01、2026-11-02 から既定で有効。`IPPAI_JIT_FN=0` で従来のリージョン)

- 翻訳の単位をゲストの関数 (呼び出し先、または入口がミスしたアドレス) にした。1 単位が wasm の
  関数 `(cpu, block) -> status` 1 つで、状態は入口と出口で Cpu にある。表には単位ごとに
  リージョンの呼び出し規約のアダプタ (f0) と本体 (f1) を置き、ハッシュ表の項目に `FN_TAG` を付ける。
- CALL はハッシュ表を引いて `call_indirect` (呼び出し先も関数)、RET は `RET_FN` を返す。呼んだ側は
  EIP が戻りアドレスなら続け、違えば (longjmp、SEH、深すぎ、途中で抜けた) 状態をそのまま上へ返す。
  RET はステータスフラグを Cpu に書く。CALL は戻り先がフラグを読むときだけ先に書く (手書きの
  コードは結果をフラグで返す: Delphi RTL の GetDynaMethod は ZF。これを死んでいるとみなして
  Reunion が初回起動で壊れた。intops.c の flgret)。
- 小さい直接の呼び出し先 (64 命令まで、call なし) は、走ったかに関係なく展開する。
- ブロックの並べ方は支配木から作る (Ramsey "Beyond Relooper"、`jit/structure.rs`): ジャンプは
  すべて開いているラベルへの `br`。`jmp [index*4 + 表]` は表ごとの分岐ノード 1 つ (`br_table`、
  読んだ飛び先が表の値と同じか確かめる)。既約にならない辺と単位に無いブロックへは関数から出る。
- 関数から出た先 (と関数の無い呼び出し先、`jmp [IAT]` の飛び先) はリングに書き、ホストが
  走ったブロックを足して作り直す (1 関数 8 回まで)、または呼び出し先を関数にする。
- 8/16 ビットの ROL/ROR を JIT に (フラグが死んでいるとき)。sqlite のバイト入れ替えが
  インタプリタに落ちていた。intops.c に rotate を追加。

flatbench (node、1 回あたり、3 回の 1/2/3 回目): リージョン 9.1 / 9.1 / 8.8 秒 →
関数単位 8.3 / 5.5 / 5.2 秒。1 回目は V8 のコンパイル待ちと作り直しが重い。bench.exe は
calls が同じ (0.12 秒)、sort (qsort の比較関数を関数ポインタで呼ぶ) が 0.84 → 2.2 秒、
crc32 が遅い。チェックサムは全部同じ。

その後 (Chromium): レジスタのマスク、呼び出しの深さを引数に、`mov r,[i*4+T]; jmp r` の
ハッシュ分岐、BSF/TZCNT/BT、x87 キャッシュを関数全体に、リングの間引き。呼び出し 1 回
15.5 → 7.1 ns (node)、flatbench は 1 回目 6.2–7 秒、2 回目以降 3.2–3.8 秒 (リージョンは
6.8–7.5 秒、ネイティブ 0.6–0.73 秒)。ただし実ゲームでは負ける: game.html (Reunion、
40 秒、Chromium) で関数単位はタイトル 13.3–13.5 秒 / ストーリー 0.35 コア、リージョンは
12.3–12.7 秒 / 0.29–0.31 コア。既定にするのはこれを逆転させてから。
  原因はコンパイル量: 初回 50 秒で関数 約 9000 回・計 10 秒 (翻訳 98 万命令)、リージョンは
  3200 回・2.4 秒 (12 万命令)。呼び出し先ごとに別の関数になり、小さい呼び出し先を
  走ったかに関係なく展開するため。`IPPAI_JIT_FN_CALLED=1` (呼ばれた所だけ関数) でタイトル
  22 → 16 秒。段階式 (リージョンを先に作り、よく呼ばれる呼び出し先だけ関数に上げ、
  リージョンから関数を直接呼ぶ) も試したが、リージョンと関数の行き来が高く flatbench が
  7.3–9.5 秒 (リージョン以下) になったので戻した。`IPPAI_JIT_COUNT` が 1 秒あたりの
  コンパイル回数と時間も出す。関数はスレッド間で共有する (効果は小さい)。
- 関数も保存する (index "VNJ2": 項目ごとに build と間接呼び出し箇所、関数は表の 2 枠 =
  アダプタと本体。アダプタは本体を `call_indirect` で次の枠として呼ぶので、他のモジュールと
  まとめても番号がずれない)。Reunion 2 回目: タイトル 23 → 17.7 秒、ストーリー 0.44 →
  0.375 コア (リージョンは 13.7 秒 / 0.34)。保存サイズはリージョンの約 5 倍 (66 MB 対 14 MB)。

- コンパイル量をさらに削る (2026-11-02): 呼ばれていない所 (関数の途中) から作る関数は、
  コンパイル済みのブロックを含めない (d5140ff)。さらに関数に複数の入口を持たせた
  (f5e06e1): 戻り先と外側のループ先頭は、先頭の分岐ノード (`br_table`) から入れる。
  ハッシュ表のブロック欄は starts の何番目か (0 が先頭) で、アダプタが深さ引数の上位
  16 ビットで渡す。呼び出しは先頭にしか行かない。ループの内側は入口にできない (既約で
  なくなる) ので、そこからの分は今も別の関数になる。呼び出しのレジスタ保存は、前回の
  読み込み以降に書いたものだけ (5187716、効果は小)。
  Reunion 初回 (100 秒): コンパイル 8100 → 5600 回、タイトル 25 → 14.5 秒前後 (リージョン
  14.3–14.7 秒)。本編の CPU は全体 0.34–0.36 対 0.31–0.33 コア、最後の 20 秒は 0.26–0.27
  対 0.25–0.26 (序盤のコンパイルの差が残る)。flatbench (Chromium) は 1 回目 5.1 秒、
  2 回目以降 3.5–3.7 秒。試してだめだったもの: インライン量を減らす (変化なし)、小さい
  関数や途中からの関数のコンパイルを遅らせる (インタプリタが増えて CPU 増)、途中への
  脱出のたびに元の関数を作り直す (作り直しが重くタイトル +2 秒)。

- 生成コードの改善 (2026-11-02): ジャンプテーブル分岐の前のフラグを不要と判定、
  `and r,imm`/movzx の直後の表引き [表 + r*s] は範囲チェックなし、64 ビット比較
  (cmp+sbb) の B/BE/L/LE と ADC の CF をホストの EFLAGS 計算なしに (fib −15%、sort −9%、
  `tests/cmp64.c`)、関数内でレジスタの範囲チェック済みをブロックをまたいで引き継ぐ
  (フレームポインタのチェックが関数に 1 回)。flatbench の perf では Worker の 95% が
  生成コードで、特定の関数に偏らない (最大は luaV_execute の命令分岐ループで約 8%)。
  試してだめだったもの: 呼び出しでレジスタと予算を wasm の引数・複数戻り値で渡す
  (Cpu を経由しない)。V8 は 6 個を超える引数と複数の戻り値をスタックに置くので速く
  ならず、呼び出し 1 回のコードは 261 → 286 バイトに増えた。

- 自己書き換え: JIT したページを `CODE_PAGES` に記録し、mprotect (PROT_WRITE)、munmap、
  MAP_FIXED の mmap、NtFlushInstructionCache (`vnhost_jit_code_changed`) がそこに触れたら
  世代を上げ、各スレッドの表を捨てる。`tests/smc.c`。
- 誤翻訳の二分探索: `IPPAI_JIT_ONLY=lo-hi` (範囲外はインタプリタ)、`IPPAI_JIT_FN_ONLY=lo-hi`
  (範囲外はリージョン)。初回だけ壊れる不具合は、保存済みの `_shared-<版>` を消してから試す。

## Android: V8's bounds checks (2026-11-02)

Android's Chrome cannot enable V8's trap handler, so every wasm load and store gets an explicit
bounds check (docs/v8-wasm-shape.md). Measured on desktop Chromium with
`--js-flags=--wasm-enforce-bounds-checks` (flatbench, FN mode, median rep):

| | bounds checks | desktop |
|---|---|---|
| before | 7.05 s | 3.5 s |
| JIT modules import memory with minimum = wine.wasm's initial size (9527650) | 5.1 s | same |
| memory never grows: 65535 pages fixed, modules import min = max | 3.8-4.1 s | same |

A growable shared memory's size is loaded again for every access; a fixed one's once per function.
wine.wasm (and sake) now start with the whole 4 GB (-sINITIAL_MEMORY=4294901760
-sALLOW_MEMORY_GROWTH=0). **To check on a real Android device at the final check**: V8 commits the
initial memory read-write up front; Android's overcommit heuristics could refuse 4 GB on a device
with little RAM (2.06 GB worked before). If it does, go back to a growable memory with the minimum
at 33792 pages (5.1 s above).

### The fallback: sake-grow.wasm (2026-11-07)

crates/sake/build.sh links sake.wasm twice from the same objects: `sake.wasm` (4 GB fixed, as
above) and `sake-grow.wasm` (-sINITIAL_MEMORY = koji::jit::wasm::GROW_MIN_PAGES = 33792 pages,
2.06 GB: the guest's low 2 GB and 64 MB for sake; grows to 4 GB). The page (web/src/runtime.ts
`sakeBuild`) tries `new WebAssembly.Memory({initial: 65535, maximum: 65535, shared: true})` itself
and hands it to sake.js (`wasmMemory`, so it is reserved once); when the browser refuses it, it
loads sake-grow. The JIT's modules declare `memory_min_pages()` (decided at the first module from
the memory's size: 65535, or 33792 for sake-grow) to 65535, and their saved regions go by the
translator's version + `g` (they would not link the other way round). `&memory=grow` forces it;
`&grow=<MiB>` (tests) mallocs that much mid-run so that the memory grows. The run log's `build:`
note says which ran.

flatbench, desktop Chromium (Playwright's), FN mode, ms, 5 runs:

| | default | `--wasm-enforce-bounds-checks` (Android-like) |
|---|---|---|
| sake.wasm (4 GB fixed) | 2511 2783 2685 2559 2647 | 3178 3031 3188 3093 3023 |
| sake-grow.wasm | 2580 2597 2494 2668 2516 | 3401 3422 3401 3382 3382 |
| sake-grow.wasm, grown 512 MiB mid-run | 2621 2534 2698 2666 2806 | 3444 3277 3400 3421 3394 |

The fallback costs ~10 % with bounds checks, nothing without; node 22's V8 shows no difference
(2.75 s both with checks). host/tests pass on the node sake-grow.js too; bench group
`memory-grow` runs kernels on it (grown mid-run) with the same checks as cpu-game. Still open, on
the real device: whether 4 GB is refused there at all (the run log tells), and the memory a long
play uses.

## FN モードを既定に (2026-11-02)

flatbench (Chromium、中央値): FN 3.4 s / リージョン 9.0 s。範囲チェックあり (Android 相当) で 4.6 / 10.8 s。
Reunion (キャッシュなし): FN タイトル 15〜17 s・本編 0.35〜0.37 コア、リージョン 14〜16 s・0.32〜0.36 コア (ノイズの範囲で
ほぼ同じ)。直接呼び出しだけの合成ベンチ (cb.exe) はリージョンの方が速い (リージョンは呼び先を合併する)。

## Freeing modules nothing refers to (2026-11-07)

Each load (a region, a function's adapter and body, a batch of functions) is a module of its
own, referenced only by this thread's table. A merge, a function built again, or a function
taking a region's blocks leaves the old one unreachable: every jump into translated code reads
its table index from the hash table or the call cache (koji never bakes one into code). Every
64 loads (`IPPAI_JIT_RECLAIM`, 1 = every load, 0 = never) kura's `reclaim` marks the slots those
two name and clears the slots of loads none of whose slots is named (`vnvm_jit_free`), so V8 can
collect the module. The saved bundle stays (one instance; its regions are entered on demand).
Slots are not reused. Counts: `IPPAI_JIT_COUNT` (`freed N (KiB)`) and the crash log's
`jit modules` line. flatbench reps=3: 869 modules loaded (8.4 MB), 105 freed (1.5 MB).
host/tests pass with `IPPAI_JIT_RECLAIM=1` (CI runs them so on sake-grow).
