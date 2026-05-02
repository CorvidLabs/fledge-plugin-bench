# fledge-plugin-bench

Run benchmarks, save a baseline, compare future runs against it, and flag regressions.

Built in Rust. Uses the [fledge-v1 plugin protocol](https://corvidlabs.github.io/fledge/plugin-protocol.html) with the `exec` + `store` capabilities — exec to run the bench tool, store to persist the baseline.

## Install

```bash
fledge plugins install CorvidLabs/fledge-plugin-bench
```

You'll be prompted to grant `exec` (run the language's bench tool) and `store` (persist the baseline).

## Usage

```bash
fledge bench                            # run + compare against baseline (or just run if none)
fledge bench save                       # run + save as the new baseline
fledge bench show                       # print the stored baseline
fledge bench clear                      # delete the stored baseline
fledge bench --threshold 5              # allow up to 5% regression (default: 10%)
fledge bench --lang go                  # force language detection
fledge bench --json                     # machine-readable
```

## Detection

| Language | Marker | Tool | Output format |
|----------|--------|------|---------------|
| Rust | `Cargo.toml` | `cargo bench` | libtest `ns/iter` + Criterion `time: [low mid high ns]` |
| Go | `go.mod` | `go test -bench=. -benchmem ./...` | `BenchmarkX  N  N.N ns/op` |
| Node | `package.json` | `npm run bench` | benchmark.js `name x N,NNN ops/sec` |
| Python | `pyproject.toml` | `pytest --benchmark-only` | `test_x  ...  N.N us` |

## Workflow

```bash
# Establish a baseline once
fledge bench save

# On every CI run, compare against it
fledge bench --threshold 10
```

If any benchmark slows down by more than the threshold, the command exits 1 and the offending entries are marked.

## Use in lanes

```toml
[lanes.perf]
description = "Block on >10% bench regression"
steps = [{ run = "fledge bench --threshold 10" }]

[lanes.perf-update]
description = "Update the perf baseline (run after intentional perf wins)"
steps = [{ run = "fledge bench save" }]
```

## JSON output

```json
{
  "schema_version": 1,
  "action": "bench_run",
  "language": "rust",
  "command": "cargo bench",
  "results": [
    {"name": "bench_parse_small", "ns_per_op": 1234.5}
  ],
  "saved_at": "2026-05-02T17:34:56Z",
  "compared": true,
  "threshold_pct": 10.0,
  "regression_count": 1,
  "diffs": [
    {
      "name": "bench_parse_small",
      "ns_per_op": 1234.5,
      "previous": 1000.0,
      "delta_pct": 23.45,
      "regression": true,
      "new": false
    }
  ]
}
```

## Where the baseline lives

`~/.config/fledge/plugins/fledge-plugin-bench/state.json` (managed by fledge's plugin store; subject to a 1 MB-per-plugin cap).

## Caveats

Bench output formats are not standardized. The parser is best-effort and pulls a single nanosecond figure per benchmark. Frameworks with richer output (pytest-benchmark groups, Criterion's full HTML reports) only get the headline number — pair this plugin with the framework's own reporting for deep analysis.

## Build

A pre-built binary ships at `bin/fledge-bench`. If `cargo` is on PATH at install time, the build hook recompiles from source for the host platform.

## License

MIT
