# V8 が扱いやすい wasm の形 (先行研究と、vnvm の翻訳器への提案)

2026-11-02。こたの依頼: 「V8 にとって扱いやすい wasm の形を、実装・論文・個人ブログから幅広く調べて、
翻訳器の出力構造の提案までまとめて」。

V8 の性質はブログや記憶ではなく **V8 main のソース (commit b685385、2026-11-01)** で確かめた。
ファイル名と行番号はこのコミットのもの (`src/` から)。vnvm の計測値は docs/translator.md、
docs/prior-work.md、vnvm-wine docs/hangover.md と、これまでのスレッドの記録から引いた。

## 要約

1. **Android の Chrome では wasm のメモリアクセスがすべて明示的な範囲チェックになる**。V8 の trap handler
   (ガードページで範囲チェックを省く仕組み) は Android では有効にできない作りになっている
   (`trap-handler/trap-handler.h:18-60`)。そのうえ vnvm のメモリは shared かつ伸長可能なので、
   チェックのたびにメモリの大きさを読み直す (`MemSize`、読み込みの省略が禁止されたロード)。さらに JIT の
   モジュールはメモリを最小 0 ページで import しているため、アクセスごとに比較が 1 つ余計に入る。
   主な対象は Android なので、これが一番効く可能性がある。デスクトップでも
   `--js-flags=--wasm-enforce-bounds-checks` で同じ状態を再現して測れる (提案 A)。
2. **V8 の呼び出し規約は、GP レジスタの引数が x64 で 5 個 (先頭はインスタンス)、arm64 で 6 個。戻り値は GP 2 個 +
   FP 2 個までレジスタで返る。callee-saved のレジスタは無い**。呼び出しをまたいで生きている値は全部
   スタックに退避される。`(status, esp)` の 2 値を返す形ならレジスタに収まる (提案 B)。
3. **x64 で割り当てに使える GP レジスタは 11 個、arm64 では 24 個**。今の関数は x86 のレジスタ 8 個、
   遅延フラグ 5 個、予算・入口・状態などを同時に生かしているので、x64 では必ず退避が出る (perf で
   退避と読み戻しが 23%、レジスタ間の mov が 40%)。合流点とループ先頭で生きている値を 9 個程度に
   抑えるのが、V8 向けの形を作るうえで一番大事な目標になる (提案 C)。
4. **Turboshaft の wasm パイプラインにはループ不変式の移動 (LICM) が無く、OSR も無い**。ループの展開は
   呼び出しも内側のループも無い 80 演算未満のループだけ。ループ先頭には毎回 V8 のスタックチェックが入る。
   Turboshaft はほぼ wasm の命令順のまま機械語にするので、命令の並べ方は出す側で決める (提案 D)。
5. **V8 のインライン化は同じインスタンスの中だけ**。call_indirect の投機的インライン化も、呼び先が同じ
   インスタンスかを最初に確かめる。単位ごとに別モジュールにすると、V8 のインライン化は一切効かない。
   熱い関数を 1 つのモジュールにまとめ直し、既知の呼び先は直接 `call` にすれば、500 バイト以下の呼び先は
   V8 が自分でインライン化する (提案 E)。
6. CheerpX、v86、QEMU-wasm はどれも同じ壁に当たっている。v86 は「大きな br_table と関数全体で使う
   ローカル」の形をブラウザがうまく扱えないと書いている。CheerpX はゲストの関数を wasm の関数にして、
   CFG の構造化、return_call、分岐ヒント (7〜10%) で対処した。vnvm の FN モードは同じ方向にある。

## 1. V8 の中で wasm に起きること

### 段階

- **遅延コンパイル**: 関数は最初の呼び出しで Liftoff がコンパイルする (`--wasm-lazy-compilation`、既定で有効)。
- **Liftoff**: 1 パスのベースラインコンパイラ。IR を作らず、wasm を読みながらそのまま機械語を出す。
  値スタックの値をレジスタかスタックに置いて追いかけるだけ。合流点では状態を揃えるための移動と退避が出る
  ([Liftoff のブログ](https://v8.dev/blog/liftoff))。
  - ローカルを初めからスタックに置くかどうか: 引数以外のローカルがキャッシュ用レジスタの半分 (x64 では
    10 個の半分の 5 個) を超えると、全部スタックに置く (`wasm/baseline/liftoff-compiler.cc:1240`
    `SpillLocalsInitially`、`liftoff-assembler-defs.h:36`)。vnvm の関数は 20 個以上のローカルを持つので、
    Liftoff の間はローカルを触るたびにメモリを読み書きする。
  - 宣言したローカルは関数に入るたびにゼロで埋める (vnvm で 50 個 15ns を確認済み)。
  - 分岐ヒントは読まない。
- **動的な階層化**: Liftoff のコードは、ループの戻り辺と関数の出口で予算を減らす。減らす量は
  前回のチェックから実行した機械語のバイト数で、1 回のチェックで 20、関数の入口で 40 を足す
  (`liftoff-compiler.cc:715-723`)。関数ごとの予算 13M を使い切ると、裏のスレッドで Turboshaft に
  かける (`flags/flag-definitions.h:2083` `wasm_tiering_budget`)。1 回で予算の 1/4 より多くは減らさない。
- **OSR は無い**。`MaybeOSR` はデバッグ用のコードでしか働かない (`liftoff-compiler.cc:11292`)。
  関数の中で長く回るループは、その関数に入り直すまで Liftoff のままになる。vnvm は予算で関数から
  出るので問題になっていないが、関数の中に大きなディスパッチループを持つ形にすると効いてくる。
- **最適化コンパイラ**: wasm は全体が Turboshaft (CFG の IR) になった。Sea of Nodes をやめたことで
  コンパイル時間は半分になった ([Leaving the Sea of Nodes](https://v8.dev/blog/leaving-the-sea-of-nodes))。
  同じ記事によれば、Sea of Nodes は wasm の元の命令順を捨てていたが、それが Binaryen などの AOT ツールが
  「16 レジスタを想定して退避が減るように」並べた順を壊していた。Turboshaft はほぼ元の順のまま進む。
  つまり **命令の並べ方の責任は wasm を出す側にある**。
- **投機的最適化** (Chrome M137): Liftoff が呼び出し先のフィードバックを集め、call_indirect と call_ref を
  投機的にインライン化し、外れたら Liftoff のフレームへ deopt する。Dart のマイクロベンチで平均 1.59 倍
  ([Speculative Optimizations for WebAssembly](https://v8.dev/blog/wasm-speculative-optimizations))。
  関数ごとに deopt は 10 回までで、それを超えると deopt 点を作らない (`flag-definitions.h:2193`)。

### Turboshaft の wasm パイプラインの段 (`compiler/pipeline.cc:2970-3050`)

| 段 | 内容 | 条件 |
|---|---|---|
| グラフ作成 | wasm → Turboshaft、インライン化もここ | |
| LoopPeeling | ループの 1 回目を剥がす | **WasmGC を使うときだけ** |
| LoopUnrolling | 小さいループの展開、回数が分かる小さいループのスタックチェック削除 | |
| WasmGCOptimize | GC の型 | GC を使うときだけ |
| WasmLowering | wasm の演算を機械の演算に | |
| WasmOptimize | LateEscapeAnalysis、MachineOptimization、MemoryOptimization、BranchElimination、LateLoadElimination、ValueNumbering | |
| Int64Lowering | i64 を 32 ビット 2 つに | 32 ビットの環境だけ |
| WasmDeadCodeElimination | DCE、スタックチェックの展開、ロード・ストアの単純化、ValueNumbering | |
| WasmSimd | SIMD の最適化 | |
| 命令選択 → レジスタ割り当て → 機械語 | 線形スキャン (`compiler/backend/register-allocator.h:1535` `LinearScanAllocator`) | |

ここから分かること:

- **LICM の段が無い**。ループの中で毎回同じ値を読む・計算するコードは、そのまま毎回実行される。
  BranchElimination と LateLoadElimination が同じ条件や同じロードの繰り返しを一部消すだけ。
- LateLoadElimination は、間にストアがあると同じ番地かもしれないロードを消せない。Cpu の構造体は
  ゲストのメモリと同じ線形メモリにあるので、**ゲストへのストアを 1 回挟むと Cpu のフィールドは読み直しに
  なる**。関数の中で何度も読むもの (予算、セグメントの基底、x87 の TOP など) はローカルに置く。
- ループ展開の条件 (`compiler/turboshaft/loop-unrolling-reducer.h:196-284`): 部分展開は内側のループも
  呼び出しも無く、演算が 80 未満のループだけ (最大 4 倍、展開後 240 演算まで)。完全展開は回数が 4 回までと
  分かる 150 演算以下のループ。ループのスタックチェックを消すのは、回数が 5000 回未満と分かるときだけ。
  x86 のループは回数が静的に分からないので、**スタックチェックは基本的にループ 1 周ごとに残る**。
- ループ先頭の phi は、ループの中で代入されるローカルにしか作られない
  (`wasm/function-body-decoder-impl.h:2019` `AnalyzeLoopAssignment`)。使われない phi は DCE が消す
  (`dead-code-elimination-reducer.h`)。本当に問題になるのは、片方の経路でだけ書かれ合流後に読まれる値
  (遅延フラグの値など) で、これは消えない。

### 呼び出し規約 (`wasm/wasm-linkage.h`)

| | GP 引数 | FP 引数 | GP 戻り値 | FP 戻り値 |
|---|---|---|---|---|
| x64 | rsi(インスタンス), rax, rdx, rcx, rbx, r9 | xmm1–6 | rax, rdx | xmm1, xmm2 |
| arm64 | x7(インスタンス), x0, x2–x6 | d0–d7 | x0, x1 | d0, d1 |
| ia32 / arm32 | 4 (うち 1 つがインスタンス) | 6 / 8 | 2 | 2 |

- 先頭の GP 引数はインスタンスなので、**wasm の引数としてレジスタで渡せるのは x64 で 5 個、arm64 で 6 個**。
  それより後はスタックに置かれる。戻り値は 3 個目からスタック。これは translator.md の「6 個を超える
  引数と複数の戻り値はスタック」という観察を正確にしたもので、戻り値は 2 個までなら
  レジスタで返る。
- **callee-saved のレジスタは無い** (`compiler/wasm-compiler-definitions.cc:105` の
  `kCalleeSaveRegisters` は空)。呼び出しをまたいで生きている値は、呼び出しの前に全部スタックに退避され、
  後で読み戻される。ゲストの EBX/ESI/EDI/EBP を wasm のローカルに持ったまま呼んでも、Cpu に書くのと同じ
  コストがかかる。呼び出しのコストを本当に消すのはインライン化だけ。
- 呼び出しの無い関数 (葉関数) は、フレームが小さければ入口のスタックチェックが省かれる
  (`compiler/turboshaft/stack-check-lowering-reducer.h:84-102`)。

### 使えるレジスタの数 (Chrome はポインタ圧縮ありでビルドされる)

- x64: rax, rbx, rdx, rcx, rsi, rdi, r8, r9, r11, r12, r15 の **11 個**
  (`codegen/x64/register-x64.h:31`。r10 は作業用、r13 はルート、r14 は圧縮の基底)。
- arm64: x0–x15, x19–x25, x27 の **24 個** (`codegen/arm64/register-arm64.h:32`)。
- つまり **x64 のデスクトップで測ったレジスタ不足は、Android (arm64) では大幅に軽くなる**。逆に
  Android には範囲チェックの問題 (次節) があり、デスクトップの測定だけでは向きが見えない。

### メモリアクセスと範囲チェック

V8 はメモリごとに範囲チェックの方式を決める (`wasm/wasm-module.cc:37-50`):

- trap handler が使える環境 (x64 の Linux/Windows/Mac、arm64 の Linux/Windows/Mac) では、memory32 の
  アクセスは範囲チェックなし。範囲外はガード領域のフォールトで捕まえる。vnvm の計測で
  「メモリのロードと範囲チェックはほぼ無料」だったのはこの環境だったから。
- **Android では trap handler を有効にできない**。`trap-handler.h:55-60` は Android で有効にしようとすると
  ビルドエラーにする (「Android のシグナル処理と crash reporting の安全性の検討が必要」とコメントがある)。
  Android の Chrome では **すべてのロードとストアに明示的な範囲チェックが付く**。
- arm64 の Mac 以外 (Linux など) では、trap handler があっても 2 バイト以上のストアには明示的なチェックが
  付く (一部だけ範囲外のストアの扱い、`flag-definitions.h:2234-2242`
  `wasm_partial_oob_writes_are_noops`)。

明示的なチェックの中身 (`wasm/turboshaft-graph-interface.cc:8174` `BoundsCheckMem`、`8347` `MemSize`):

```
if (offset + size - 1 > 宣言した最小サイズ)  trap_if(offset + size - 1 >= メモリサイズ)   ← 最小 0 なら毎回
trap_if(index >= メモリサイズ - (offset + size - 1))
```

- メモリサイズの取り方: shared で伸長可能なメモリでは、**アクセスごとに読み込み省略を禁止したロードで
  読み直す** (`8358` `NotLoadEliminable`)。伸長しない (最小 = 最大) メモリでは、関数の入口で 1 回読んだ
  値を使う (`6731-6755`、`wasm-module.h:161` `can_grow()`)。
- vnvm の JIT のモジュールはメモリを「shared、最小 0、最大 65536 ページ」で import している
  (`crates/koji/src/jit/wasm.rs:423-427`)。最小が 0 なので、Android では 1 アクセスに比較 2 つと
  サイズのロード 1〜2 回が付く。wine.wasm 側は `-sALLOW_MEMORY_GROWTH=1` なので同じく伸長可能。
- memory64 は trap handler があっても上限との比較が 1 つ要る (`8252-8268`)。64 ビットのゲストでも
  wasm のメモリは memory32 のままがよい。
- wasm のアクセスの `offset` 即値は機械語のアドレス指定に畳み込まれる (x64 の base+index+disp、
  arm64 は index を uxtw で拡張して base+index、即値は別の加算)。`--turbo-wasm-address-reassociation`
  (既定で有効) が定数の加算を offset 側へ移す。

### インライン化 (`flag-definitions.h:2250-2273`、`turboshaft-graph-interface.cc:2745-3123`)

- 呼び先の本体が **500 バイト以下** (`wasm_inlining_max_size`)、関数全体の予算 5000 ノード、または元の大きさの
  3 倍まで。Liftoff が集めた呼び出し回数で順番を決める。Liftoff で走る前に Turboshaft に行った関数
  (`--no-liftoff` など) にはフィードバックが無い。
- 直接の `call` と `return_call` は同じモジュールの関数ならインライン化される。
- `call_indirect` / `return_call_indirect` は、フィードバックにある呼び先を、まず
  **呼び先のインスタンスが自分と同じか** を確かめてから展開する (`2869-2878`
  `kUnlikelyCrossInstanceCall`)。インポートした関数はインライン化しない。vnvm のように単位ごとに
  モジュールとインスタンスを作り、共有のテーブルで呼び合う形では、どの呼び出しも展開されない。

### 起動とコンパイルの量

- Turboshaft の時間は本体の大きさにほぼ比例し、vnvm の計測で本体 1KB あたり 1.26ms (prior-work.md)。
  範囲チェックの出口 (bail) が Turboshaft の時間の約 2 割。
- モジュールを大量に作るとコード領域が散らばり、別モジュールへの呼び出しが遅くなる
  (vnvm-perf-notes: 4 モジュール 4ns、512 モジュール 190〜370ns)。
- コードキャッシュは compileStreaming、128KB 以上、Turboshaft のコードだけ (prior-work.md)。
- compilation hints (`metadata.code.compilation_priority`、`metadata.code.instr_freq`、
  `metadata.code.call_targets`) は V8 に実装はあるが、まだ実験フラグの裏 (`flags/feature-flags.h:109`)。
  出荷されれば、サーバー側で集めた呼び先を `call_targets` で渡してインライン化させられる。
- 分岐ヒント (`metadata.code.branch_hint`) は出荷済み。Turboshaft は読み、Liftoff は読まない。

## 2. 先行実装の出力の形

| | 単位 | 制御 | 状態 | 学べること |
|---|---|---|---|---|
| **CheerpX** (Leaning Technologies) | 熱い入口から直接ジャンプで届く範囲を 1 関数 | CFGStackifier で構造化。直接・間接の call は wasm の call、戻りは「行儀のよいスタック」を前提に wasm の return、外れたときだけ別経路。間接ジャンプは return_call (スタックが溢れるため) | 解釈実行でプロファイル → 熱いトレースを JIT | ゲストの関数 ≈ wasm の関数。PLT の `jmp [GOT]` は「ほぼ直接」として devirtualize。分岐ヒントで 7〜10%。perf + jitdump に元の x86 を載せて機械語レベルで見る |
| **v86** | 熱いページとそこから届く数ページを 1 関数 | 全入口を巨大な br_table で受け、残りを stackifier | レジスタは線形メモリ、遅延フラグ | 「深い br_table と関数全体で使うローカルの形を、ブラウザはうまく扱えない」(how-it-works.md)。MAX_PAGES を小さくせざるを得なかった。v86 の負の結果がそのまま vnvm の region モードの問題 |
| **QEMU-wasm** (ktock、TCG wasm backend パッチ) | TB 1 つを 1 モジュール | TCI で走らせ、多く走った TB だけ wasm に | QEMU の env | 「ブラウザは数千のモジュールを作れない」。vnvm の QEMU 版でも 1 TB 1 モジュールは遅く、まとめて 1 関数にして改善した (vnvm-perf-notes) |
| **elfconv** | ELF 全体を AOT (LLVM 経由) | LLVM → wasm | | AOT で関数単位にすれば LLVM と Binaryen の最適化が全部効く。動的コードには使えない |
| **wemu** (44670) | インタプリタ + HLE | | ゲストのアドレスを wasm のアドレスとしてそのまま使う (fastmem) | Win32 を Rust で直接実装。vnvm の sake (フルスクラッチの Win32 層、旧称 sake) と同じ発想。CPU 側の参考は少ない (以前の調査どおり) |
| **Box64** | 動的ブロック (BIGBLOCK で大きく) | ネイティブのジャンプ | ゲストのレジスタをホストのレジスタに固定、遅延フラグ (DF)、ネイティブのフラグ (NATIVEFLAGS) | CALLRET (ジャンプ表を飛ばす call/ret)、SAFEFLAGS (CALL/RET でフラグは死んでいるとみなす。既定は「RET の多くはフラグが要る、CALL は要らない」)。vnvm が Delphi の RTL で踏んだ問題と同じ所に既定値を置いている |
| **FEX / Rosetta 2** | 関数・複数ブロック | ネイティブ | ホストのレジスタに固定 | wasm ではレジスタを固定できないので、そのままは使えない。代わりに「生きている値の数を減らす」ことで同じ効果を狙う |
| **Emscripten / Binaryen / Cheerp** | C の関数 | Relooper / Stackifier | ローカル | 直接 wasm が速いのは、関数が小さく、生きている値が少なく、呼び出しが直接の call で、V8 がインライン化できるから |

## 3. 論文と記事

- Jangda ら "Not So Fast: Analyzing the Performance of WebAssembly vs. Native Code" (USENIX ATC 2019、
  [arXiv:1901.09056](https://arxiv.org/abs/1901.09056)): Chrome の wasm はネイティブより平均 55% 遅い。
  原因はロード・ストアの増加 (レジスタ不足による退避)、分岐の増加 (スタックチェック、間接呼び出しの
  チェック)、コードの増加。vnvm の perf の内訳 (退避 23%、関数の入口 16%) と同じ種類。
- Titzer "Whose baseline compiler is it anyway?" (CGO 2024、[arXiv:2305.13241](https://arxiv.org/abs/2305.13241)):
  Liftoff を含む 1 パスコンパイラの仕組みと比較。1 パスコンパイラは合流点でレジスタの状態を揃えるのが
  弱点で、ローカルの数と合流点の数がコードの質を決める。
- Ramsey "Beyond Relooper" (ICFP 2022): 支配木から構造化する方法。vnvm の `jit/structure.rs` が使っている。
- Pignotti "Extreme WebAssembly 1/2" (Leaning Technologies、2020)、"Branch Hinting" (同):
  CheerpX の構造、tail call の必要性、分岐ヒント。
- [v86 how-it-works.md](https://github.com/copy/v86/blob/master/docs/how-it-works.md): 上の表のとおり。
- [V8 ブログ](https://v8.dev/blog): Liftoff (2018)、dynamic tiering (2022)、tail calls (2023)、
  Leaving the Sea of Nodes (2025)、Speculative Optimizations (2025)。
- QEMU の TCG wasm バックエンドのパッチ (Kohei Tokunaga、2025、
  [cover letter](https://patchew.org/QEMU/cover.1756216429.git.ktokunaga.mail@gmail.com/)):
  TCI を既定にして、多く走った TB だけを WebAssembly.Module にする。
- 既に prior-work.md で扱ったもの (Dynamo、HQEMU、Hiser ら、Lasagne、Risotto、FEX、Rosetta 2) は省く。

## 4. vnvm の翻訳器への提案

効きそうな順。どれも「出す wasm の形」だけで、V8 側の変更は要らない。

### A. Android の範囲チェックを軽くする (主な対象なので最優先で測る)

1. **JIT のモジュールのメモリの import の最小を、作った時点のメモリのページ数にする**
   (`wasm.rs` の `0x02, 0x03, 0x00` の 0 を現在のページ数に)。import の検査は「実際の大きさ ≥ 宣言の最小」
   なので、作った時点の大きさなら必ず通る。保存したモジュールは、wine.wasm の初期メモリ
   (`-sINITIAL_MEMORY=2214592512` = 33792 ページ) を最小にすれば、どの起動でも通る。
   これで Android ではアクセスごとの比較が 1 つ減る。デスクトップには影響なし。
2. **メモリを伸長しない形にする実験** (wine.wasm を `-sINITIAL_MEMORY=<最大>` で
   `ALLOW_MEMORY_GROWTH` なし、JIT のモジュールは最小 = 最大で import)。サイズのロードが関数の入口で
   1 回になる (その値も関数の間ずっと生きる値になるが、arm64 ではレジスタに余裕がある)。shared のメモリはもともと最大まで仮想アドレスを予約するので、仮想アドレスの使用量は
   変わらない。実際に触ったページだけが物理メモリになるはずだが、Android の Chrome が大きな初期メモリを
   拒まないかは実機で確かめる必要がある (最後の確認として)。
3. **測り方**: デスクトップの Chromium で `--js-flags=--wasm-enforce-bounds-checks` を付けると、
   Android と同じ明示的なチェックになる。CPU のスロットリングと組み合わせれば、実機なしで
   1. と 2. の効果を測れる (Android で測らない、というルールに合う)。今の差 (付ける / 付けない) を
   最初に測れば、Android で範囲チェックがどれだけの割合かが分かる。
4. 自前の範囲チェック (GUEST_LO..HI) は、wine.wasm の領域 (上位 2GB) を守るために必要なので残る。
   V8 のチェックとは別物で、まとめられない。ただし Android ではこちらも明示的なチェックと同じ重さに
   なるので、「同じ基底のチェックは 1 回」(addr_pair、safe 追跡) の効き目は Android の方が大きい。
5. 確認事項: 対象の端末の Chrome が 64 ビットか (`chrome://version`)。32 ビットの Chrome では i64 が
   全部 32 ビット 2 つに分解され (Int64LoweringPhase)、レジスタも 8 個前後になるので、前提が変わる。

### B. 関数の境界 (呼び出し規約) を V8 のレジスタに合わせる

- 引数は GP で **cpu を含めて 5 個まで** (x64)、戻り値は **2 個まで** に収める。
  例: `(cpu: i32, arg: i32, esp: i32) -> (status: i32, esp: i32)`。次の作業として挙がっている
  「esp を引数・戻り値に」は、この形ならスタックを通らない。3 個目の戻り値を足した時点で
  全部の戻り値がスタック経由になるわけではなく、3 個目以降だけがスタックになる。
- 前に試した「レジスタを全部引数と複数の戻り値で」(16 個) は、x64 では 11 個以上がスタックに載るので
  負けて当然だった。試すなら、呼び先が読む・呼び元が後で読むレジスタだけを、2 つずつ i64 に詰めて
  渡す形 (例 `eax | edx << 32`)。詰める・取り出すのは 1〜2 命令で、ストアとロードの組より安い可能性がある。
  ただし callee-saved が無いので、呼び出しをまたいで呼び元が持ち続ける値の退避は消えない。
  効果は小さいと見込むので、B はマイクロベンチ (/tmp/claude-0/hb/callb.c と同じ形) で先に測る。
- FP の引数レジスタ (x64 で 6 個) は空いているが、GP との間の移動 (movq) が往復で数サイクルかかるので、
  ゲストの整数レジスタを載せるのは勧めない。x87/SSE の値を渡すなら使える。

### C. 合流点とループ先頭で生きている値を減らす (いちばん大事)

x64 で使えるレジスタは 11 個。そこに cpu、メモリの基底 (shared のメモリは動かないので、関数の入口で 1 回
読んだ値を関数の間ずっと持つ)、作業用が入るので、
ゲストの値として持てるのは 7〜8 個。合流点ごとに 15〜20 個の値が生きていると、Turboshaft の線形スキャンは
退避と移動を大量に出す (vnvm の perf で mov/lea 40%、退避 23%)。

1. **遅延フラグを「実行時の値」ではなく「翻訳時の状態」にする**。今は L_OP (どの演算か) を
   ローカルに持ち、合流点で L_OP、L_FA、L_FB、L_FR、L_FC の 5 個が phi になりうる。
   翻訳時には各点で「最後にフラグを立てた命令」が分かっている。合流する経路で同じなら L_OP は定数で、
   要るオペランドだけが生きる。経路で違うときだけ、合流の前にフラグの読み手が要る形 (例えば ZF だけなら
   結果 1 個、CF だけなら 0/1 の 1 個) にそろえる。CF/ZF/SF の読み手しかない合流点で 5 個を 1〜2 個にできる。
   まず `L_OP` が実行時の値として phi になっている合流点の数を、翻訳器に数えさせるとよい。
2. **比較はその br_if の直前で作る**。Turboshaft の命令選択は、1 回しか使われない比較とその分岐を
   1 組 (`cmp` + `jcc`) にする。比較の結果をローカルに置いて別のブロックで分岐すると、0/1 の値の
   計算と、その値の生存期間が増える。フラグの元の値 (ゲストのレジスタ) が変わっていなければ、
   読み手のブロックで比較を作り直す (再計算) 方が安い。
3. **ゲストのレジスタの生存解析を合流点に使う**。ある合流点の後、どの経路でも読む前に書かれるレジスタは
   死んでいる。死んだレジスタを合流点の前で書き換えない (ローカルに代入しない) と、Turboshaft の
   phi も生まれない。関数の出口や呼び出しのように Cpu に書く必要がある点だけ、生きているものとして扱う。
4. **合流点で使わない値は Cpu に置いたままにする**。セグメントの基底、x87、MMX/SSE、予算以外の
   カウンタなど、関数の中で一部でしか使わない値は、使うブロックで読み、使い終わったら書く。
   ただし同じブロックの中で何度も読む値はローカルに (ゲストへのストアで Cpu のロードは消せなくなるため)。
5. **作業用の値はローカルではなく wasm の値スタックで渡す**。Turboshaft にとっては同じだが、Liftoff は
   スタックの値をレジスタに持ち、ローカルは (6 個以上あると) メモリに置く。起動中の Liftoff の時間が減る。
6. 目安: 熱いループの先頭で生きている値を、x64 で 9 個以下。arm64 ならもっと持てるので、
   x64 で合わせておけば Android では退避がほぼ消える。`--print-wasm-code` で出た機械語の、
   ループ先頭の前後の `mov [rbp-..]` の数を数えると確かめられる。

### D. ループと命令の並べ方

1. **ループ不変の値は翻訳器が自分でループの外に出す** (V8 に LICM が無い)。ループの中で変わらない
   ベースレジスタの範囲チェック (今の safe 追跡をループの先頭に移す)、セグメントの基底、表の基底。
2. **ループの 1 周は小さく、呼び出しなしに**。80 演算未満で呼び出しも内側のループも無ければ、
   Turboshaft が最大 4 倍に部分展開する。展開されるとスタックチェックと予算の減算の割合が下がる。
   memcpy/strlen のような x86 のループ (rep movs、バイトのループ) はこの条件に入りやすい。
3. **予算の減算はループの戻り辺に 1 回だけ** (ブロックごとにしない)。V8 のスタックチェックも
   ループ 1 周に 1 回入るので、1 周あたりの固定費は「予算 + スタックチェック」の 2 つになる。
4. **値は使う直前に作る**。Turboshaft は wasm の命令順をほぼ保つので、遠くで計算してローカルに置いた値は、
   その間ずっとレジスタを占める。アドレスの計算は、そのアクセスの直前に置く。
5. 分岐ヒント: 出口 (bail、予算切れ、ハッシュ表の外れ) は unlikely に。Chromium 153 で
   既に試して差が出なかったが、合流点の値を減らした後で、コールドパスの退避がホットパスに
   出なくなる効果は変わりうる。C を入れた後で測り直す価値はある。

### E. インライン化を V8 にさせる (同じインスタンスにまとめる)

1. **熱い関数を 1 つのモジュールにまとめ直す** (HQEMU の考え方: 温まってから、まとめて作り直す)。
   既知の呼び先は、同じモジュールの中なら `call $f` (直接)、外なら今のとおり。V8 は 500 バイト以下の
   呼び先を自分でインライン化し、呼び出しの退避も消える。翻訳器の手でのインライン化 (今の 64 命令の規則)
   と役割を分けられる: 翻訳器は「呼び先がどこか」を決め、展開の判断は V8 に任せる。
2. まとめたモジュールでは、`call_indirect` も同じインスタンスの中の呼び先になるので、V8 の投機的
   インライン化が効く。関数ポインタの呼び出し (qsort の比較関数、COM、WndProc) に効く。テーブルは
   そのモジュール自身のもの (または共有でも、呼び先がそのインスタンスの関数) である必要がある。
3. 実ゲームの計測では、リージョン間の乗り換えをまとめても 1% 未満だった (vnvm-v8)。これは「乗り換えの
   回数」の話で、インライン化による関数の中の改善 (退避が消える、定数が伝わる) は別に効く。
   flatbench (fib、oop、sqlite) のように呼び出しの多いコードで先に確かめる。
4. まとめ直したモジュールはサーバーに保存し、compileStreaming のコードキャッシュに載せる
   (Turboshaft のコードだけがキャッシュされる。インライン化済みのコードがそのまま次回に使われる)。

### F. 起動中 (Liftoff の間) のコスト

1. 冷たい関数はローカルを少なく (Liftoff は引数以外のローカルが 5 個を超えると全部メモリに置く)。
   作り直しで熱くなった関数だけを C の形にすればよく、初回の翻訳は今のままでもよい。
2. 宣言したローカルは関数に入るたびにゼロで埋められる。compact_locals (a0690a2) はこれを減らした。
   ローカルの種類 (i32/i64/f64/v128) ごとに必要な数だけ宣言する方針を続ける。
3. サーバー側の wasm-opt は、本編の CPU に効かなかった (vnvm-aot)。Turboshaft が同じことをするので、
   wasm-opt で効くのは、Turboshaft に無い LICM (`--licm`) くらい。D.1 を翻訳器で入れる代わりに、
   保存する部分にだけ `--licm` をかけて効果を見るのは安い実験になる。

### G. 細かいこと

- 32 ビットのゲストのレジスタは i32 で持つ (x64 の 32 ビット演算は上位を 0 にするので、
  アドレスにするときの拡張が要らない。arm64 は uxtw でアドレス指定に畳み込まれる)。
- アドレスの正の変位は `offset` 即値に (今の `folded`)。x86 の 32 ビットの折り返しとは違い、wasm の offset は
  4GB を超えると範囲外になるが、ゲストの領域は下位 2GB なので問題にならない。
- PSHUFB は `i8x16.swizzle(a, b & 0x8F)` で 1 命令 + and (swizzle は 16 以上の添字を 0 にする)。
  x64 では pshufb、arm64 では tbl になる。
- SETcc/CMOVcc は wasm の `select` に。Turboshaft は i32/i64 の select を、対応する CPU では cmov/csel にする
  (`wasm/turboshaft-graph-interface.cc:1104`、`compiler/turboshaft/select-lowering-reducer.h:51`)。

## 5. 次の一手 (実装スレッド向け)

1. `--wasm-enforce-bounds-checks` あり・なしで flatbench と Reunion の本編を測る (Android の範囲チェックの
   重さ)。続けて A.1 (import の最小) を入れて同じ条件で測る。小さい変更で、デスクトップには影響が無い。
2. 翻訳器に「合流点で phi になる値の数」と「実行時の値として残る L_OP」の統計を出させ、C.1〜C.3 の
   どれから手を付けるかを決める。
3. 戻り値を `(status, esp)` の 2 個にする (B)。2 個までならレジスタで返る。
4. 熱い関数のまとめ直しと直接 `call` (E) を flatbench で試す。

## 出典

V8 のソース (https://github.com/v8/v8 、commit b685385、2026-11-01):
`src/wasm/wasm-linkage.h`、`src/compiler/wasm-compiler-definitions.cc`、`src/codegen/x64/register-x64.h`、
`src/codegen/arm64/register-arm64.h`、`src/wasm/baseline/liftoff-compiler.cc`、
`src/wasm/baseline/liftoff-assembler-defs.h`、`src/wasm/turboshaft-graph-interface.cc`、
`src/wasm/wasm-module.cc`、`src/wasm/wasm-module.h`、`src/trap-handler/trap-handler.h`、
`src/compiler/pipeline.cc`、`src/compiler/turboshaft/loop-unrolling-reducer.h`、
`src/compiler/turboshaft/stack-check-lowering-reducer.h`、`src/compiler/turboshaft/wasm-optimize-phase.cc`、
`src/compiler/turboshaft/wasm-dead-code-elimination-phase.cc`、`src/compiler/backend/register-allocator.h`、
`src/wasm/function-body-decoder-impl.h`、`src/flags/flag-definitions.h`、`src/flags/feature-flags.h`。

V8 の記事: https://v8.dev/blog/liftoff 、https://v8.dev/blog/wasm-dynamic-tiering 、
https://v8.dev/blog/wasm-tail-call 、https://v8.dev/blog/leaving-the-sea-of-nodes 、
https://v8.dev/blog/wasm-speculative-optimizations 、https://v8.dev/docs/wasm-compilation-pipeline

実装: CheerpX (https://labs.leaningtech.com/blog/extreme-webassembly-1-pushing-browsers-to-their-absolute-limits 、
https://labs.leaningtech.com/blog/extreme-webassembly-2-the-sad-state-of-webassembly-tail-calls 、
https://labs.leaningtech.com/blog/branch-hinting 、
https://labs.leaningtech.com/blog/beyond-devtools-profiling-webapps-at-the-assembly-level 、
Cheerp の stackifier: https://medium.com/leaningtech/solving-the-structured-control-flow-problem-once-and-for-all-5123117b1ee2)、
v86 (https://github.com/copy/v86/blob/master/docs/how-it-works.md)、
QEMU-wasm (https://github.com/ktock/qemu-wasm 、TCG wasm バックエンドのパッチ
https://patchew.org/QEMU/cover.1756216429.git.ktokunaga.mail@gmail.com/)、
Box64 (https://github.com/ptitSeb/box64/blob/main/docs/USAGE.md の CALLRET、SAFEFLAGS、BIGBLOCK、NATIVEFLAGS)、
wemu (https://github.com/44670/wemu)、elfconv (https://github.com/yomaytk/elfconv)、
Binaryen (https://github.com/WebAssembly/binaryen)、
分岐ヒント (https://github.com/WebAssembly/branch-hinting)、
compilation hints (https://github.com/WebAssembly/compilation-hints)。

論文: Jangda ら ATC'19 (https://arxiv.org/abs/1901.09056)、Titzer CGO'24 (https://arxiv.org/abs/2305.13241)、
Ramsey ICFP'22 "Beyond Relooper" (https://dl.acm.org/doi/10.1145/3547621)。

vnvm の内部の記録: docs/translator.md、docs/prior-work.md、vnvm-wine docs/hangover.md、
project memory の vnvm-v8、vnvm-calls、vnvm-perf-notes、vnvm-perf-notes2、vnvm-perf-notes3、vnvm-aot。

## 6. 外部の調査報告との突き合わせ (2026-11-02、こたが共有)

こたが別に用意した報告「V8向けWebAssembly動的変換系を高速化するための調査報告」(CheerpX、v86、
flycast-wasm を比べて、ホットなリージョンを 1 つの wasm 関数にし、guest の状態を locals に閉じ込める
方向を勧めるもの) を、vnvm の今の実装と上のソースの確認に照らした。

### 結論

報告の主な方針は正しいが、**vnvm ではほとんど入っている**。報告の前提 (「2〜3 倍の差」「CPUState を
毎命令読み書き」「基本ブロックごとのディスパッチ」「メモリアクセスごとの helper」) は、vnvm の現状とは
合わない。残っている差は、報告が扱っていない 2 点にある。Android の範囲チェック (4 章 A) と、
呼び出しの多いフラットなコード (flatbench はネイティブの約 5 倍、本編はデスクトップで 1.2〜1.4 倍)。

### 項目ごとの対応

| 報告の提案 | vnvm の今 | コメント |
|---|---|---|
| guest のレジスタを locals にキャッシュし、書いたものだけ出口で書き戻す | 済み。GPR は L_REG、MMX/XMM はブロック内とループで locals (`jit/packed.rs` `mm_get`、`mod.rs` の `mm_loop`/`xm_loop`)、x87 は関数全体でキャッシュ (420b307)。呼び出しで書き戻すのは前回以降に書いたものだけ (5187716) | 報告が言う 3〜5 倍は flycast-wasm の改善の幅で、vnvm は最初の JIT から locals を使っている |
| ホットな direct edge を同じ関数の br に、巨大な br_table にしない | 済み。リージョン、FN モード (支配木から構造化、`jit/structure.rs`)。入口の br_table は戻り先とループ先頭だけ (f5e06e1) | v86 の失敗を避ける形はもう取っている |
| メモリは software TLB の fast path を inline に | **不要**。vnvm はユーザーモードで、ゲストのアドレスがそのまま wasm のアドレス。TLB は無く、範囲チェック 1 つだけ (同じ基底では 1 回、`addr_pair`/`near_safe`) | v86 と flycast-wasm はシステムエミュレータなので TLB が要る。vnvm はそれより一段軽い |
| EFLAGS を遅延にし、生存解析で必要なものだけ作る | 済み (遅延フラグ + 生存解析、CALL/RET でフラグは死んでいる扱い、cmp+jcc の融合) | 残りは 4 章 C.1 (合流点で L_OP が実行時の値になる所) |
| indirect edge を guard 付きで特殊化する | 済み (リングのヒントで間接呼び出しと末尾ジャンプを guard 付きでインライン化、`jmp [i*4+T]` は br_table、ハッシュ分岐) | V8 の投機的インライン化は同じインスタンスでしか効かないので、翻訳器側でやっているのが正しい (4 章 E) |
| SSE を v128 に直接 | 済み (MMX と SSE2 の整数命令、`jit/packed.rs`) | relaxed SIMD はビット単位で同じ結果が要るので使い所は少ない。PSHUFB は普通の swizzle で足りる (4 章 G) |
| slow path に分岐ヒント | 試して差なし (Chromium 153、vnvm-games7) | C を入れた後で測り直す価値はある (4 章 D.5) |
| x87 は実際の命令頻度で方針を分ける | 計測済み: Reunion は MMX 50%、整数 37%、x87 1.3%。NScripter は整数 80% (vnvm-games7) | 今のゲームでは浮動小数点の割合は小さい。効くのは MMX のブレンドのループ |
| 1 フレームあたりのカウンタで測る | `IPPAI_JIT_COUNT` (ブロック、入口、ハッシュ表、乗り換え、コンパイル回数) と `IPPAI_JIT_STATS` がある | 報告のリストのうち、範囲チェックの出口 (bail) の回数と Wine 境界の回数は足すとよい |
| リージョンの大きさを live GPR/XMM 数・入口数・バイト数の cost function で切る | 一部 (大きさの上限、入口の制限)。生きている値の数では切っていない | 4 章 C の「合流点の値 9 個以下」を切る基準にも使える。採り入れる価値がある |

### 報告の中で直すべき点

1. **「ARM64 Linux の Chromium で測れば Android の原因を分解しやすい」は、範囲チェックについては成り立たない**。
   V8 は ARM64 Linux では trap handler を使い、Android では使えない (`trap-handler.h:18-60`)。ARM64 Linux で
   測ると、Android で一番重いかもしれないコストが消える。代わりに `--wasm-enforce-bounds-checks` を付ける。
2. ARM64 の整数の引数レジスタ x7, x0, x2–x6 のうち、**x7 はインスタンス**。wasm の引数としてレジスタで
   渡せるのは 6 個 (x64 は 5 個)。
3. Branch Hinting の出荷は、prior-work.md の確認では M136 (報告は 137)。どちらにしても今の Chrome では有効。
4. 報告は「Wine の処理は Wasm ネイティブに早く抜ける」と書いている。vnvm では Wine の Unix 側はもう
   wasm で、PE の DLL を直接 wasm にする作業は別のスレッドにある (vnvm-wine docs/wasm-dlls.md)。

### 報告から新しく採るもの

- flycast-wasm (https://github.com/nasomers/flycast-wasm 、SH4 → wasm JIT、2026): レジスタは locals、
  ホットなコードを複数ブロックのモジュールにまとめる、RAM アクセスを inline にして import の往復を
  1 フレーム約 50 万回から 9 千回にした (作者自身の計測)。残課題に FPU レジスタのキャッシュを挙げている。
  vnvm と同じ方向で、vnvm より 1 段手前にいる。
- リージョンを切る基準に「生きている値の数」を入れる (上の表)。
