# Clauses

Each file lists one API family's behavior as numbered clauses, each with its status and where
it comes from (a Microsoft Learn page, or `(none)` when the documentation is silent):

* `doc`: the documentation says it; the model does it.
* `measured`: Windows' recorded answers showed it (the golden named); the model does it.
* `assumed`: the model does it but neither the documentation nor an answer settles it yet;
  the cases that reach it are compared with Windows and the clause becomes `measured` (or is
  corrected) when the answers agree.
* `open`: not modelled; a case that reaches it prints `unmodeled <id>` and is compared with
  Windows only, until an answer settles it.
* `excluded`: deliberately not asked (calls that read past Windows' own arrays and may crash
  it); no case needs to reach it.

The models name a clause each time they apply it (`spec::hit`), and `cargo test` checks that
every clause here is reached by some case of the suites, that the code names no clause missing
here, and prints the coverage (`cargo run -- spec`: each clause, its status, the cases that
reach it).
