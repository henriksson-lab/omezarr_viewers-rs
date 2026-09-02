# Fuzzing the parsers

Three entry points here take bytes this viewer did not write, from `file://`,
`http(s)://` or `s3://` alike:

| target | entry point | why it is worth fuzzing |
|---|---|---|
| `table` | `objects::table::read` | every count is a `u64` the file chooses — column count, row count, each name's length — and all of them are used to allocate and to multiply |
| `geojson` | `annotations::geojson::parse` | `serde_json` handles the JSON and enforces its own nesting limit; what is left is our walk over `childObjects` and the property readers |
| `npy_header` | `npy_header::split` and `classify` | a length-prefixed blob followed by a Python dict literal |

A malformed file should come back as an **error**, because an error names the
file and the number that was wrong. A panic is survivable — nothing sets
`panic = "abort"`, so it unwinds and the server stays up — and costs a dropped
connection plus a log line that does not say which file was bad.

## Running

Needs nightly and `cargo install cargo-fuzz`. The crate is its own workspace, so
CI's stable `cargo clippy --workspace` does not try to build it.

```sh
make fuzz                          # table, 60s
make fuzz TARGET=geojson TIME=600  # any target, any budget
cargo +nightly fuzz run table fuzz/corpus/table fuzz/seeds/table -- -max_total_time=60
```

The corpus directory is given **first**, because libFuzzer writes what it grows
into the first one it is named — point that at the seeds and the committed
inputs become a dumping ground for machine output. (Which is what happened the
first time this target was written.)

`fuzz/seeds/` is committed and `fuzz/corpus/` is not. The seeds are valid files —
a copy of the golden `fragments.bftable`, a real QuPath export, a numpy header —
because a fuzzer seeded with nothing spends its budget rediscovering the magic
word.

The table seed is a *copy* and is **not** a third pin on the golden blob: the
two that must stay byte-identical are `server/tests/data/` and blockflow's
`tests/data/`. This one only has to be a valid table, so if it drifts nothing
breaks — a starting point, not a contract. What libFuzzer grows from them is machine output.

A crashing input lands in `fuzz/artifacts/`. **Turn it into a named test rather
than committing it**: `a_column_count_larger_than_the_blob_is_refused_rather_than_allocated`
says what was wrong, and a file called `crash-8f3a...` does not.

## The cheap half, which runs on every build

`server/tests/parser_fuzz.rs` mutates the same three seeds with a fixed
generator and a fixed budget, and runs under `make test`. It is where the
capacity overflow in `table::read` was found, on its first run. Determinism is
the point: a fuzz failure nobody can reproduce is a flake, and a flake in CI
gets muted.

```sh
FUZZ_CASES=5000000 FUZZ_SEED=7 cargo test -p server --test parser_fuzz
```

Run it in **debug** as well as release. Release turns arithmetic overflow checks
off, and two of the three bugs found here only panic with them on. (cargo-fuzz
builds with `-Cdebug-assertions`, so the libFuzzer targets have them either way.)
