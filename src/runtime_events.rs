//! Ordered provider-event reconciliation layered on top of the canonical sidecar runtime plan.
//!
//! Incremental events are accepted only when they are exactly sequential. A valid revision gap
//! or explicit `resync` makes the state sticky-stale; only an authoritative full snapshot can
//! clear that fence. The preferred APIs are pure transitions that return an independent next
//! state. The imperative `apply_*` methods are compatibility shells around those transitions.

use crate::{
    is_runtime_key_allowed, RuntimeApplyOutcome, RuntimeSnapshot, RuntimeSnapshotUpdate,
    RuntimeUpdatePlan, RuntimeValue, MAX_RUNTIME_KEYS, MAX_RUNTIME_VALUE_BYTES, MAX_SAFE_REVISION,
    RUNTIME_UPDATE_PROTOCOL,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

pub const RUNTIME_EVENT_PROTOCOL: &str = "ores.sidecar-runtime-event.v1";

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
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RuntimeEventUpdate {
    pub protocol: String,
    pub sidecar: String,
    /// Canonical positive decimal. Strings preserve the cross-runtime safe-integer boundary.
    pub revision: String,
    pub operation: RuntimeEventOperation,
    #[serde(default)]
    pub values: Vec<RuntimeValue>,
    #[serde(default)]
    pub keys: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEventOutcome {
    Applied { previous: u64, current: u64 },
    Duplicate { current: u64, incoming: u64 },
    ReconcileRequired { current: u64, incoming: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventTransition {
    pub next: RuntimeEventState,
    pub outcome: RuntimeEventOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventSnapshotTransition {
    pub next: RuntimeEventState,
    pub outcome: RuntimeApplyOutcome,
}

/// Event-aware runtime overlay state.
///
/// A revision gap or explicit `resync` event makes this state sticky-stale. Incremental events
/// can never clear that state, even if the missing revision subsequently arrives. A full
/// snapshot at the current or a newer revision is required to repair it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventState {
    sidecar: String,
    allowed_keys: BTreeSet<String>,
    snapshot: RuntimeSnapshot,
    stale: bool,
}

impl RuntimeEventState {
    pub fn new(plan: &RuntimeUpdatePlan) -> Self {
        Self {
            sidecar: plan.sidecar.clone(),
            allowed_keys: plan.allowed_keys.iter().cloned().collect(),
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
            stale: self.stale,
        }
    }

    /// Compute an authoritative snapshot repair without mutating `self`.
    ///
    /// Equal revisions are accepted deliberately: reconciliation may need to repair divergent
    /// local values without inventing a provider revision. Older snapshots are ignored and do
    /// not clear a stale fence.
    pub fn transition_snapshot(
        &self,
        update: RuntimeSnapshotUpdate,
    ) -> RuntimeEventResult<RuntimeEventSnapshotTransition> {
        if update.protocol != RUNTIME_UPDATE_PROTOCOL {
            return Err(RuntimeEventError::InvalidSnapshot(
                "unsupported runtime protocol",
            ));
        }
        if update.sidecar != self.sidecar {
            return Err(RuntimeEventError::TargetMismatch {
                expected: self.sidecar.clone(),
                actual: update.sidecar,
            });
        }
        let revision = parse_revision(&update.revision, false)?;
        if revision < self.snapshot.revision {
            return Ok(RuntimeEventSnapshotTransition {
                next: self.independent(),
                outcome: RuntimeApplyOutcome::StaleIgnored {
                    current: self.snapshot.revision,
                    incoming: revision,
                },
            });
        }

        let values = validated_runtime_values(&self.allowed_keys, update.values)?;
        Ok(RuntimeEventSnapshotTransition {
            next: Self {
                sidecar: self.sidecar.clone(),
                allowed_keys: self.allowed_keys.iter().cloned().collect(),
                snapshot: RuntimeSnapshot { revision, values },
                stale: false,
            },
            outcome: RuntimeApplyOutcome::Applied {
                previous: self.snapshot.revision,
                current: revision,
            },
        })
    }

    /// Imperative compatibility shell around [`Self::transition_snapshot`].
    pub fn apply_snapshot(
        &mut self,
        update: RuntimeSnapshotUpdate,
    ) -> RuntimeEventResult<RuntimeApplyOutcome> {
        let transition = self.transition_snapshot(update)?;
        let outcome = transition.outcome;
        *self = transition.next;
        Ok(outcome)
    }

    /// Compute the next state for one ordered provider event without mutating `self`.
    ///
    /// Validation is deliberately performed before duplicate detection so a malformed replay
    /// never becomes accepted evidence merely because its revision is old.
    pub fn transition_event(
        &self,
        update: RuntimeEventUpdate,
    ) -> RuntimeEventResult<RuntimeEventTransition> {
        if update.protocol != RUNTIME_EVENT_PROTOCOL {
            return Err(RuntimeEventError::InvalidEvent(
                "unsupported runtime event protocol",
            ));
        }
        if update.sidecar != self.sidecar {
            return Err(RuntimeEventError::TargetMismatch {
                expected: self.sidecar.clone(),
                actual: update.sidecar,
            });
        }

        let revision = parse_revision(&update.revision, true)?;
        validate_event_shape(&update)?;

        let validated_values = match update.operation {
            RuntimeEventOperation::Upsert | RuntimeEventOperation::Replace => Some(
                validated_runtime_values(&self.allowed_keys, update.values.clone())?,
            ),
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
            return Ok(RuntimeEventTransition {
                next: self.independent(),
                outcome: RuntimeEventOutcome::Duplicate {
                    current: self.snapshot.revision,
                    incoming: revision,
                },
            });
        }
        if self.stale {
            return Ok(RuntimeEventTransition {
                next: self.independent(),
                outcome: RuntimeEventOutcome::ReconcileRequired {
                    current: self.snapshot.revision,
                    incoming: revision,
                },
            });
        }
        if revision != self.snapshot.revision.saturating_add(1) {
            let mut next = self.independent();
            next.stale = true;
            return Ok(RuntimeEventTransition {
                next,
                outcome: RuntimeEventOutcome::ReconcileRequired {
                    current: self.snapshot.revision,
                    incoming: revision,
                },
            });
        }
        if update.operation == RuntimeEventOperation::Resync {
            let mut next = self.independent();
            next.stale = true;
            return Ok(RuntimeEventTransition {
                next,
                outcome: RuntimeEventOutcome::ReconcileRequired {
                    current: self.snapshot.revision,
                    incoming: revision,
                },
            });
        }

        let mut values = self
            .snapshot
            .values
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<BTreeMap<_, _>>();
        match update.operation {
            RuntimeEventOperation::Upsert => {
                for (key, value) in validated_values.expect("upsert values were validated") {
                    values.insert(key, value);
                }
            }
            RuntimeEventOperation::Delete => {
                for key in validated_keys.expect("delete keys were validated") {
                    values.remove(&key);
                }
            }
            RuntimeEventOperation::Replace => {
                values = validated_values.expect("replacement values were validated");
            }
            RuntimeEventOperation::Invalidate => values.clear(),
            RuntimeEventOperation::Resync => unreachable!("resync returns before mutation"),
        }

        Ok(RuntimeEventTransition {
            next: Self {
                sidecar: self.sidecar.clone(),
                allowed_keys: self.allowed_keys.iter().cloned().collect(),
                snapshot: RuntimeSnapshot { revision, values },
                stale: false,
            },
            outcome: RuntimeEventOutcome::Applied {
                previous: self.snapshot.revision,
                current: revision,
            },
        })
    }

    /// Imperative compatibility shell around [`Self::transition_event`].
    pub fn apply_event(
        &mut self,
        update: RuntimeEventUpdate,
    ) -> RuntimeEventResult<RuntimeEventOutcome> {
        let transition = self.transition_event(update)?;
        let outcome = transition.outcome;
        *self = transition.next;
        Ok(outcome)
    }
}

fn validate_event_shape(update: &RuntimeEventUpdate) -> RuntimeEventResult<()> {
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
        Err(RuntimeEventError::InvalidEvent(
            "runtime event operation payload is invalid",
        ))
    }
}

fn validated_runtime_values(
    allowed_keys: &BTreeSet<String>,
    values: Vec<RuntimeValue>,
) -> RuntimeEventResult<BTreeMap<String, String>> {
    if values.len() > MAX_RUNTIME_KEYS {
        return Err(RuntimeEventError::InvalidEvent(
            "runtime update contains too many values",
        ));
    }
    values.into_iter().try_fold(
        BTreeMap::new(),
        |mut next, entry| -> RuntimeEventResult<BTreeMap<String, String>> {
            validate_runtime_key(&entry.key)?;
            if !allowed_keys.contains(&entry.key) {
                return Err(RuntimeEventError::RuntimeKeyNotAllowed(entry.key));
            }
            if entry.value.len() > MAX_RUNTIME_VALUE_BYTES {
                return Err(RuntimeEventError::InvalidEvent(
                    "runtime value exceeds 64 KiB",
                ));
            }
            if next.insert(entry.key.clone(), entry.value).is_some() {
                return Err(RuntimeEventError::DuplicateRuntimeValue(entry.key));
            }
            Ok(next)
        },
    )
}

fn validated_runtime_keys(
    allowed_keys: &BTreeSet<String>,
    keys: &[String],
) -> RuntimeEventResult<Vec<String>> {
    if keys.len() > MAX_RUNTIME_KEYS {
        return Err(RuntimeEventError::InvalidEvent(
            "runtime delete contains too many keys",
        ));
    }
    let mut unique = BTreeSet::new();
    for key in keys {
        validate_runtime_key(key)?;
        if !allowed_keys.contains(key) {
            return Err(RuntimeEventError::RuntimeKeyNotAllowed(key.clone()));
        }
        if !unique.insert(key.clone()) {
            return Err(RuntimeEventError::DuplicateRuntimeValue(key.clone()));
        }
    }
    Ok(keys.to_vec())
}

fn validate_runtime_key(key: &str) -> RuntimeEventResult<()> {
    if is_runtime_key_allowed(key) {
        Ok(())
    } else {
        Err(RuntimeEventError::InvalidRuntimeKey(key.to_owned()))
    }
}

fn parse_revision(value: &str, positive: bool) -> RuntimeEventResult<u64> {
    if value.is_empty()
        || value.len() > 16
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || (value.len() > 1 && value.starts_with('0'))
    {
        return Err(RuntimeEventError::InvalidRevision);
    }
    let revision = value
        .parse::<u64>()
        .map_err(|_| RuntimeEventError::InvalidRevision)?;
    if revision > MAX_SAFE_REVISION || (positive && revision == 0) {
        return Err(RuntimeEventError::InvalidRevision);
    }
    Ok(revision)
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum RuntimeEventError {
    #[error("runtime event is invalid: {0}")]
    InvalidEvent(&'static str),
    #[error("runtime snapshot is invalid: {0}")]
    InvalidSnapshot(&'static str),
    #[error("runtime revision must be canonical decimal within the cross-runtime safe integer bound")]
    InvalidRevision,
    #[error("runtime update targeted {actual}, expected {expected}")]
    TargetMismatch { expected: String, actual: String },
    #[error("invalid or secret-bearing runtime key: {0}")]
    InvalidRuntimeKey(String),
    #[error("runtime key is not allowlisted by .ores-sidecar.toml: {0}")]
    RuntimeKeyNotAllowed(String),
    #[error("duplicate runtime value or key: {0}")]
    DuplicateRuntimeValue(String),
}

pub type RuntimeEventResult<T> = std::result::Result<T, RuntimeEventError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        RuntimeUpdateProvider, RuntimeUpdateRole, RUNTIME_CACHE_NAME,
    };
    use std::path::PathBuf;

    fn plan() -> RuntimeUpdatePlan {
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

    fn snapshot(revision: u64, values: Vec<RuntimeValue>) -> RuntimeSnapshotUpdate {
        RuntimeSnapshotUpdate {
            protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
            sidecar: "chat".to_owned(),
            revision: revision.to_string(),
            values,
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

    fn value(key: &str, value: &str) -> RuntimeValue {
        RuntimeValue {
            key: key.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn pure_event_transition_preserves_source_and_matches_imperative_shell() {
        let source = RuntimeEventState::new(&plan())
            .transition_snapshot(snapshot(1, Vec::new()))
            .unwrap()
            .next;
        let update = event(
            2,
            RuntimeEventOperation::Upsert,
            vec![value("PROVIDER_TIMEOUT_MS", "2500")],
            Vec::new(),
        );
        let transition = source.transition_event(update.clone()).unwrap();

        assert_eq!(source.snapshot().revision, 1);
        assert!(source.snapshot().values.is_empty());
        assert_eq!(transition.next.snapshot().revision, 2);
        assert_eq!(
            transition.next.snapshot().values.get("PROVIDER_TIMEOUT_MS").map(String::as_str),
            Some("2500")
        );

        let mut applied = source.clone();
        let outcome = applied.apply_event(update).unwrap();
        assert_eq!(outcome, transition.outcome);
        assert_eq!(applied, transition.next);
    }

    #[test]
    fn revision_gap_is_sticky_until_authoritative_snapshot_repair() {
        let seeded = RuntimeEventState::new(&plan())
            .transition_snapshot(snapshot(1, Vec::new()))
            .unwrap()
            .next;
        let gap = seeded
            .transition_event(event(
                3,
                RuntimeEventOperation::Upsert,
                vec![value("WORKER_BATCH_SIZE", "20")],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(
            gap.outcome,
            RuntimeEventOutcome::ReconcileRequired {
                current: 1,
                incoming: 3,
            }
        );
        assert!(gap.next.is_stale());
        assert_eq!(gap.next.snapshot().revision, 1);
        assert!(!seeded.is_stale());

        let fill = gap
            .next
            .transition_event(event(
                2,
                RuntimeEventOperation::Upsert,
                vec![value("WORKER_BATCH_SIZE", "15")],
                Vec::new(),
            ))
            .unwrap();
        assert!(fill.next.is_stale());
        assert_eq!(fill.next.snapshot().revision, 1);

        let repaired = fill
            .next
            .transition_snapshot(snapshot(3, vec![value("WORKER_BATCH_SIZE", "20")]))
            .unwrap();
        assert!(!repaired.next.is_stale());
        assert_eq!(repaired.next.snapshot().revision, 3);
        assert_eq!(
            repaired.next.snapshot().values.get("WORKER_BATCH_SIZE").map(String::as_str),
            Some("20")
        );
    }

    #[test]
    fn equal_revision_snapshot_can_repair_stale_local_values() {
        let seeded = RuntimeEventState::new(&plan())
            .transition_snapshot(snapshot(2, vec![value("PROVIDER_TIMEOUT_MS", "old")]))
            .unwrap()
            .next;
        let stale = seeded
            .transition_event(event(4, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new()))
            .unwrap()
            .next;
        assert!(stale.is_stale());

        let repaired = stale
            .transition_snapshot(snapshot(2, vec![value("PROVIDER_TIMEOUT_MS", "repaired")]))
            .unwrap();
        assert_eq!(
            repaired.outcome,
            RuntimeApplyOutcome::Applied {
                previous: 2,
                current: 2,
            }
        );
        assert!(!repaired.next.is_stale());
        assert_eq!(
            repaired.next.snapshot().values.get("PROVIDER_TIMEOUT_MS").map(String::as_str),
            Some("repaired")
        );
    }

    #[test]
    fn delete_replace_invalidate_and_resync_remain_distinct() {
        let mut state = RuntimeEventState::new(&plan());
        state
            .apply_snapshot(snapshot(
                1,
                vec![
                    value("PROVIDER_TIMEOUT_MS", "2500"),
                    value("WORKER_BATCH_SIZE", "10"),
                ],
            ))
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
                vec![value("PROVIDER_TIMEOUT_MS", "3000")],
                Vec::new(),
            ))
            .unwrap();
        assert_eq!(state.snapshot().values.len(), 1);
        assert!(!state.snapshot().values.contains_key("WORKER_BATCH_SIZE"));

        state
            .apply_event(event(4, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new()))
            .unwrap();
        assert!(state.snapshot().values.is_empty());

        let resync = state
            .apply_event(event(5, RuntimeEventOperation::Resync, Vec::new(), Vec::new()))
            .unwrap();
        assert_eq!(
            resync,
            RuntimeEventOutcome::ReconcileRequired {
                current: 4,
                incoming: 5,
            }
        );
        assert!(state.is_stale());
        assert_eq!(state.snapshot().revision, 4);
    }

    #[test]
    fn malformed_events_fail_before_duplicate_detection_and_never_mutate() {
        let state = RuntimeEventState::new(&plan())
            .transition_snapshot(snapshot(2, Vec::new()))
            .unwrap()
            .next;
        let malformed_duplicate = event(
            2,
            RuntimeEventOperation::Delete,
            vec![value("PROVIDER_TIMEOUT_MS", "ignored")],
            vec!["PROVIDER_TIMEOUT_MS".to_owned()],
        );
        assert!(matches!(
            state.transition_event(malformed_duplicate),
            Err(RuntimeEventError::InvalidEvent(_))
        ));
        assert_eq!(state.snapshot().revision, 2);
        assert!(!state.is_stale());
    }

    #[test]
    fn invalid_allowlist_data_is_atomic_and_does_not_trip_the_stale_fence() {
        let state = RuntimeEventState::new(&plan())
            .transition_snapshot(snapshot(1, Vec::new()))
            .unwrap()
            .next;
        let invalid = state.transition_event(event(
            2,
            RuntimeEventOperation::Upsert,
            vec![
                value("WORKER_BATCH_SIZE", "10"),
                value("UNDECLARED_FLAG", "true"),
            ],
            Vec::new(),
        ));
        assert!(matches!(
            invalid,
            Err(RuntimeEventError::RuntimeKeyNotAllowed(key)) if key == "UNDECLARED_FLAG"
        ));
        assert_eq!(state.snapshot().revision, 1);
        assert!(state.snapshot().values.is_empty());
        assert!(!state.is_stale());
    }

    #[test]
    fn duplicate_events_are_idempotent_and_preserve_stale_state() {
        let seeded = RuntimeEventState::new(&plan())
            .transition_snapshot(snapshot(2, Vec::new()))
            .unwrap()
            .next;
        let duplicate = seeded
            .transition_event(event(2, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new()))
            .unwrap();
        assert_eq!(
            duplicate.outcome,
            RuntimeEventOutcome::Duplicate {
                current: 2,
                incoming: 2,
            }
        );
        assert_eq!(duplicate.next, seeded);

        let stale = seeded
            .transition_event(event(4, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new()))
            .unwrap()
            .next;
        assert!(stale.is_stale());
        let old = stale
            .transition_event(event(2, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new()))
            .unwrap();
        assert!(old.next.is_stale());
        assert_eq!(old.next.snapshot().revision, 2);
    }

    #[test]
    fn event_revision_zero_and_unsafe_integer_bounds_fail_closed() {
        let state = RuntimeEventState::new(&plan());
        let mut zero = event(1, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new());
        zero.revision = "0".to_owned();
        assert_eq!(
            state.transition_event(zero).unwrap_err(),
            RuntimeEventError::InvalidRevision
        );

        let mut oversized = event(1, RuntimeEventOperation::Invalidate, Vec::new(), Vec::new());
        oversized.revision = (MAX_SAFE_REVISION + 1).to_string();
        assert_eq!(
            state.transition_event(oversized).unwrap_err(),
            RuntimeEventError::InvalidRevision
        );
    }
}
