# 先行研究と vnvm への適用 (2026-11-01)

他の x86 翻訳系と論文で何をしているかを調べ、vnvm (Wine は wasm、x86 は x86→wasm JIT) に
当てはまるものを並べた。Windows と PE だから前提にできること (呼び出し規約、どのゲームでも同じ
DLL、IAT、SEH) も使う。各項目の数値は出典が公表しているもの。出典ごとの URL は最後にまとめた。

## 1 回目の起動 (cold) の内訳 (このマシン、headless Chromium 153、4 コア)

Reunion、保存なし・ブラウザのキャッシュなし。タイトルまで 13.1〜13.8 秒です。保存した
リージョンがある場合は 10.9 秒、さらにコードキャッシュも効く場合は 9〜10 秒、ネイティブは 5.9 秒。

- 最初の 14 秒の CPU (perf、全コア):
  - アイドル 30%
  - V8 のコンパイルスレッド 24%: リージョンの TurboFan に 7.0 秒、wine.wasm に 1.8 秒
    (`--trace-wasm-compilation-times`)
  - Wine のワーカー 33%
- TurboFan にかかったリージョンは 2716 個中 591 個で、本体 1KB あたり 1.26ms かかった。
  30KB を超える 67 個だけで 3.7 秒だった。
- それでも `--liftoff-only` (TurboFan を使わない) でタイトルまで 14.3 秒。コンパイルの CPU は、
  空いているコアで走るので待ち時間にはあまり効いていない。
- ゲームスレッドは起動後 4〜9 秒の間はほぼ埋まっていて、その後はゲームのタイマー待ちになる。
  埋まっている間の内訳:

  | 内訳 | 割合 |
  |---|---|
  | x86 のリージョン | 33% |
  | インタプリタ | 17% (うち命令デコードが約 4 割) |
  | V8 のランタイム | 17% (リージョンごとのモジュール作成・Liftoff・インスタンス化) |
  | Wine の unix 側 | 12% |
  | カーネル | 11% (ページフォルト、コード領域の保護切り替えに伴う TLB shootdown) |
  | 翻訳器 | 3% |

- リージョンの翻訳の約 4 割 (3056 個中 1246 個、x86 コード 720KB 中 310KB) が Wine の DLL と
  補助プロセスのもの。残りがゲーム本体とプラグイン。
- 生成コードは x86 1 命令あたり平均 35.7 バイト。mov は 30 バイトで全体の 34% を占める。
  オフラインの再翻訳 (`cargo run --release -p vnx86 --example corpus -- out.wasm *.idx`) で
  全リージョンを TurboFan にかけると 10.2 秒 (1 スレッド)、Liftoff は 0.5 秒。範囲チェックの出口
  (bail) を全部 trap にすると 8.1 秒なので、出口の分岐は TurboFan の時間の約 2 割に当たる。

## 調べたもの

### Box64 (ptitSeb)

- CALLRET: RET が CALL の次へ直接戻る。
- SAFEFLAGS: CALL と RET の時点ではフラグを要らないとみなす。既定は 1 で、「RET では
  たいてい要る、CALL ではたいてい要らない」。
- NATIVEFLAGS: ARM のフラグに直接割り当てる。7-Zip で最大 30%。
- X87DOUBLE: x87 を double (精度制御で float) で計算する。
- STRONGMEM: TSO 用のバリアの強さを選ぶ。v0.3.6 からは Windows の volatile metadata を読む。
  ただし x64 PE の話。
- BIGBLOCK=3: PE でも大きなブロックを作る (Wine 向け)。
- DynaCache (v0.3.8〜、v0.4.4 で既定):
  - 実行ファイル・DLL ごとの永続キャッシュで、**同じライブラリなら別のプログラムでも
    使い回す**。
  - Factorio のロードが 39 → 29 秒 (2 回目)。
- WowBox64: Wine の中で 32bit の x86 コードだけをエミュレートする PE DLL (Wine 自体は
  ネイティブ)。

### FEX-Emu

- multiblock (2503 で既定): 複数のブロックをまとめて翻訳する。発見を 1 ページに絞り、
  飛び先でブロックを分けたらコンパイル時間が半分になった。
- 使われないフラグの計算を消す。2312 で SF/ZF/CF/OF をホストの NZCV に置いた (同月の他の改善と
  合わせて Geekbench +17.6%)。
- x87 を 64bit 浮動小数点で近似するモード。
- TSO の再現: スタックへのアクセスは対象外。揃っていない atomic は half barrier。
- AOT IR (旧): エクスポート・シンボル・アンワインド表と CALL の飛び先から入口を集め、約 9 割を
  見つけた。ゲストコードの xxh3 をキーにした。clang の起動 1.29 → 0.38 秒。
- 2601 で AOT イメージの自動生成と検証、2609 でディスクキャッシュ (試験的)。
- thunk: GL/Vulkan をホストのライブラリに渡す。
- Wine 統合: libwow64fex.dll (i386)、libarm64ecfex.dll (x64)。CPU だけを受け持つ。

### Rosetta 2 (dougallj の解析)

- インストール時に AOT で text 全体を翻訳し、JIT はほぼ使わない。
- 状態は命令境界で常に正規の形にする (例外を正確にするため)。命令をまたぐ最適化は 2 つだけ:
  - 使われないフラグの計算を消す
  - push/pop の列をまとめる
- CALL/RET を ARM の BL/RET にし、(ゲストの戻り先, ホストの戻り先) を別スタックに積んで
  RET で照合する。ハードウェアの戻り先予測が効く。
- メモリ順序はハードウェアの TSO モード。

### Windows on ARM (XTA、CHPE、ARM64EC、Prism)

- XtaCache サービス: 翻訳したブロックを**モジュールごとに**保存し、「他のアプリも初回起動で
  使える」。
- CHPE: x86 から呼べるが中身は ARM64 のシステム DLL を出荷する。システム DLL は翻訳しない。
- ARM64EC: x64 の規約に合わせたネイティブコード。entry/exit thunk で行き来し、間接呼び出しは
  チェッカーが振り分ける。
- Prism (24H2) の既定値:
  - x87 は 64bit 近似
  - マルチコアのバリアは Fast
  - アプリごとのキャッシュ
  - 自己書き換えは一般的な場合だけ扱う

### QEMU TCG

- goto_tb で TB をつなぎ、飛び先が分かったらジャンプを書き換える。
- 間接分岐は lookup_and_goto_ptr (CPU ごとの tb_jmp_cache を引いて飛ぶ)。
- フラグは cc_op で遅延評価し、TB の終わりか例外の時だけ書き出す。
- 生存解析で死んだ一時値を消す。
- 自己書き換えはコードのページを書き込み禁止にし、ページごとの TB 一覧で無効化する。

### v86

- 熱いページと、そこから届く数ページを 1 つの wasm 関数にする (入口ごとの br_table、
  stackifier)。
- モジュール間はつながず、毎回メインループに戻る。
- 作者は、ブラウザは巨大な br_table と関数全体のローカルを苦手とする、と書いている。

### CheerpX / WebVM (Leaning Technologies)

- インタプリタで発見し、熱い入口から直接分岐で届くコードを 1 つの wasm 関数にする
  (vnvm のリージョンと同じ)。
- 間接ジャンプは return_call_indirect。
- 分岐ヒントの提案者で、`br_if_unlikely` の試作では 7〜10% 速くなった。

### 論文

- Jangda ら, "Not So Fast" (USENIX ATC'19):
  - wasm はネイティブより平均 45% (Firefox) 〜 55% (Chrome) 遅い。
  - 原因はレジスタ不足によるロード 2.0 倍・ストア 2.3 倍、範囲チェック・スタック検査の分岐、
    コードの肥大。
  - 直接 wasm にしたコードの 1.2〜1.3 倍という目標の下限は、ほぼこれで決まる。
- Reddi ら, "Persistent Code Caching" (CGO'07、Pin):
  - 実行をまたいだ翻訳の再利用で GUI アプリが最大約 90% 改善。
  - **別のプログラムが作ったライブラリの翻訳の再利用で 59%**。
- HQEMU (CGO'12): 別スレッドで LLVM 最適化したトレースに差し替え、QEMU 比で SPECint 2.6 倍、
  SPECfp 4.1 倍。
- Dynamo (PLDI'00) / NET: 熱い飛び先から続けて実行される経路をトレースにする。
- Hiser ら (CGO'07): 間接分岐の扱い方で DBT のオーバーヘッドは大きく変わる。戻り (RET) は
  別扱いにする価値がある。
- 条件コード: 遅延評価をさらに遅らせる "super-lazy" でコード量 -8%、翻訳時間 -5.7%
  (SBESC'14)。
- 弱いメモリモデル上での TSO の再現:
  - Lasagne (PLDI'22): フェンスを平均 45.5% 削減。
  - Risotto (ASPLOS'23)。
  - ゲストのスレッドが別々のワーカーで動くので、Android (arm64) では関係がある。

### V8 の性質 (V8 main のソースで確認)

- 分岐ヒント (`metadata.code.branch_hint`) は Chrome M136 で出荷済み (Android も)。
  Turboshaft は読むが、Liftoff は無視する。
- compilation hints (関数ごとの層の指定) は実験フラグの裏で、出荷されていない。
- 遅延コンパイル (初回呼び出しで Liftoff) と動的階層化:
  - 予算 13M を使い切ると TurboFan にかかる。
  - wasm には OSR が無いので、長いループは関数に入り直すまで Liftoff のまま。
- 投機的インライン化 (call_indirect/call_ref、M137) は同じインスタンス内だけ。インポートした
  テーブル越しの呼び出しはインライン化しない。
- コードキャッシュ:
  - compileStreaming だけが対象で、キーは URL。128KB 以上の場合だけ。
  - キャッシュされるのは TurboFan のコードだけ。

## vnvm に入れたもの (2026-11-01)

1. **Wine の DLL・補助プロセスの翻訳をゲーム間で共有** (XTA の「モジュールごとのキャッシュを
   他のアプリが初回から使う」、Pin の 59%、Box64 DynaCache の共有ライブラリ)。
   - 仕組み:
     - vnvm-server は、あるプログラムの保存に、別のプログラムも保存したリージョン (起点と
       元のコードが同じもの) があれば `_shared-<版>` に移す。
     - どのページも自分の保存と一緒にこれを読む。
     - 読み込むファイルの先読みも `_shared.chunks` で共有する。
   - URL がゲームによらず同じなので、ブラウザのコードキャッシュも別のゲームで効く。
   - bench.exe と Reunion の間だけで 715 リージョンが共通 (Wine の起動と DLL)。
   - 初めてのゲームを想定した測定 (Reunion を共有だけで起動): タイトルまで 13.3 秒が、
     コードキャッシュありで 10.0 秒、なしで 11.1〜12.5 秒になった。
2. **保存の版を翻訳器のソースのハッシュに** (`wine/jit-version.txt`、hangover/build.sh が書く)。
   - これまでは wine.wasm の Last-Modified が版だったので、web だけのビルドでも保存が全部
     無駄になっていた。
   - いまは vnx86 と vnhost の jit.rs/cpu.rs、vnjit.js が変わったときだけ作り直す。
3. **インタプリタのデコードキャッシュ** (`vnx86::DecodeCache`)。
   - スレッドごとに 2048 エントリの直接マップで、命令のバイト列が同じときだけ使う
     (自己書き換えにも安全)。
   - 1 命令 75 → 46 ns (ネイティブの計測、`--example ibench`)。

## これからの候補 (効きそうな順)

1. **Wine の PE DLL を wasm に直接ビルドする (CHPE / ARM64EC / Hangover の考え方)**。
   - Wine のソースはあり、呼び出し規約 (stdcall/cdecl/thiscall/fastcall、EBX/ESI/EDI/EBP は
     保存) も決まっている。
   - ゲスト側から入る thunk は、スタック上の引数を読んでネイティブの関数を呼ぶ。
     コールバック (WndProc、qsort の比較関数、COM) は exit thunk で JIT に戻す。
   - 全ゲームで Wine 側の翻訳・インタプリタ・フラグ処理が無くなる。一番効くが、一番大きい。
     まず msvcrt/ucrtbase の文字列・メモリ系や RtlEnterCriticalSection のような葉関数から
     始められる (memmove/memset はもう HLE)。
2. **サーバー側の AOT**。
   - PE のメタデータから入口を集めて、初回の前に翻訳しておく。使えるもの: エクスポート表、
     .reloc が指す関数ポインタ・vtable・ジャンプ表、SafeSEH ハンドラ表、CFG の
     GuardCFFunctionTable、TLS コールバック。
   - FEX は入口の約 9 割を見つけた。Rosetta 2 は全部 AOT にしている。
   - 共有の束 (1.) と同じ形式で配れる。
3. **リージョンをまとめて 1 モジュールに** (モジュール作成 17% + カーネル 11%)。
   - 起動中に 2700 個のモジュールを作っている。一度に翻訳したリージョンを 1 つのモジュールに
     まとめれば、インスタンス化・コード領域の確保・TLB shootdown がその分減る。
   - 同じインスタンス内なら V8 の投機的インライン化も効く。
4. **分岐ヒント**:
   - 範囲チェックの出口、インタプリタへの退避、予算切れ、ハッシュ表の外れを unlikely にする。
   - Turboshaft がコールドパスを外に出す。CheerpX は 7〜10%。安く入れられる。
5. **CALL/RET を wasm の call/return に** (Rosetta 2 の BL/RET + 別スタック、Box64 CALLRET)。
   - 呼び出し先リージョンを return_call でなく call で呼び、戻ってきたら戻り先を照合する。
   - 外れたとき (SEH の巻き戻し、longjmp) は今のハッシュ表に落とす。
   - V8 の戻り先予測とインライン化が効く。sort (qsort ↔ 比較関数) の差の主因。
6. **CALL/RET でフラグは死んでいるとみなす** (Box64 SAFEFLAGS、Rosetta の未使用フラグ消去)。
   - Win32 の ABI ではフラグは呼び出しをまたがない。DF=0 も保証されている。
   - vnvm はリージョン間で遅延フラグを引数で渡しているので、効くのは主にリージョン内の合流点
     (phi が減る)。手書きの asm に備えて、モジュールごとに戻せるようにする。
7. **IAT を定数とみなす**。
   - `call [__imp_X]` はローダーが書いた後は変わらないので、翻訳時に読んで直接の飛び先にする
     (外れたら今の経路)。
   - GetProcAddress の結果も同じ。1. と組むと exit thunk 無しでネイティブを呼べる。
8. **範囲チェックの出口を減らす**。
   - 出口の分岐は TurboFan の時間の約 2 割。複数のアクセスのチェックを 1 つにまとめ、
     ループの外に出す。
   - Windows のユーザー空間 (0x10000〜0x7fffffff) の外へのアクセスは SEH を使うコード以外では
     起きないので、チェックは「失敗したらインタプリタでやり直す」だけでよい。
9. **TurboFan を避けて背景で最適化** (HQEMU)。
   - 起動中に一瞬だけ熱いリージョンも TurboFan にかかっている。ゲームが温まってから翻訳器
     自身が合流・最適化した版を作り、保存して次回から使う方が CPU は少ない。
10. **Android (arm64) での TSO**。
    - wasm の普通のロード/ストアは arm64 では順序を守らない。ロックの無いキューなど x86 の
      TSO に頼るゲストコードは壊れうる。
    - FEX と同じく「スタック以外への store を release にする」などを、Android で問題が出たら
      入れる。

## 出典

- Box64: https://github.com/ptitSeb/box64/blob/main/docs/USAGE.md 、
  https://box86.org/2024/12/new-version-of-box64-v0-3-2-and-box86-v0-3-8/ 、
  https://box86.org/2025/06/new-box64-v0-3-6-released 、
  https://box86.org/2025/10/new-box64-v0-3-8-released/
- FEX: https://fex-emu.com/FEX-2312/ 、https://fex-emu.com/FEX-2502/ 、https://fex-emu.com/FEX-2609/ 、
  https://skmp-dev.pages.dev/blog/fex-aotir 、https://wiki.fex-emu.com/index.php/Development:ARM64EC
- Rosetta 2: https://dougallj.wordpress.com/2022/11/09/why-is-rosetta-2-fast/
- Windows on ARM: https://learn.microsoft.com/windows/arm/apps-on-arm-x86-emulation 、
  https://learn.microsoft.com/windows/arm/arm64ec-abi 、
  https://learn.microsoft.com/windows/arm/apps-on-arm-program-compat-troubleshooter
- QEMU: https://www.qemu.org/docs/master/devel/tcg.html
- v86: https://github.com/copy/v86/blob/master/docs/how-it-works.md
- CheerpX: https://labs.leaningtech.com/blog/extreme-webassembly-1-pushing-browsers-to-their-absolute-limits 、
  https://labs.leaningtech.com/blog/branch-hinting
- 論文: https://www.usenix.org/conference/atc19/presentation/jangda (Not So Fast)、
  Reddi et al. "Persistent Code Caching" CGO'07、
  https://cs.nthu.edu.tw/~ychung/conference/2012-CGO-HQEMU.pdf 、Bala et al. "Dynamo" PLDI'00、
  Hiser et al. CGO'07、Lasagne PLDI'22、Risotto ASPLOS'23
- V8: https://v8.dev/blog/wasm-speculative-optimizations 、https://v8.dev/blog/wasm-code-caching 、
  https://v8.dev/blog/wasm-dynamic-tiering
