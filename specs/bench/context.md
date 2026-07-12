---
spec: bench.spec.md
---

## Context

The plugin provides a language-neutral performance regression gate on top of the fledge-v1 protocol.

## Related Modules

- fledge-v1 plugin execution and storage capabilities

## Design Decisions

- Persist a compact portable baseline rather than framework-specific artifacts.
- Normalize measurements so comparisons share one unit.
