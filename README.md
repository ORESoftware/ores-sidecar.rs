# ores-sidecar.rs

Shared Rust configuration and runtime-update **contract authority** for ORESoftware sidecars.

`ORESoftware/ores-sidecar.rs` owns the reusable `.ores-sidecar.toml`, full-snapshot, and ordered runtime-event contracts. `ores-otel/ores-otel-sidecar.rs` remains the canonical executable/inherited sidecar runtime implementation; product sidecars should import that runtime instead of copying health, receiver, or process machinery. Keeping those roles distinct prevents an executable consumer from silently becoming a second editable configuration authority.

## `.ores-sidecar.toml`

The repository-root `.ores-sidecar.toml` can describe one sidecar or a supervisor-owned set of sidecars with repeated `[[sidecars]]` tables. `SidecarConfig::resolve_one()` accepts an implicit default only when exactly one sidecar is present; `resolve_all()` returns a deterministic name-sorted fleet for supervisor use.

```toml
protocol = "ores.sidecar-config.v1"

[runtimeUpdates]
provider = "ores-redis-lru-cache"
lruConfigPath = ".ores-lru.toml"
role = "server"
cache = "runtime-env"

[[sidecars]]
name = "api"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "api"
runtimeKeys = ["REQUEST_TIMEOUT_MS"]
```

Listener settings are immutable process configuration. Runtime updates are a separate allowlisted overlay: each sidecar explicitly declares mutable keys, secret-looking environment keys are rejected, updates target one sidecar identity, and revisions are monotonic. Revision values are decimal strings on cross-runtime wires so JavaScript and other runtimes do not lose integer precision.

`runtimeUpdates.lruConfigPath` points to the existing `.ores-lru.toml` authority instead of duplicating Redis URLs, key prefixes, reconnect policy, or pub/sub settings here. The intended adapter is `ores-redis-lru-cache`'s `runtime-env` cache.

### Full snapshots

Simple consumers translate a reconciled backend snapshot into `RuntimeSnapshotUpdate` (`ores.sidecar-runtime.v1`). The preferred API is `RuntimeState::transition`, which returns an independent next state without mutating its source; `RuntimeState::apply` remains an imperative compatibility shell at long-lived runtime boundaries. Values are validated as one atomic allowlisted map and older/equal revisions cannot regress snapshot state.

### Ordered runtime events

Consumers that ingest backend events use `RuntimeEventState` and `RuntimeEventUpdate` (`ores.sidecar-runtime-event.v1`). The event contract supports:

- `upsert` for bounded allowlisted key/value patches;
- `delete` for bounded allowlisted key removals;
- `replace` for atomic replacement of the full runtime overlay;
- `invalidate` for clearing the overlay; and
- `resync` as an explicit reconciliation fence.

`RuntimeEventState::transition_event` and `transition_snapshot` are the preferred value-oriented APIs. They return fully independent next-state values; `apply_event` and `apply_snapshot` are thin imperative compatibility shells.

Events must advance exactly one revision at a time. A valid revision gap or `resync` makes event state sticky-stale without advancing the stored revision; later incremental events cannot repair it. An authoritative full snapshot at the **same or a newer revision** may repair local values and clear that fence. Malformed events are validated before duplicate detection, so an old replay cannot become accepted merely because its revision is stale. Secret-like keys, undeclared keys, duplicate keys/values, oversized values, noncanonical revisions, and unsafe cross-runtime integers fail before state mutation.

The event reducer deliberately remains transport/provider neutral. Redis credentials, reconnect behavior, pub/sub transport, and snapshot acquisition continue to belong to `.ores-lru.toml` and `ores-redis-lru-cache`.

## Contract authority

`contracts/main.tsp` and `contracts/authored.schema.json` are independently maintained peer authorities. CI runs `@oresoftware/typespec-json-schema-validator` (`tjsv`) fail-closed over both authorities and the instance corpus. Generated schemas remain evidence only; they are not a third authority.

The Rust implementation is executable admission for those contracts, not a replacement for either authored authority.

## Safety properties

- config files are capped at 256 KiB and reject unknown fields;
- sidecar identities and runtime namespaces are bounded ASCII segments;
- loopback-only sidecars cannot bind a non-loopback address;
- runtime config paths cannot be absolute or escape the repository root;
- runtime keys must be explicit uppercase environment-style names and may not look secret-bearing;
- full runtime snapshots and event operations are atomic;
- pure transition APIs leave source state unchanged and return independent state values;
- ordered events are shape-checked before duplicate/idempotency handling;
- revision gaps and explicit resync events fail closed into sticky reconciliation state;
- authoritative equal-or-newer snapshots can repair event-aware state without inventing a provider revision; and
- runtime diagnostics do not include rejected runtime values.
