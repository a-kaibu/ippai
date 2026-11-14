# shirusu: sake's Win32 calls against Windows'

[shirusu](https://github.com/a-kaibu/shirusu-poc) records what a Windows program asks of Windows
(typed arguments, results, last errors) into a public Trace format, and compares two Traces
with object correlation (handles matched by the call that made them, not by value). Here it
compares a trial game on real Windows with the same game on sake.

## Pieces

- **sake** (`crates/sake/src/shirusu.rs`): with `SAKE_SHIRUSU=1`, every guest call to an API of
  `tools/shirusu/apis.txt` writes an enter line and a leave line on stderr (the page's
  console.log): raw arguments, result, last error, and what shirusu's own hooks read of memory
  (CreateFileW's path, WriteFile's and ReadFile's buffers up to 4096 bytes, the I/O counts).
- **tools/shirusu** (`sake-shirusu LOG OUT.trace --arch x86|x86_64`): those lines into a
  Trace. Schemas come from shirusu's API catalog and values from shirusu-hook's capture
  functions, so both sides hash and encode alike. Outside masu's workspace, pinned to one
  shirusu commit (`SHIRUSU_REV` in the workflow is the same).
- **.github/workflows/shirusu.yml**: each title of `tests/shirusu/games.txt` played on
  windows-2025 under `shirusu capture --mode filtered --follow-children --api ...` (winplay.py
  with `LAUNCH`) and on sake (corpus ci.sh with `SAKE_ENV=SAKE_SHIRUSU=1`), then
  shirusu-conformance on both Traces and shirusu-compare between them
  (`tests/shirusu/report.py`: artifact `shirusu-report`, the run's summary).

## Reading the report

compare's output is candidates, not verdicts: a different number of ReadFile calls can be two
correct engines, and timing makes the order of calls on different threads differ. What is worth
a look: a call only one side makes, a different result or last error for the same call on the
same object, a byte read differently.

Limits that show up as differences without being sake's:

- sake records calls that reach its traps: guest code (the game, its DLLs, the guest CRT). A
  call between two of sake's own DLLs (msvcrt's fopen in Rust, not through CreateFileA) is not
  seen; on Windows the standard boundary records Runtime -> Windows calls (msvcrt's CreateFileA).
- shirusu's catalog has CreateFileA, FindFirstFileA and the Reg*A at L1: pointers only, no
  strings, so a path differs only through what follows (the handle's reads).
- a call that never returns (ExitThread, a handler that jumps) becomes a Loss Record of an
  unknown termination without its enter: shirusu-conformance fails an enter left open and
  shirusu-emit has no aborted event.
