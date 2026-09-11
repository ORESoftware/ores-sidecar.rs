#![forbid(unsafe_code)]

//! Shared, fail-closed configuration primitives for ORESoftware sidecars.
//!
//! `.ores-sidecar.toml` owns sidecar identity, immutable listener settings, and the
//! allowlist of values that may be overlaid at runtime. It deliberately does not
//! own Redis credentials or Redis transport settings: those stay in `.ores-lru.toml`.
//! `ores-redis-lru-cache` can translate its authoritative `runtime-env` snapshots
//! into [`RuntimeSnapshotUpdate`] values and apply them through [`RuntimeState`].

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::IpAddr,
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

pub const CONFIG_FILE_NAME: &str = ".ores-sidecar.toml";
pub const CONFIG_PROTOCOL: &str = "ores.sidecar-config.v1";
pub const RUNTIME_UPDATE_PROTOCOL: &str = "ores.sidecar-runtime.v1";
pub const RUNTIME_CACHE_NAME: &str = "runtime-env";
pub const MAX_CONFIG_FILE_BYTES: usize = 256 * 1024;
pub const MAX_SIDECARS: usize = 64;
pub const MAX_RUNTIME_KEYS: usize = 128;
pub const MAX_RUNTIME_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_SAFE_REVISION: u64 = 9_007_199_254_740_991;

const SENSITIVE_KEY_PARTS: [&str; 6] = [
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PRIVATE_KEY",
    "DATABASE_URL",
    "CREDENTIAL",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum RuntimeUpdateProvider {
    #[serde(rename = "ores-redis-lru-cache")]
    OresRedisLruCache,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeUpdateRole {
    Server,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeUpdates {
    pub provider: RuntimeUpdateProvider,
    pub lru_config_path: String,
    pub role: RuntimeUpdateRole,
    pub cache: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Sidecar {
    pub name: String,
    pub enabled: bool,
    pub bind_ip: String,
    pub bind_port: u16,
    pub loopback_only: bool,
    pub runtime_namespace: String,
    pub runtime_keys: Vec<String>,
}

impl Sidecar {
    pub fn bind_ip_addr(&self) -> Result<IpAddr> {
        self.bind_ip
            .parse()
            .map_err(|_| Error::InvalidConfig("sidecars.bindIp must be an IP address"))
    }

    pub fn bind_address(&self) -> String {
        format!("{}:{}", self.bind_ip, self.bind_port)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SidecarConfig {
    protocol: String,
    pub runtime_updates: RuntimeUpdates,
    pub sidecars: Vec<Sidecar>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeUpdatePlan {
    pub sidecar: String,
    pub runtime_namespace: String,
    pub provider: RuntimeUpdateProvider,
    pub lru_config_path: PathBuf,
    pub role: RuntimeUpdateRole,
    pub cache: String,
    pub allowed_keys: BTreeSet<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeValue {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeSnapshotUpdate {
    pub protocol: String,
    pub sidecar: String,
    /// Canonical non-negative decimal. A string avoids cross-runtime integer precision loss.
    pub revision: String,
    pub values: Vec<RuntimeValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSnapshot {
    pub revision: u64,
    pub values: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeApplyOutcome {
    Applied { previous: u64, current: u64 },
    StaleIgnored { current: u64, incoming: u64 },
}

/// Result of applying one runtime update without mutating the source state.
///
/// `next` owns independent strings, collections, and snapshot values. It can be
/// retained, mutated through the compatibility shell, or moved to another task
/// without aliasing the `RuntimeState` used to create it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeTransition {
    pub next: RuntimeState,
    pub outcome: RuntimeApplyOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeState {
    sidecar: String,
    allowed_keys: BTreeSet<String>,
    snapshot: RuntimeSnapshot,
}

struct RuntimeStateChange {
    next_snapshot: Option<RuntimeSnapshot>,
    outcome: RuntimeApplyOutcome,
}

impl SidecarConfig {
    pub fn parse(input: &str) -> Result<Self> {
        if input.len() > MAX_CONFIG_FILE_BYTES {
            return Err(Error::InvalidConfig("config file exceeds 256 KiB"));
        }
        let parsed: Self = basic_toml::from_str(input)
            .map_err(|_| Error::InvalidConfig("TOML syntax or type mismatch"))?;
        parsed.validate()?;
        Ok(parsed)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path).map_err(|source| Error::ConfigIo {
            path: path.to_path_buf(),
            source,
        })?;
        if bytes.len() > MAX_CONFIG_FILE_BYTES {
            return Err(Error::InvalidConfig("config file exceeds 256 KiB"));
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| Error::InvalidConfig("config file must be UTF-8"))?;
        Self::parse(text)
    }

    pub fn load_repo_root(repo_root: impl AsRef<Path>) -> Result<Self> {
        Self::load(repo_root.as_ref().join(CONFIG_FILE_NAME))
    }

    pub fn protocol(&self) -> &str {
        &self.protocol
    }

    pub fn validate(&self) -> Result<()> {
        if self.protocol != CONFIG_PROTOCOL {
            return Err(Error::InvalidConfig("unsupported sidecar config protocol"));
        }
        validate_runtime_updates(&self.runtime_updates)?;
        if self.sidecars.is_empty() || self.sidecars.len() > MAX_SIDECARS {
            return Err(Error::InvalidConfig("sidecars must contain 1..=64 entries"));
        }

        let mut names = BTreeSet::new();
        for sidecar in &self.sidecars {
            validate_segment(&sidecar.name, "sidecars.name")?;
            if !names.insert(sidecar.name.as_str()) {
                return Err(Error::DuplicateSidecar(sidecar.name.clone()));
            }
            if sidecar.bind_port == 0 {
                return Err(Error::InvalidConfig("sidecars.bindPort must be non-zero"));
            }
            let bind_ip = sidecar.bind_ip_addr()?;
            if sidecar.loopback_only && !bind_ip.is_loopback() {
                return Err(Error::NonLoopbackBind(sidecar.name.clone()));
            }
            validate_segment(&sidecar.runtime_namespace, "sidecars.runtimeNamespace")?;
            if sidecar.runtime_keys.len() > MAX_RUNTIME_KEYS {
                return Err(Error::InvalidConfig(
                    "sidecars.runtimeKeys must contain at most 128 entries",
                ));
            }
            let mut runtime_keys = BTreeSet::new();
            for key in &sidecar.runtime_keys {
                validate_runtime_key(key)?;
                if !runtime_keys.insert(key.as_str()) {
                    return Err(Error::DuplicateRuntimeKey {
                        sidecar: sidecar.name.clone(),
                        key: key.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Resolve every configured sidecar in deterministic name order. This is the
    /// supervisor/all-sidecars path.
    pub fn resolve_all(&self) -> Result<Vec<Sidecar>> {
        self.validate()?;
        let mut sidecars = self.sidecars.clone();
        sidecars.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(sidecars)
    }

    /// Resolve one sidecar. A selector may be omitted only when the file contains
    /// exactly one sidecar, preventing a multi-sidecar file from choosing implicitly.
    pub fn resolve_one(&self, selector: Option<&str>) -> Result<Sidecar> {
        self.validate()?;
        if let Some(selector) = selector {
            return self
                .sidecars
                .iter()
                .find(|sidecar| sidecar.name == selector)
                .cloned()
                .ok_or_else(|| Error::UnknownSidecar(selector.to_owned()));
        }
        if self.sidecars.len() != 1 {
            return Err(Error::AmbiguousSidecarSelection);
        }
        Ok(self.sidecars[0].clone())
    }

    pub fn runtime_plan_for(&self, sidecar: &Sidecar) -> Result<RuntimeUpdatePlan> {
        self.validate()?;
        if !self.sidecars.iter().any(|entry| entry.name == sidecar.name) {
            return Err(Error::UnknownSidecar(sidecar.name.clone()));
        }
        Ok(RuntimeUpdatePlan {
            sidecar: sidecar.name.clone(),
            runtime_namespace: sidecar.runtime_namespace.clone(),
            provider: self.runtime_updates.provider,
            lru_config_path: PathBuf::from(&self.runtime_updates.lru_config_path),
            role: self.runtime_updates.role,
            cache: self.runtime_updates.cache.clone(),
            allowed_keys: sidecar.runtime_keys.iter().cloned().collect(),
        })
    }
}

impl RuntimeState {
    pub fn new(plan: &RuntimeUpdatePlan) -> Self {
        Self {
            sidecar: plan.sidecar.clone(),
            allowed_keys: plan.allowed_keys.iter().cloned().collect(),
            snapshot: RuntimeSnapshot {
                revision: 0,
                values: BTreeMap::new(),
            },
        }
    }

    pub fn snapshot(&self) -> &RuntimeSnapshot {
        &self.snapshot
    }

    /// Return a fully independent state value.
    fn independent(&self) -> Self {
        Self {
            sidecar: self.sidecar.clone(),
            allowed_keys: self.allowed_keys.iter().cloned().collect(),
            snapshot: RuntimeSnapshot {
                revision: self.snapshot.revision,
                values: self
                    .snapshot
                    .values
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            },
        }
    }

    /// Compute the next runtime state without mutating `self`.
    ///
    /// This is the preferred domain/state-machine API. Every successful return
    /// owns a fully independent `RuntimeState`; even a stale update produces a
    /// fresh value so callers can reason in `next = transition(current)` terms.
    pub fn transition(&self, update: RuntimeSnapshotUpdate) -> Result<RuntimeTransition> {
        let change = self.plan_update(update)?;
        let next = match change.next_snapshot {
            Some(snapshot) => Self {
                sidecar: self.sidecar.clone(),
                allowed_keys: self.allowed_keys.iter().cloned().collect(),
                snapshot,
            },
            None => self.independent(),
        };
        Ok(RuntimeTransition {
            next,
            outcome: change.outcome,
        })
    }

    /// Apply a complete runtime snapshot to this long-lived runtime holder.
    ///
    /// This compatibility shell is intentionally imperative. Runtime updates may
    /// arrive repeatedly, while sidecar identity and the allowlist are immutable;
    /// rebuilding those metadata allocations for every update would add copy work
    /// with no semantic benefit. The mutation is therefore limited to swapping in
    /// the whole freshly constructed snapshot produced by the pure transition
    /// planner. Use [`Self::transition`] when alias-free value flow is preferred.
    pub fn apply(&mut self, update: RuntimeSnapshotUpdate) -> Result<RuntimeApplyOutcome> {
        let change = self.plan_update(update)?;
        if let Some(snapshot) = change.next_snapshot {
            self.snapshot = snapshot;
        }
        Ok(change.outcome)
    }

    fn plan_update(&self, update: RuntimeSnapshotUpdate) -> Result<RuntimeStateChange> {
        if update.protocol != RUNTIME_UPDATE_PROTOCOL {
            return Err(Error::InvalidRuntimeUpdate("unsupported runtime protocol"));
        }
        if update.sidecar != self.sidecar {
            return Err(Error::RuntimeTargetMismatch {
                expected: self.sidecar.clone(),
                actual: update.sidecar,
            });
        }
        let revision = parse_revision(&update.revision)?;
        if revision <= self.snapshot.revision {
            return Ok(RuntimeStateChange {
                next_snapshot: None,
                outcome: RuntimeApplyOutcome::StaleIgnored {
                    current: self.snapshot.revision,
                    incoming: revision,
                },
            });
        }
        if update.values.len() > MAX_RUNTIME_KEYS {
            return Err(Error::InvalidRuntimeUpdate(
                "runtime snapshot contains too many values",
            ));
        }

        let values = update.values.into_iter().try_fold(
            BTreeMap::new(),
            |mut next, entry| -> Result<BTreeMap<String, String>> {
                validate_runtime_key(&entry.key)?;
                if !self.allowed_keys.contains(&entry.key) {
                    return Err(Error::RuntimeKeyNotAllowed(entry.key));
                }
                if entry.value.len() > MAX_RUNTIME_VALUE_BYTES {
                    return Err(Error::InvalidRuntimeUpdate("runtime value exceeds 64 KiB"));
                }
                if next.insert(entry.key.clone(), entry.value).is_some() {
                    return Err(Error::DuplicateRuntimeValue(entry.key));
                }
                Ok(next)
            },
        )?;

        Ok(RuntimeStateChange {
            next_snapshot: Some(RuntimeSnapshot { revision, values }),
            outcome: RuntimeApplyOutcome::Applied {
                previous: self.snapshot.revision,
                current: revision,
            },
        })
    }
}

pub fn is_sensitive_runtime_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    SENSITIVE_KEY_PARTS.iter().any(|part| upper.contains(part))
}

pub fn is_runtime_key_allowed(key: &str) -> bool {
    is_env_key(key) && !is_sensitive_runtime_key(key)
}

fn validate_runtime_updates(runtime: &RuntimeUpdates) -> Result<()> {
    if runtime.cache != RUNTIME_CACHE_NAME {
        return Err(Error::InvalidConfig(
            "runtimeUpdates.cache must be runtime-env",
        ));
    }
    validate_safe_relative_path(&runtime.lru_config_path)?;
    Ok(())
}

fn validate_safe_relative_path(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 256 {
        return Err(Error::InvalidConfig(
            "runtimeUpdates.lruConfigPath must be a bounded relative path",
        ));
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(Error::InvalidConfig(
            "runtimeUpdates.lruConfigPath must stay inside the repository root",
        ));
    }
    Ok(())
}

fn validate_segment(value: &str, field: &'static str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(Error::InvalidConfig(field));
    }
    Ok(())
}

fn validate_runtime_key(key: &str) -> Result<()> {
    if !is_env_key(key) {
        return Err(Error::InvalidRuntimeKey(key.to_owned()));
    }
    if is_sensitive_runtime_key(key) {
        return Err(Error::SensitiveRuntimeKey(key.to_owned()));
    }
    Ok(())
}

fn is_env_key(value: &str) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == b'_')
        && value.len() <= 128
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn parse_revision(value: &str) -> Result<u64> {
    if value.is_empty()
        || value.len() > 16
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(Error::InvalidRuntimeUpdate(
            "revision must be canonical decimal",
        ));
    }
    let revision = value
        .parse::<u64>()
        .map_err(|_| Error::InvalidRuntimeUpdate("revision is out of range"))?;
    if revision > MAX_SAFE_REVISION {
        return Err(Error::InvalidRuntimeUpdate(
            "revision exceeds the cross-runtime safe integer bound",
        ));
    }
    Ok(revision)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("sidecar config is invalid: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to read sidecar config {path}: {source}")]
    ConfigIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("duplicate sidecar name: {0}")]
    DuplicateSidecar(String),
    #[error("sidecar {0} requires a loopback bind")]
    NonLoopbackBind(String),
    #[error("multiple sidecars are configured; select one explicitly")]
    AmbiguousSidecarSelection,
    #[error("unknown sidecar: {0}")]
    UnknownSidecar(String),
    #[error("invalid runtime key: {0}")]
    InvalidRuntimeKey(String),
    #[error("secret-bearing runtime keys are forbidden: {0}")]
    SensitiveRuntimeKey(String),
    #[error("duplicate runtime key {key} in sidecar {sidecar}")]
    DuplicateRuntimeKey { sidecar: String, key: String },
    #[error("runtime snapshot is invalid: {0}")]
    InvalidRuntimeUpdate(&'static str),
    #[error("runtime update targeted {actual}, expected {expected}")]
    RuntimeTargetMismatch { expected: String, actual: String },
    #[error("runtime key is not allowlisted by .ores-sidecar.toml: {0}")]
    RuntimeKeyNotAllowed(String),
    #[error("duplicate runtime value: {0}")]
    DuplicateRuntimeValue(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn config(sidecars: &str) -> String {
        format!(
            r#"protocol = "{CONFIG_PROTOCOL}"

[runtimeUpdates]
provider = "ores-redis-lru-cache"
lruConfigPath = ".ores-lru.toml"
role = "server"
cache = "runtime-env"

{sidecars}
"#
        )
    }

    fn runtime_state() -> RuntimeState {
        let parsed = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "ores-chat"
runtimeKeys = ["PROVIDER_TIMEOUT_MS"]"#,
        ))
        .unwrap();
        let sidecar = parsed.resolve_one(None).unwrap();
        RuntimeState::new(&parsed.runtime_plan_for(&sidecar).unwrap())
    }

    fn runtime_update(revision: u64, value: Option<&str>) -> RuntimeSnapshotUpdate {
        RuntimeSnapshotUpdate {
            protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
            sidecar: "chat".to_owned(),
            revision: revision.to_string(),
            values: value
                .into_iter()
                .map(|value| RuntimeValue {
                    key: "PROVIDER_TIMEOUT_MS".to_owned(),
                    value: value.to_owned(),
                })
                .collect(),
        }
    }

    #[test]
    fn one_sidecar_is_the_implicit_default() {
        let parsed = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "ores-chat"
runtimeKeys = ["PROVIDER_TIMEOUT_MS"]"#,
        ))
        .unwrap();
        assert_eq!(parsed.resolve_one(None).unwrap().name, "chat");
    }

    #[test]
    fn multi_sidecar_config_supports_all_and_requires_explicit_single_selection() {
        let parsed = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "worker"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7420
loopbackOnly = true
runtimeNamespace = "worker"
runtimeKeys = ["WORKER_BATCH_SIZE"]

[[sidecars]]
name = "api"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "api"
runtimeKeys = ["REQUEST_TIMEOUT_MS"]"#,
        ))
        .unwrap();
        assert!(matches!(
            parsed.resolve_one(None),
            Err(Error::AmbiguousSidecarSelection)
        ));
        assert_eq!(parsed.resolve_one(Some("worker")).unwrap().bind_port, 7420);
        let all = parsed.resolve_all().unwrap();
        assert_eq!(
            all.iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["api", "worker"]
        );
    }

    #[test]
    fn loopback_only_entries_fail_closed() {
        let result = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "0.0.0.0"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "chat"
runtimeKeys = []"#,
        ));
        assert!(matches!(result, Err(Error::NonLoopbackBind(name)) if name == "chat"));
    }

    #[test]
    fn path_traversal_is_rejected() {
        let input = config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "chat"
runtimeKeys = []"#,
        )
        .replace(".ores-lru.toml", "../.ores-lru.toml");
        assert!(SidecarConfig::parse(&input).is_err());
    }

    #[test]
    fn runtime_keys_reject_secret_names() {
        let result = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "chat"
runtimeKeys = ["API_TOKEN"]"#,
        ));
        assert!(matches!(result, Err(Error::SensitiveRuntimeKey(key)) if key == "API_TOKEN"));
    }

    #[test]
    fn runtime_snapshots_are_allowlisted_and_monotonic() {
        let mut state = runtime_state();

        let applied = state.apply(runtime_update(7, Some("2500"))).unwrap();
        assert_eq!(
            applied,
            RuntimeApplyOutcome::Applied {
                previous: 0,
                current: 7
            }
        );

        let stale = state.apply(runtime_update(6, None)).unwrap();
        assert_eq!(
            stale,
            RuntimeApplyOutcome::StaleIgnored {
                current: 7,
                incoming: 6
            }
        );
        assert_eq!(
            state
                .snapshot()
                .values
                .get("PROVIDER_TIMEOUT_MS")
                .map(String::as_str),
            Some("2500")
        );
    }

    #[test]
    fn functional_transition_returns_independent_state_without_mutating_source() {
        let source = runtime_state();
        let transition = source.transition(runtime_update(7, Some("2500"))).unwrap();

        assert_eq!(source.snapshot().revision, 0);
        assert!(source.snapshot().values.is_empty());
        assert_eq!(transition.next.snapshot().revision, 7);
        assert_eq!(
            transition
                .next
                .snapshot()
                .values
                .get("PROVIDER_TIMEOUT_MS")
                .map(String::as_str),
            Some("2500")
        );

        let mut evolved = transition.next;
        evolved.apply(runtime_update(8, Some("3000"))).unwrap();
        assert_eq!(source.snapshot().revision, 0);
        assert_eq!(evolved.snapshot().revision, 8);
    }

    #[test]
    fn stale_functional_transition_returns_fresh_value_and_preserves_source() {
        let seeded = runtime_state()
            .transition(runtime_update(7, Some("2500")))
            .unwrap()
            .next;
        let transition = seeded.transition(runtime_update(6, None)).unwrap();

        assert_eq!(
            transition.outcome,
            RuntimeApplyOutcome::StaleIgnored {
                current: 7,
                incoming: 6
            }
        );
        assert_eq!(transition.next, seeded);

        let mut evolved = transition.next;
        evolved.apply(runtime_update(8, Some("3000"))).unwrap();
        assert_eq!(seeded.snapshot().revision, 7);
        assert_eq!(
            seeded
                .snapshot()
                .values
                .get("PROVIDER_TIMEOUT_MS")
                .map(String::as_str),
            Some("2500")
        );
    }

    #[test]
    fn imperative_shell_matches_functional_transition() {
        let source = runtime_state();
        let expected = source.transition(runtime_update(7, Some("2500"))).unwrap();
        let mut applied = source;
        let outcome = applied.apply(runtime_update(7, Some("2500"))).unwrap();

        assert_eq!(outcome, expected.outcome);
        assert_eq!(applied, expected.next);
    }

    #[test]
    fn undeclared_runtime_values_are_rejected_without_partial_application() {
        let mut state = runtime_state();
        let result = state.apply(RuntimeSnapshotUpdate {
            protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
            sidecar: "chat".to_owned(),
            revision: "1".to_owned(),
            values: vec![RuntimeValue {
                key: "UNDECLARED_FLAG".to_owned(),
                value: "true".to_owned(),
            }],
        });
        assert!(matches!(
            result,
            Err(Error::RuntimeKeyNotAllowed(key)) if key == "UNDECLARED_FLAG"
        ));
        assert_eq!(state.snapshot().revision, 0);
    }

    #[test]
    fn runtime_update_json_uses_precision_safe_revision_strings() {
        let update = RuntimeSnapshotUpdate {
            protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
            sidecar: "chat".to_owned(),
            revision: MAX_SAFE_REVISION.to_string(),
            values: Vec::new(),
        };
        let json = serde_json::to_value(update).unwrap();
        assert_eq!(json["revision"], MAX_SAFE_REVISION.to_string());
    }
}
