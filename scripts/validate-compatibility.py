#!/usr/bin/env python3
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
policy = json.loads((ROOT / "COMPATIBILITY.json").read_text())
readme = (ROOT / "README.md").read_text()
migration = (ROOT / "MIGRATION.md").read_text()
consumers = (ROOT / "CONSUMERS.md").read_text()
cargo = (ROOT / "Cargo.toml").read_text()
errors = []

def require(condition: bool, message: str) -> None:
    if not condition:
        errors.append(message)

require(policy.get("repository") == "ORESoftware/ores-sidecar.rs", "repository mismatch")
require(policy.get("status") == "compatibility", "status must be compatibility")
require(policy.get("canonical_runtime_repository") == "ores-otel/ores-otel-sidecar.rs", "canonical runtime mismatch")
require(policy.get("canonical_runtime_work_goes_here") is False, "canonical runtime work must stay out of this repository")
require(policy.get("config_contract", {}).get("filename") == ".ores-sidecar.toml", "config filename mismatch")
require(policy.get("config_contract", {}).get("protocol") == "ores.sidecar-config.v1", "config protocol mismatch")

for authority in policy.get("config_contract", {}).get("peer_authorities", []):
    require((ROOT / authority).is_file(), f"missing peer authority: {authority}")

allowed = set(policy.get("allowed_source_paths", []))
actual = {p.relative_to(ROOT).as_posix() for p in (ROOT / "src").rglob("*.rs")}
require(actual <= allowed, f"new Rust implementation surface is forbidden here: {sorted(actual - allowed)}")

match = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
require(match is not None, "Cargo package version not found")
if match:
    version = match.group(1)
    baseline = policy.get("release_policy", {}).get("baseline_version")
    require(version == baseline, f"compatibility crate version advanced from frozen baseline {baseline} to {version}")

require("ores-otel/ores-otel-sidecar.rs" in readme, "README must name canonical runtime")
require("immutable" in migration.lower(), "migration guide must require immutable pinning")
for consumer in ("ORESoftware/ores-cli", "ORESoftware/admin-api-server-template.rs", "ORESoftware/admin-web-server-template.rs"):
    require(consumer in consumers, f"consumer inventory missing {consumer}")

if errors:
    for error in errors:
        print(f"compatibility-policy: {error}", file=sys.stderr)
    raise SystemExit(1)
print("ores-sidecar compatibility policy: ok")
