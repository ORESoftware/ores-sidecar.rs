
Note: canonical lib is in - ores-otel/ores-otel-sidecar.rs - The repo-local control file confirms the same thing even more explicitly: ores-otel/ores-otel-sidecar.rs is the canonical shared library, and product sidecars are supposed to import it rather than copy config/health/runtime.

# ores-sidecar.rs

Shared Rust configuration and runtime-update contracts for ORESoftware sidecars.

The reusable `.ores-sidecar.toml` contract, runtime overlay policy, and cross-runtime schema parity checks live here. Service-specific sidecars consume this crate rather than reimplementing shared configuration semantics.
