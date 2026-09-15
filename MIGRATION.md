# Migration to the canonical sidecar runtime

Canonical shared runtime implementation lives in `ores-otel/ores-otel-sidecar.rs`. This repository remains a compatibility/config-contract surface while consumers migrate.

## Required order

1. Pin the current `ORESoftware/ores-sidecar.rs` dependency to an immutable Git commit SHA or immutable released patch version.
2. Keep `.ores-sidecar.toml` and the `ores.sidecar-config.v1` wire/config semantics unchanged.
3. Admit the same independently authored TypeSpec and JSON Schema corpus in the canonical runtime; generated artifacts are evidence, not a third editable authority.
4. Switch runtime implementation/imports to `ores-otel/ores-otel-sidecar.rs` while keeping consumer conformance tests against the shared corpus.
5. Remove the compatibility dependency only after config parsing, listener invariants, runtime-update snapshot semantics, revision monotonicity, and health/probe behavior pass in the consumer.

## What may still change here

Compatibility-only patch releases may fix security, reproducibility, or contract-conformance defects required by existing consumers. New shared runtime subsystems, feature lines, or ownership expansion belong in the canonical runtime repository.

## Retirement

This repository can move to archive-only treatment when the canonical runtime consumes the same admitted config corpus, every active consumer is migrated or pinned to an immutable revision/version, and no compatibility fix has been required here for 180 days.
