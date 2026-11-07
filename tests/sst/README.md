# SingleStepTests on koji

[SingleStepTests](https://github.com/SingleStepTests) are single-instruction tests recorded on
real chips: the full register and memory state before and after, for every opcode (and ModR/M
group), with random operands, segment overrides and LOCK/REP prefixes. Five suites run here,
all in real mode, on koji's interpreter in 16-bit mode with real-mode segments
(`koji::LDT_REAL`): final registers, defined flags and every memory byte must match the chip.

| cpu | suite | tests | what it adds |
| --- | --- | --- | --- |
| 80386 | 80386 `v1_ex_real_mode` (386EX) | 1.6 M | every 66/67 prefix combination, 32-bit operands and addresses |
| 80286 | 80286 `v1_real_mode` | 1.4 M | the 16-bit set on other silicon, 24 address lines |
| 8086 | 8086 `v1_binary` | 0.5 M | the 808x's 16-bit set (other random states, other edge cases) |
| 8088 | 8088 `v2_binary` | 2.5 M | the same on the 8088 |
| V20 | V20 `v1_native_binary` | 2.5 M | NEC's 8088 with the 186 instructions (PUSHA, BOUND, ENTER, shifts by imm) |

    tests/sst/fetch.sh [cpu...]            # once: ~1.9 GB to target/sst/<cpu> (pinned commits)
    cargo test --release -p koji --test singlestep                     # all suites in parallel
    SST_CPU=8088 SST_FILTER=F6.7 SST_SHOW=5 cargo test --release -p koji --test singlestep -- --nocapture
    SST_UPDATE=1 cargo test --release -p koji --test singlestep        # rewrite expected-<cpu>.tsv
    (cd tools/jit16test && SST_N=200 cargo run --release -- sst [cpu]) # the 16-bit JIT

`moo.rs` reads the MOO files and decides what is run (`skip`, `not_modeled`); it is shared by
the interpreter's test and tools/jit16test (which runs what the translator does itself, of the
tests the interpreter gets right). `expected-<cpu>.tsv`: passing tests per file. Fewer than
listed fails; more prints a note.

Not compared or not run, and why (the counts are printed per run):

- flags Intel documents as undefined for the instruction (iced's tables; shifts by >= width:
  CF, OF), IF / IOPL / NT (koji is a user-mode CPU), the reserved FLAGS bits PUSHF stores
- #GP / #SS from real-mode segment limits (koji has none), I/O and privileged instructions,
  SIB bytes with a scale but no index (the 386 scales the base, later CPUs don't), SHLD/SHRD
  by more than the operand width
- 64 KB segment wrap on the 16-bit chips (the 808x wraps an offset past FFFF, the 286 each
  word of a far pointer at FFFE; koji, whose Win16 code never does it, goes on linearly)
- 80286: 0F (its protected-mode system instructions), ESC without a coprocessor, LOCK on an
  instruction that can't be locked (the 286 ignores it, the 386 raises #UD), MOV to/from
  segment registers 4-7 and LDS/LES of a register (#UD on the 286; FS/GS on the 386)
- 8086/8088/V20: what the 808x does that IA-32 doesn't. Its aliases (60-6F as 70-7F, C0/C1/C8/C9
  as the RETs, undefined ModR/M forms of 8C/8E/8F/C6/C7/FE/FF, SETMO, LEA/LES/LDS of a
  register, POP CS), SALC, ESC and WAIT, shift counts it doesn't mask to 5 bits, PUSH SP (it
  pushes the decremented SP), REP before MUL/IDIV (negates the result); the V20's 0F
  extensions, REPC/REPNC, and AAM/AAD, which ignore their base

Handled as the chip does: the 808x's and V20's divide error and BOUND push the address of the
next instruction; their tests end at the next instruction (the 386's and 286's at a HLT).

Left in expected-<cpu>.tsv as known differences (behaviour of these chips later CPUs don't
share, or single edge cases):

- 80386: POPAD with a 16-bit stack loading ESP's upper half, IDIV quotients the 386 returns
  where later CPUs raise #DE, AAM 0's flags, a few single tests (LSS/LES at the segment end,
  REP string ops overwriting themselves)
- 80286: AAM 0's flags (9), IDIV quotients the 286 returns where later CPUs raise #DE (4)
- 8086, 8088, V20: DAA/DAS/AAA/AAS for operands outside the BCD range (the 808x's own adjust
  rules: AAA/AAS change AL by 6 without carrying into AH; DAA/DAS's flags), AAM 0 (the 808x's divide by zero leaves its own flags), IDIV
  quotients of -128/-32768 (the 808x raises a divide error where later CPUs return them)
