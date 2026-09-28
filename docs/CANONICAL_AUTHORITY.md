# Canonical Sidecar authority

`ORESoftware/ores-sidecar.rs` is a compatibility repository. The canonical shared Sidecar runtime is `ores-otel/ores-otel-sidecar.rs`.

## Current contract

New `.ores-sidecar.toml` consumers must use the canonical `ores.sidecar/config/v1` shape from the `ores-otel` peer authorities. The current root fields are `schema` and `sidecars`; each Sidecar uses `id` and nested `runtime_updates` policy. The canonical repository's TypeSpec and independently authored Draft 2020-12 JSON Schema are release-vetoing peers and TJSV is downstream admission evidence.

## Compatibility boundary

This repository retains an older `protocol` / `runtimeUpdates` / `name`-based shape only so existing compatibility code can be audited and migrated deliberately. It must not be copied into new products, fleet linters, or generated contracts. If the historical and canonical shapes disagree, do not pick the historical shape as a winner; migrate or isolate the compatibility consumer and keep the canonical authority explicit.

## Static-analysis rule

Repository-wide tooling such as `ORESoftware/ores-cli` should bind `.ores-sidecar.toml` checks to the canonical `ores-otel/ores-otel-sidecar.rs` TypeSpec and authored JSON Schema. Compatibility checks for this repository should be explicitly scoped as legacy evidence rather than presented as the current domain contract.
