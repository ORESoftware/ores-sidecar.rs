# Compatibility consumer inventory

Inventory date: 2026-09-14.

The fleet search found current references that still depend on this compatibility boundary or its `.ores-sidecar.toml` contract:

- `ORESoftware/ores-cli` — audits `.ores-sidecar.toml` and its runtime invariants.
- `ORESoftware/admin-api-server-template.rs` — embeds/validates the config and records an immutable `ores-sidecar.rs` authority revision.
- `ORESoftware/admin-web-server-template.rs` — embeds/validates the config and records an immutable `ores-sidecar.rs` authority revision.

These consumers need the config shape, validation rules, sidecar identity/listener constraints, and runtime-update snapshot semantics while migration proceeds. New shared sidecar runtime implementation must not be added here; it belongs in `ores-otel/ores-otel-sidecar.rs`.

Consumers should pin this compatibility repository by immutable commit or released patch version. When a consumer switches to the canonical runtime, keep `.ores-sidecar.toml` wire/config compatibility until the peer TypeSpec/JSON Schema corpus is admitted there as well.
