# sidecar observability boundary

Driver: `ORESoftware/ores-docs#5`

This is a bounded review contract for an independently mergeable slice of the driver issue. It does not claim full rollout or implementation.

## Invariants

- Emit structured JSON/OTLP with trace/span, service, repository, environment, and commit metadata.
- Keep error fingerprints versioned and exclude occurrence-specific trace/request/timestamp fields from the hash.
- Never copy sensitive log bodies into issue/ticket automation by default.
- Bound local buffering/retry behavior so exporter failure cannot exhaust the host.

## Verification

- Bind evidence to the exact PR/source revision.
- Add or retain fail-closed negative coverage around untrusted inputs.
- Keep credentials and sensitive payloads out of fixtures, logs, and review text.
- Treat missing or zero-step CI as missing evidence.

## Non-goals

This contract does not bypass branch protection or create new secret-delivery channels.
