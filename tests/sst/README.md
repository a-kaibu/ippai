# SingleStepTests/80386 on koji

`crates/koji/tests/singlestep.rs` runs the real-mode suite of
[SingleStepTests/80386](https://github.com/SingleStepTests/80386) (1.6 M single-instruction
tests recorded on a real 386EX: every opcode with every 66/67 prefix combination, random
segment overrides, LOCK and REP) on koji's interpreter in 16-bit mode with real-mode segments
(`koji::LDT_REAL`). Final registers, defined flags and every memory byte must match the chip.

    tests/sst/fetch.sh                     # once: ~430 MB to target/sst (pinned commit)
    cargo test --release -p koji --test singlestep
    SST_FILTER=0FA4 SST_SHOW=5 cargo test --release -p koji --test singlestep -- --nocapture
    SST_UPDATE=1 cargo test --release -p koji --test singlestep   # rewrite expected.tsv

`expected.tsv`: passing tests per file. Fewer than listed fails; more prints a note.

Not compared or not run, and why:

- flags Intel documents as undefined for the instruction (iced's tables; shifts by >= width:
  CF, OF), IF / IOPL / NT (koji is a user-mode CPU)
- #GP / #SS from real-mode segment limits (koji has none), I/O and privileged instructions,
  SIB bytes with a scale but no index (the 386 scales the base, later CPUs don't), SHLD/SHRD
  by more than the operand width

Left in expected.tsv as known differences (386 behaviour later CPUs don't share or edge
cases): POPAD with a 16-bit stack loading ESP's upper half, IDIV quotients the 386 returns
where later CPUs raise #DE, AAM 0's flags, a few single tests (LSS/LES at the segment end,
REP string ops overwriting themselves).
