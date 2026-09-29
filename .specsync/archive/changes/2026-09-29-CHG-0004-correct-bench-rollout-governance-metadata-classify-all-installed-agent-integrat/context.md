---
change: CHG-0004-correct-bench-rollout-governance-metadata-classify-all-installed-agent-integrat
artifact: context
---

# Context

The rollout installed four agent integrations, but the SDD configuration omitted their directories from meaningful path enforcement. The CHG-0003 changelog row also omitted the spec version, and Gemini's create-change instructions added literal quotes around an argument placeholder that can already be quoted by the invocation layer. These are governance and guidance defects only; Bench runtime behavior is unchanged.
