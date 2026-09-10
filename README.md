# ores-sidecar.rs

Shared Rust configuration and runtime-update contracts for ORESoftware sidecars.

The reusable `.ores-sidecar.toml` contract, runtime overlay policy, and cross-runtime schema parity checks live here. Service-specific sidecars consume this crate rather than reimplementing shared configuration semantics.
