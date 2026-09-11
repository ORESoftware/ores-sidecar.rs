# Historical Sidecar contract snapshot

This directory is retained as a compatibility snapshot for the original `ORESoftware/ores-sidecar.rs` implementation. It is **not** the current `.ores-sidecar.toml` authority for new ORES sidecars.

The canonical shared library and current peer authorities are owned by `ores-otel/ores-otel-sidecar.rs`:

- `contracts/sidecar/main.tsp` — independently authored TypeSpec authority;
- `contracts/sidecar/authored.schema.json` — independently authored JSON Schema Draft 2020-12 authority;
- `.ores-sidecar.toml` — repository example of the current `ores.sidecar/config/v1` file shape.

The canonical contract was introduced in `ores-otel/ores-otel-sidecar.rs` before this compatibility repository's current snapshot. New consumers, static analyzers, code generators, documentation, and migration work must follow the canonical `ores-otel` contract. Do not treat this directory's `protocol = "ores.sidecar-config.v1"` / `runtimeUpdates` shape as a peer authority for new code.

Existing tests in this repository may continue validating the historical compatibility implementation. Their passing status is compatibility evidence only and must not be interpreted as authority precedence over `ores-otel/ores-otel-sidecar.rs`.
