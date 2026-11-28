# Source duplication benchmark

Use this benchmark when changing the matcher. It compares each release CLI run with checked-in expected output, including the order of the pairs, both source ranges, score, minimum, disposition, diagnostics and exit status. A mismatch exits with an error. The reference analyzer is Lint Gate commit `ddb4ed0059b3fc6b8c80a20cd0009f022b03e429`.

## Reproduce

Run from the Lint Gate checkout containing the benchmark with Python 3.12+ and Git. Pass the absolute path to the analyzer you want to measure. To build a packaged release executable from a clean checkout with Cargo's default target directory, use the following commands.

```sh
cargo +1.88.0 package --locked
cargo +1.88.0 build --release --locked \
  --manifest-path target/package/lint-gate-0.1.1/Cargo.toml
cli="$PWD/target/package/lint-gate-0.1.1/target/release/lint-gate"
python3 benchmarks/duplication/run.py --cli "$cli" --repeat 5
```

The default run uses local synthetic sources. To include Rolling Release, supply a local Git repository containing commit `8c4ff5a195dc2b757760d59db1298e4358cbe378`. The harness exports its pinned `src` tree into a temporary directory. The policy selects `src/**`, excludes `**/generated/**` and enables only duplication in report-only mode. [Pinned source tree](https://github.com/lhajoosten/rolling-release/tree/8c4ff5a195dc2b757760d59db1298e4358cbe378/src)

```sh
python3 benchmarks/duplication/run.py --cli "$cli" --rolling-release /path/to/rolling-release --repeat 5
```

## Measured baseline

Five consecutive invocations per case on Ubuntu 24.04.5 LTS, x86-64, Intel Core Ultra X7 358H, using Rust 1.88.0 and the locked release profile. Each sample measures a fresh CLI process through exit, including reading source, parsing, matching, consolidation and writing the report. Fixture preparation, compilation and the harness's JSON comparison are outside the timed interval. The analyzer at this revision exposes end-to-end timing only. Filesystem caches were allowed to warm between runs.

| Workload | Minimum tokens | Outcome | Individual runs (s) | Median (s) |
| --- | ---: | --- | --- | ---: |
| Rolling Release | 35 | 50 reported | 0.198, 0.189, 0.196, 0.183, 0.178 | 0.189 |
| Rolling Release | 50 | 16 reported | 0.172, 0.171, 0.218, 0.186, 0.168 | 0.172 |
| Rolling Release | 75 | 2 reported | 0.167, 0.170, 0.179, 0.167, 0.164 | 0.167 |
| 2,000 adjacent calls | 20 | 8 reported | 0.013, 0.010, 0.012, 0.012, 0.012 | 0.012 |
| Add 30,000 adjacent calls | 20 | scan exited 2 | 0.251, 0.173, 0.167, 0.162, 0.165 | 0.167 |
| Same exhausted input | 20 | baseline exited 2 | 0.185, 0.170, 0.172, 0.165, 0.181 | 0.172 |
| Two files, 30 adjacent calls each | 20 | 5 reported | 0.002, 0.002, 0.003, 0.002, 0.002 | 0.002 |

All 91 TypeScript files (520,140 bytes) completed at each minimum with zero diagnostics. The largest pair shared 99 tokens. The [corpus oracle](rolling-release.json) records each full report. Keep the oracle fixed when evaluating a candidate.

The offline repetitive fixture uses a four-token `ping();` statement. With 2,000 repeats the longest accepted non-overlapping pair spans 4,000 tokens on each side. Adding another file with 30,000 repeats produces `duplication_limit`. The fixture checks for exit 2, zero findings, and byte-empty baseline stdout. The [cross-file fixture](../../tests/fixtures/cli/source_duplication_cross_file_repetition_and_third_copy.json) also checks a 120-token match between two smaller repeated blocks.

For a third-copy regression, the harness scans two established files, then adds a third file while leaving both earlier files untouched. It checks the original 15-token pair and both new 10-token pairs. The packaged baseline fixture separately verifies that the original pair retains its exception while the two new pairs block. These checks run offline and have no timing threshold.
