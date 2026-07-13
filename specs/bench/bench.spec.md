---
module: bench
version: 1
status: active
files:
  - src/main.rs

db_tables: []
depends_on: []
---

# Bench

## Purpose

Run language-native benchmarks through fledge, persist a baseline, compare later results, and fail when regressions exceed a configured threshold.

## Public API

| Command | Behavior |
|---------|----------|
| Run (default) | Execute benchmarks and compare results with a stored baseline when present. |
| Save | Execute benchmarks and persist a schema-versioned baseline. |
| Show | Display the stored baseline in human or JSON form. |
| Clear | Remove the stored baseline. |
| Threshold option | Set the allowed regression percentage; defaults to 10. |
| Language option | Override marker-based Rust, Go, Node, or Python detection. |
| JSON option | Emit schema-versioned machine-readable output. |

## Invariants

1. Running benchmarks requires the fledge `exec` capability.
2. Saving a baseline requires the fledge `store` capability.
3. Baselines retain schema version, language, command, results, and save time.
4. A comparison exits non-zero when any result exceeds the configured regression threshold.
5. New benchmarks are reported but are not regressions without a prior value.
6. All parsed measurements are normalized to nanoseconds per operation.

## Behavioral Examples

```
Given a stored baseline at 1,000 nanoseconds per operation
When the same benchmark measures 1,200 with a 10 percent threshold
Then the result is marked as a regression and the command exits non-zero
```

## Error Cases

| Error | When | Behavior |
|-------|------|----------|
| Missing `exec` capability | `run` or `save` is requested | Log an error and exit 126. |
| Missing `store` capability | `save` is requested | Log an error and exit 126. |
| Unsupported language | No supported marker or override is available | Report detection failure and exit 2. |
| Benchmark command failure | Native tool returns non-zero | Surface its output and exit 1. |
| Unparseable output | Native tool succeeds without recognized measurements | Warn and return an empty result set. |

## Dependencies

- fledge-v1 `exec` and `store` capabilities
- Rust, Go, Node, or Python benchmark tooling selected by the project
- `serde`, `serde_json`, and `regex`

## Change Log

| Version | Date | Changes |
|---------|------|---------|
| 1 | 2026-07-12 | Document existing benchmark and baseline behavior for SpecSync 5 adoption. |
