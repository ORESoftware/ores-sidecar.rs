#![forbid(unsafe_code)]

//! Shared, fail-closed configuration primitives for ORESoftware sidecars.
//!
//! `.ores-sidecar.toml` owns sidecar identity, immutable listener settings, and the
//! allowlist of values that may be overlaid at runtime. It deliberately does not
//! own Redis credentials or Redis transport settings: those stay in `.ores-lru.toml`.
//! `ores-redis-lru-cache` can translate its authoritative `runtime-env` snapshots
//! into [`RuntimeSnapshotUpdate`] values and ordered provider events into
//! [`RuntimeEventUpdate`] values. Consumers that need gap detection and resync fencing
//! use [`RuntimeEventState`]; simple full-snapshot consumers may continue using
//! [`RuntimeState`].

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
pub const RUNTIME_EVENT_PROTOCOL: &str = "ores.sidecar-runtime-event.v1";
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeEventOperation {
    Upsert,
    Delete,
    Replace,
    Invalidate,
    Resync,
}

/// Ordered provider event after the transport adapter has authenticated and decoded it.
///
/// Shape is validated again by [`RuntimeEventState::apply_event`]: upserts carry values,
/// deletes carry keys, replaces carry only values, and invalidate/resync carry neither.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeEventUpdate {
    pub protocol: String,
    pub sidecar: String,
    /// Canonical positive decimal for event revisions.
    pub revision: String,
    pub operation: RuntimeEventOperation,
    #[serde(default)]
    pub values: Vec<RuntimeValue>,
    #[serde(default)]
    pub keys: Vec<String>,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEventOutcome {
    Applied { previous: u64, current: u64 },
    Duplicate { current: u64, incoming: u64 },
    ReconcileRequired { current: u64, incoming: u64 },
}

#[derive(Clone, Debug)]
pub struct RuntimeState {
    sidecar: String,
    allowed_keys: BTreeSet<String>,
    snapshot: RuntimeSnapshot,
}

/// Event-aware runtime overlay state.
///
/// A revision gap or explicit `resync` event makes the state sticky-stale. While stale,
/// incremental events never mutate state even if the missing revision later arrives. An
/// authoritative full snapshot passed to [`Self::apply_snapshot`] is required to repair it.
#[derive(Clone, Debug)]
pub struct RuntimeEventState {
    sidecar: String,
    allowed_keys: BTreeSet<String>,
    snapshot: RuntimeSnapshot,
    stale: bool,
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
            allowed_keys: plan.allowed_keys.clone(),
            snapshot: RuntimeSnapshot {
                revision: 0,
                values: BTreeMap::new(),
            },
        }
    }

    pub fn snapshot(&self) -> &RuntimeSnapshot {
        &self.snapshot
    }

    /// Apply a complete runtime snapshot. `ores-redis-lru-cache` should reconcile
    /// event gaps before calling this API, then pass the authoritative `runtime-env`
    /// snapshot and its revision. Older/equal revisions cannot regress local state.
    pub fn apply(&mut self, update: RuntimeSnapshotUpdate) -> Result<RuntimeApplyOutcome> {
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
            return Ok(RuntimeApplyOutcome::StaleIgnored {
                current: self.snapshot.revision,
                incoming: revision,
            });
        }
        let next = validated_runtime_values(&self.allowed_keys, update.values)?;

        let previous = self.snapshot.revision;
        self.snapshot = RuntimeSnapshot {
            revision,
            values: next,
        };
        Ok(RuntimeApplyOutcome::Applied {
            previous,
            current: revision,
        })
    }
}

impl RuntimeEventState {
    pub fn new(plan: &RuntimeUpdatePlan) -> Self {
        Self {
            sidecar: plan.sidecar.clone(),
            allowed_keys: plan.allowed_keys.clone(),
            snapshot: RuntimeSnapshot {
                revision: 0,
                values: BTreeMap::new(),
            },
            stale: false,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> &RuntimeSnapshot {
        &self.snapshot
    }

    #[must_use]
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// Apply an authoritative full snapshot and clear sticky stale state.
    ///
    /// Equal revisions are accepted deliberately: a backend reconciliation may need to
    /// repair locally divergent values without inventing a new provider revision.
    pub fn apply_snapshot(
        &mut self,
        update: RuntimeSnapshotUpdate,
    ) -> Result<RuntimeApplyOutcome> {
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
        if revision < self.snapshot.revision {
            return Ok(RuntimeApplyOutcome::StaleIgnored {
                current: self.snapshot.revision,
                incoming: revision,
            });
        }
        let next = validated_runtime_values(&self.allowed_keys, update.values)?;
        let previous = self.snapshot.revision;
        self.snapshot = RuntimeSnapshot {
            revision,
            values: next,
        };
        self.stale = false;
        Ok(RuntimeApplyOutcome::Applied {
            previous,
            current: revision,
        })
    }

    /// Apply one ordered provider event atomically.
    ///
    /// Malformed events are rejected without changing revision or stale state. A valid
    /// revision gap or explicit resync marks the state stale before returning
    /// `ReconcileRequired`; only an authoritative snapshot can clear that fence.
    pub fn apply_event(&mut self, update: RuntimeEventUpdate) -> Result<RuntimeEventOutcome> {
        if update.protocol != RUNTIME_EVENT_PROTOCOL {
            return Err(Error::InvalidRuntimeEvent("unsupported runtime event protocol"));
        }
        if update.sidecar != self.sidecar {
            return Err(Error::RuntimeTargetMismatch {
                expected: self.sidecar.clone(),
                actual: update.sidecar,
            });
        }
        let revision = parse_positive_revision(&update.revision)?;
        validate_event_shape(&update)?;

        let validated_values = match update.operation {
            RuntimeEventOperation::Upsert | RuntimeEventOperation::Replace => {
                Some(validated_runtime_values(&self.allowed_keys, update.values.clone())?)
            }
            RuntimeEventOperation::Delete
            | RuntimeEventOperation::Invalidate
            | RuntimeEventOperation::Resync => None,
        };
        let validated_keys = if update.operation == RuntimeEventOperation::Delete {
            Some(validated_runtime_keys(&self.allowed_keys, &update.keys)?)
        } else {
            None
        };

        if revision <= self.snapshot.revision {
            return Ok(RuntimeEventOutcome::Duplicate {
                current: self.snapshot.revision,
                incoming: revision,
            });
        }
        if self.stale {
            return Ok(RuntimeEventOutcome::ReconcileRequired {
                current: self.snapshot.revision,
                incoming: revision,
            });
        }
        if revision != self.snapshot.revision.saturating_add(1) {
            self.stale = true;
            return Ok(RuntimeEventOutcome::ReconcileRequired {
                current: self.snapshot.revision,
                incoming: revision,
            });
        }
        if update.operation == RuntimeEventOperation::Resync {
            self.stale = true;
            return Ok(RuntimeEventOutcome::ReconcileRequired {
                current: self.snapshot.revision,
                incoming: revision,
            });
        }

        let previous = self.snapshot.revision;
        let mut next = self.snapshot.values.clone();
        match update.operation {
            RuntimeEventOperation::Upsert => {
                for (key, value) in validated_values.expect("upsert values validated") {
                    next.insert(key, value);
                }
            }
            RuntimeEventOperation::Delete => {
                for key in validated_keys.expect("delete keys validated") {
                    next.remove(&key);
                }
            }
            RuntimeEventOperation::Replace => {
                next = validated_values.expect("replacement values validated");
            }
            RuntimeEventOperation::Invalidate => next.clear(),
            RuntimeEventOperation::Resync => unreachable!("resync returns before mutation"),
        }
        self.snapshot = RuntimeSnapshot {
            revision,
            values: next,
        };
        Ok(RuntimeEventOutcome::Applied {
            previous,
            current: revision,
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

fn validated_runtime_values(
    allowed_keys: &BTreeSet<String>,
    values: Vec<RuntimeValue>,
) -> Result<BTreeMap<String, String>> {
    if values.len() > MAX_RUNTIME_KEYS {
        return Err(Error::InvalidRuntimeUpdate(
            "runtime update contains too many values",
        ));
    }
    let mut next = BTreeMap::new();
    for entry in values {
        validate_runtime_key(&entry.key)?;
        if !allowed_keys.contains(&entry.key) {
            return Err(Error::RuntimeKeyNotAllowed(entry.key));
        }
        if entry.value.len() > MAX_RUNTIME_VALUE_BYTES {
            return Err(Error::InvalidRuntimeUpdate("runtime value exceeds 64 KiB"));
        }
        if next.insert(entry.key.clone(), entry.value).is_some() {
            return Err(Error::DuplicateRuntimeValue(entry.key));
        }
    }
    Ok(next)
}

fn validated_runtime_keys(
    allowed_keys: &BTreeSet<String>,
    keys: &[String],
) -> Result<Vec<String>> {
    if keys.len() > MAX_RUNTIME_KEYS {
        return Err(Error::InvalidRuntimeEvent(
            "runtime delete contains too many keys",
        ));
    }
    let mut unique = BTreeSet::new();
    for key in keys {
        validate_runtime_key(key)?;
        if !allowed_keys.contains(key) {
            return Err(Error::RuntimeKeyNotAllowed(key.clone()));
        }
        if !unique.insert(key.clone()) {
            return Err(Error::DuplicateRuntimeValue(key.clone()));
        }
    }
    Ok(keys.to_vec())
}

fn validate_event_shape(update: &RuntimeEventUpdate) -> Result<()> {
    let valid = match update.operation {
        RuntimeEventOperation::Upsert => !update.values.is_empty() && update.keys.is_empty(),
        RuntimeEventOperation::Delete => update.values.is_empty() && !update.keys.is_empty(),
        RuntimeEventOperation::Replace => update.keys.is_empty(),
        RuntimeEventOperation::Invalidate | RuntimeEventOperation::Resync => {
            update.values.is_empty() && update.keys.is_empty()
        }
    };
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidRuntimeEvent(
            "runtime event operation payload is invalid",
        ))
    }
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

fn parse_positive_revision(value: &str) -> Result<u64> {
    let revision = parse_revision(value)?;
    if revision == 0 {
        return Err(Error::InvalidRuntimeEvent(
            "event revision must be greater than zero",
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
    #[error("runtime event is invalid: {0}")]
    InvalidRuntimeEvent(&'static str),
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

    fn event_plan() -> RuntimeUpdatePlan {
        RuntimeUpdatePlan {
            sidecar: "chat".to_owned(),
            runtime_namespace: "ores-chat".to_owned(),
            provider: RuntimeUpdateProvider::OresRedisLruCache,
            lru_config_path: PathBuf::from(".ores-lru.toml"),
            role: RuntimeUpdateRole::Server,
            cache: RUNTIME_CACHE_NAME.to_owned(),
            allowed_keys: BTreeSet::from([
                "PROVIDER_TIMEOUT_MS".to_owned(),
                "WORKER_BATCH_SIZE".to_owned(),
            ]),
        }
    }

    fn event(
        revision: u64,
        operation: RuntimeEventOperation,
        values: Vec<RuntimeValue>,
        keys: Vec<String>,
    ) -> RuntimeEventUpdate {
        RuntimeEventUpdate {
            protocol: RUNTIME_EVENT_PROTOCOL.to_owned(),
            sidecar: "chat".to_owned(),
            revision: revision.to_string(),
            operation,
            values,
            keys,
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
        let parsed = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "chat"
runtimeKeys = ["PROVIDER_TIMEOUT_MS"]"#,
        ))
        .unwrap();
        let sidecar = parsed.resolve_one(None).unwrap();
        let plan = parsed.runtime_plan_for(&sidecar).unwrap();
        let mut state = RuntimeState::new(&plan);

        let applied = state
            .apply(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "7".to_owned(),
                values: vec![RuntimeValue {
                    key: "PROVIDER_TIMEOUT_MS".to_owned(),
                    value: "2500".to_owned(),
                }],
            })
            .unwrap();
        assert_eq!(
            applied,
            RuntimeApplyOutcome::Applied {
                previous: 0,
                current: 7
            }
        );

        let stale = state
            .apply(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "6".to_owned(),
                values: Vec::new(),
            })
            .unwrap();
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
    fn undeclared_runtime_values_are_rejected_without_partial_application() {
        let parsed = SidecarConfig::parse(&config(
            r#"[[sidecars]]
name = "chat"
enabled = true
bindIp = "127.0.0.1"
bindPort = 7410
loopbackOnly = true
runtimeNamespace = "chat"
runtimeKeys = ["PROVIDER_TIMEOUT_MS"]"#,
        ))
        .unwrap();
        let sidecar = parsed.resolve_one(None).unwrap();
        let plan = parsed.runtime_plan_for(&sidecar).unwrap();
        let mut state = RuntimeState::new(&plan);
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
    fn ordered_events_apply_and_revision_gap_is_sticky_until_snapshot_repair() {
        let mut state = RuntimeEventState::new(&event_plan());
        state
            .apply_snapshot(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "1".to_owned(),
                values: Vec::new(),
            })
            .unwrap();

        let applied = state
            .apply_event(event(
                2,
                RuntimeEventOperation::Upsert,
                vec![RuntimeValue {
                    key: "PROVIDER_TIMEOUT_MS".to_owned(),
                    value: "2500".to_owned(),
                }],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            applied,
            RuntimeEventOutcome::Applied {
                previous: 1,
                current: 2
            }
        );

        let gap = state
            .apply_event(event(
                4,
                RuntimeEventOperation::Upsert,
                vec![RuntimeValue {
                    key: "WORKER_BATCH_SIZE".to_owned(),
                    value: "20".to_owned(),
                }],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            gap,
            RuntimeEventOutcome::ReconcileRequired {
                current: 2,
                incoming: 4
            }
        );
        assert!(state.is_stale());
        assert!(!state.snapshot().values.contains_key("WORKER_BATCH_SIZE"));

        let would_fill_gap = state
            .apply_event(event(
                3,
                RuntimeEventOperation::Upsert,
                vec![RuntimeValue {
                    key: "WORKER_BATCH_SIZE".to_owned(),
                    value: "15".to_owned(),
                }],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            would_fill_gap,
            RuntimeEventOutcome::ReconcileRequired {
                current: 2,
                incoming: 3
            }
        );
        assert!(!state.snapshot().values.contains_key("WORKER_BATCH_SIZE"));

        let repaired = state
            .apply_snapshot(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "4".to_owned(),
                values: vec![RuntimeValue {
                    key: "WORKER_BATCH_SIZE".to_owned(),
                    value: "20".to_owned(),
                }],
            })
            .unwrap();
        assert_eq!(
            repaired,
            RuntimeApplyOutcome::Applied {
                previous: 2,
                current: 4
            }
        );
        assert!(!state.is_stale());
        assert_eq!(
            state
                .snapshot()
                .values
                .get("WORKER_BATCH_SIZE")
                .map(String::as_str),
            Some("20")
        );
    }

    #[test]
    fn event_operations_are_atomic_and_fail_closed() {
        let mut state = RuntimeEventState::new(&event_plan());
        state
            .apply_snapshot(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "1".to_owned(),
                values: vec![RuntimeValue {
                    key: "PROVIDER_TIMEOUT_MS".to_owned(),
                    value: "2500".to_owned(),
                }],
            })
            .unwrap();

        let invalid = state.apply_event(event(
            2,
            RuntimeEventOperation::Upsert,
            vec![
                RuntimeValue {
                    key: "WORKER_BATCH_SIZE".to_owned(),
                    value: "10".to_owned(),
                },
                RuntimeValue {
                    key: "UNDECLARED_FLAG".to_owned(),
                    value: "true".to_owned(),
                },
            ],
            Vec::new(),
        ));
        assert!(matches!(
            invalid,
            Err(Error::RuntimeKeyNotAllowed(key)) if key == "UNDECLARED_FLAG"
        ));
        assert_eq!(state.snapshot().revision, 1);
        assert!(!state.snapshot().values.contains_key("WORKER_BATCH_SIZE"));
        assert!(!state.is_stale());

        let malformed = state.apply_event(event(
            2,
            RuntimeEventOperation::Delete,
            vec![RuntimeValue {
                key: "PROVIDER_TIMEOUT_MS".to_owned(),
                value: "ignored".to_owned(),
            }],
            vec!["PROVIDER_TIMEOUT_MS".to_owned()],
        ));
        assert!(matches!(malformed, Err(Error::InvalidRuntimeEvent(_))));
        assert_eq!(state.snapshot().revision, 1);
        assert!(!state.is_stale());
    }

    #[test]
    fn delete_replace_invalidate_and_resync_have_distinct_semantics() {
        let mut state = RuntimeEventState::new(&event_plan());
        state
            .apply_snapshot(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "1".to_owned(),
                values: vec![
                    RuntimeValue {
                        key: "PROVIDER_TIMEOUT_MS".to_owned(),
                        value: "2500".to_owned(),
                    },
                    RuntimeValue {
                        key: "WORKER_BATCH_SIZE".to_owned(),
                        value: "10".to_owned(),
                    },
                ],
            })
            .unwrap();

        state
            .apply_event(event(
                2,
                RuntimeEventOperation::Delete,
                Vec::new(),
                vec!["PROVIDER_TIMEOUT_MS".to_owned()],
            ))
            .unwrap();
        assert!(!state.snapshot().values.contains_key("PROVIDER_TIMEOUT_MS"));

        state
            .apply_event(event(
                3,
                RuntimeEventOperation::Replace,
                vec![RuntimeValue {
                    key: "PROVIDER_TIMEOUT_MS".to_owned(),
                    value: "3000".to_owned(),
                }],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(state.snapshot().values.len(), 1);
        assert!(!state.snapshot().values.contains_key("WORKER_BATCH_SIZE"));

        state
            .apply_event(event(
                4,
                RuntimeEventOperation::Invalidate,
                Vec::new(),
                Vec::new(),
            ))
            .unwrap();
        assert!(state.snapshot().values.is_empty());

        let resync = state
            .apply_event(event(
                5,
                RuntimeEventOperation::Resync,
                Vec::new(),
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            resync,
            RuntimeEventOutcome::ReconcileRequired {
                current: 4,
                incoming: 5
            }
        );
        assert!(state.is_stale());
        assert_eq!(state.snapshot().revision, 4);
    }

    #[test]
    fn duplicate_events_are_idempotent_and_do_not_reopen_stale_state() {
        let mut state = RuntimeEventState::new(&event_plan());
        state
            .apply_snapshot(RuntimeSnapshotUpdate {
                protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
                sidecar: "chat".to_owned(),
                revision: "2".to_owned(),
                values: Vec::new(),
            })
            .unwrap();
        let duplicate = state
            .apply_event(event(
                2,
                RuntimeEventOperation::Invalidate,
                Vec::new(),
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            duplicate,
            RuntimeEventOutcome::Duplicate {
                current: 2,
                incoming: 2
            }
        );
        assert!(!state.is_stale());
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

        let event = RuntimeEventUpdate {
            protocol: RUNTIME_EVENT_PROTOCOL.to_owned(),
            sidecar: "chat".to_owned(),
            revision: MAX_SAFE_REVISION.to_string(),
            operation: RuntimeEventOperation::Invalidate,
            values: Vec::new(),
            keys: Vec::new(),
        };
        let event_json = serde_json::to_value(event).unwrap();
        assert_eq!(event_json["revision"], MAX_SAFE_REVISION.to_string());
    }
}
