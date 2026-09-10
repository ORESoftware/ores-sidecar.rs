# ores-sidecar.rs

Shared Rust configuration and runtime-update contracts for ORESoftware sidecars.

## `.ores-sidecar.toml`

The repository-root `.ores-sidecar.toml` file can describe one sidecar or a supervisor-owned set of sidecars using the same repeated `[[sidecars]]` table. `SidecarConfig::resolve_one()` accepts an implicit default only when exactly one sidecar is present; `resolve_all()` returns a deterministic name-sorted fleet for supervisor use.

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

Listener settings are immutable process configuration. Runtime updates are a separate full-snapshot overlay: each sidecar explicitly allowlists mutable keys, secret-looking environment keys are rejected, updates target one sidecar identity, and revisions are monotonic. Revision values are decimal strings on the cross-runtime wire so JavaScript and other runtimes do not lose integer precision.

`runtimeUpdates.lruConfigPath` points to the existing `.ores-lru.toml` authority rather than duplicating Redis URLs, key prefixes, reconnect policy, or pub/sub settings here. The intended adapter is `ores-redis-lru-cache`'s `runtime-env` cache: it reconciles Redis/pubsub state first and then applies an authoritative snapshot through `RuntimeState::apply`.

## Contract authority

`contracts/main.tsp` and `contracts/authored.schema.json` are independently maintained peer authorities. CI runs `@oresoftware/typespec-json-schema-validator` (`tjsv`) fail-closed over both authorities and the instance corpus. Generated schemas remain evidence only; they are not a third authority.

## Safety properties

- config files are capped at 256 KiB and reject unknown fields;
- sidecar identities and runtime namespaces are bounded ASCII segments;
- loopback-only sidecars cannot bind a non-loopback address;
- runtime config paths cannot be absolute or escape the repository root;
- runtime keys must be explicit uppercase environment-style names and may not look secret-bearing;
- runtime snapshots are applied atomically and older/equal revisions cannot regress current state.
