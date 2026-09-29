## MODIFIED

### SPEC SECTION Invariants

1. Running benchmarks requires the fledge `exec` capability.
2. Saving a baseline requires the fledge `store` capability.
3. Showing a baseline loads it only when `store` is granted; otherwise it reports that no baseline is saved.
4. Clearing emits a storage request and reports completion without independently rejecting a missing `store` capability.
5. Baselines retain schema version, language, command, results, and save time.
6. A comparison exits non-zero when any result exceeds the configured regression threshold.
7. New benchmarks are reported but are not regressions without a prior value.
8. All parsed measurements are normalized to nanoseconds per operation.

### REQUIREMENT REQ-bench-002

The plugin SHALL save, show, and clear a schema-versioned benchmark baseline through fledge storage while preserving the current capability-specific behavior.

Acceptance Criteria
- Save rejects a missing `store` capability.
- Show loads only when `store` is granted and otherwise reports that no baseline is saved.
- Clear emits the storage request and reports completion without independently rejecting a missing `store` capability.
