# PGO workflow

This project has two intentionally separate performance workflows:

- `make bench` runs the Divan suite in `benches/bench.rs`. It measures library
  and pager operations in-process, including benchmark setup choices and
  Divan's allocation profiler.
- `make pgo-profile` builds and launches the release-shaped `diff-pretty`
  application. Only that child process is instrumented; the driver, Cargo,
  compiler, proc macros, tests, and benchmarks are not part of the collected
  workload.

The distinction matters because LLVM PGO describes executed application code,
not the code paths and sampling behavior of a benchmark harness.

## Audit and chosen POC objective

Before this separation, `pgo-profile` set an unscoped `RUSTFLAGS` and ran
`cargo bench --bench bench`. That could profile the Divan binary, its global
`AllocProfiler`, benchmark setup, and the `render_pgo_training_workload`
function in addition to the renderer. It was not a profile of the installed
binary.

The repository's current evidence points to these likely optimization
objectives:

| Objective | Evidence | POC status |
| --- | --- | --- |
| Non-interactive Git-log rendering | `render_reader_to_log_000`, `render_log_000`, and the application's pipe path in `src/main.rs` | Chosen |
| Retained rendering and pager viewport work | `render_document_show_010`, `pager_viewport_show_010`, and the native pager in `src/pager.rs` | Deferred |
| Large, metadata-heavy, colorized, and plain input shapes | The fixture buckets and corresponding Divan benchmarks in `benches/bench.rs` | Deferred from the first profile |

The initial POC assumes that a deterministic multi-commit Git-log render is
the most useful first target. This is an engineering assumption, not a
user-validated usage weight. The profile workload is deliberately one narrow
scenario: one child launch, `fixtures/log_000.patch` on stdin, and
`--paging=never`. It exercises process startup, stdin buffering, incremental
patch parsing, word pairing, ANSI rendering, and writing through the same
application boundary used when stdout is not a terminal.

The following are explicitly deferred until usage evidence is available:

- a PTY workload for pager startup, viewport repaint, navigation, and search;
- user-curated weights across Git show, log, colorized, plain, and large diffs;
- cold-cache versus warm-cache and empty-state versus large-state scenarios;
- terminal-size and corpus-size matrices;
- latency, RSS, binary-size, and regression thresholds chosen by users.

## Commands and boundaries

For a host build:

```sh
make pgo-instrument TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
make pgo-profile TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
make release-pgo TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
make verify-release TARGET="$(rustc -vV | awk '/^host:/ {print $2}')"
```

`pgo-profile` is the normal profile command. The other explicit steps are
useful when inspecting the boundary:

1. `pgo-instrument` removes only the variant-specific
   `target/pgo-instrument-*` and `target/pgo-profiles/<target>-*` directories,
   then builds `--bin diff-pretty` into the isolated instrument target. The
   final Linux/macOS release uses `build-std`; the instrument build uses the
   installed target standard library because applying profile generation to the
   host's custom core/std build currently creates duplicate lang items.
   This is an explicit profile warning, not a reason to treat the POC as
   complete.
2. `scripts/pgo-workload.py` rejects a missing or non-executable binary and
   launches it directly. It writes child output to `/dev/null` so output volume
   does not dominate the profile and uses `LLVM_PROFILE_FILE` to give each
   child process its own `.profraw` file.
3. `pgo-merge` refuses to merge an empty profile directory, runs
   `llvm-profdata`, writes `merged-functions.txt`, requires `diff_pretty`
   application symbols, and rejects Divan or benchmark symbols.
4. `release-pgo` first builds dependencies without profile-use, then applies
   `-Cprofile-use` and `-Cllvm-args=-pgo-warn-missing-function` only to the
   application binary through `cargo rustc`. The final verification checks for
   profile sections and the profile runtime as well as the platform's linking
   contract.

Profile generation uses `CARGO_TARGET_<target>_RUSTFLAGS`, not unscoped
`RUSTFLAGS`, so host-side build scripts and proc macros are outside the target
profile boundary. Linux dynamic and static builds have separate instrument and
profile directories. They must be run in the same architecture-aware
`Dockerfile` image as profile use and final verification.

Linux commands are:

```sh
make pgo-instrument-linux TARGET=x86_64-unknown-linux-musl
make pgo-profile-linux TARGET=x86_64-unknown-linux-musl
make release-pgo-linux TARGET=x86_64-unknown-linux-musl
make verify-release-dynamic TARGET=x86_64-unknown-linux-musl

make pgo-instrument-linux-static TARGET=x86_64-unknown-linux-musl
make pgo-profile-linux-static TARGET=x86_64-unknown-linux-musl
make release-pgo-linux-static TARGET=x86_64-unknown-linux-musl
make verify-release TARGET=x86_64-unknown-linux-musl
```

The release workflow runs these inside the project `Dockerfile`; do not copy a
profile from a different libc, linker, target, or LLVM toolchain.

## Comparing the same workload

Divan results and end-to-end process measurements are separate reports. To
compare binaries, preserve both application binaries under explicit names and
run the same driver repeatedly:

```sh
python3 scripts/pgo-workload.py \
  --binary ./diff-pretty-baseline \
  --iterations 9
python3 scripts/pgo-workload.py \
  --binary ./diff-pretty-pgo \
  --iterations 9
```

The driver reports median, minimum, and maximum child-process time. Build time
is excluded. The first POC does not claim a baseline-versus-PGO improvement;
that measurement must be collected on the target release machine and should
be reported with its distribution and any profile warnings.
