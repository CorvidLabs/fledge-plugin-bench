---
spec: bench.spec.md
---

## User Stories

- As a developer, I want to compare benchmark results against a durable baseline.
- As a CI author, I want performance regressions beyond a threshold to fail the gate.

## Acceptance Criteria

### REQ-bench-001

The plugin SHALL run native benchmarks for detected or explicitly selected Rust, Go, Node, and Python projects.

### REQ-bench-002

The plugin SHALL save, show, and clear a schema-versioned benchmark baseline through fledge storage while preserving the current capability-specific behavior.

Acceptance Criteria
- Save rejects a missing `store` capability.
- Show loads only when `store` is granted and otherwise reports that no baseline is saved.
- Clear emits the storage request and reports completion without independently rejecting a missing `store` capability.

### REQ-bench-003

The plugin SHALL exit non-zero when a comparable benchmark regresses beyond the configured threshold.

### REQ-bench-004

The plugin SHALL provide schema-versioned JSON output for run, save, and show operations.

## Constraints

- Native benchmark output parsing is best-effort and normalizes headline measurements to nanoseconds per operation.

## Out of Scope

- Framework-specific detailed reports and visualization.
