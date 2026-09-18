use std::{collections::BTreeSet, path::PathBuf};

use ores_sidecar::{
    is_runtime_key_allowed, Error, RuntimeApplyOutcome, RuntimeSnapshotUpdate, RuntimeState,
    RuntimeUpdatePlan, RuntimeUpdateProvider, RuntimeUpdateRole, RuntimeValue, MAX_SAFE_REVISION,
    RUNTIME_CACHE_NAME, RUNTIME_UPDATE_PROTOCOL,
};

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

fn update(revision: &str, values: &[(&str, &str)]) -> RuntimeSnapshotUpdate {
    RuntimeSnapshotUpdate {
        protocol: RUNTIME_UPDATE_PROTOCOL.to_owned(),
        sidecar: "chat".to_owned(),
        revision: revision.to_owned(),
        values: values
            .iter()
            .map(|(key, value)| RuntimeValue {
                key: (*key).to_owned(),
                value: (*value).to_owned(),
            })
            .collect(),
    }
}

#[test]
fn failed_multi_value_update_is_atomic_for_the_imperative_shell() {
    let mut state = RuntimeState::new(&plan());
    state
        .apply(update("1", &[("PROVIDER_TIMEOUT_MS", "2500")]))
        .unwrap();

    let result = state.apply(update(
        "2",
        &[
            ("PROVIDER_TIMEOUT_MS", "3000"),
            ("UNDECLARED_FLAG", "true"),
        ],
    ));

    assert!(matches!(
        result,
        Err(Error::RuntimeKeyNotAllowed(key)) if key == "UNDECLARED_FLAG"
    ));
    assert_eq!(state.snapshot().revision, 1);
    assert_eq!(
        state
            .snapshot()
            .values
            .get("PROVIDER_TIMEOUT_MS")
            .map(String::as_str),
        Some("2500")
    );
    assert!(!state.snapshot().values.contains_key("UNDECLARED_FLAG"));
}

#[test]
fn pure_transition_does_not_alias_or_mutate_the_source_state() {
    let mut source = RuntimeState::new(&plan());
    source
        .apply(update("1", &[("PROVIDER_TIMEOUT_MS", "2500")]))
        .unwrap();

    let transition = source
        .transition(update("2", &[("WORKER_BATCH_SIZE", "10")]))
        .unwrap();

    assert_eq!(
        transition.outcome,
        RuntimeApplyOutcome::Applied {
            previous: 1,
            current: 2,
        }
    );
    assert_eq!(source.snapshot().revision, 1);
    assert_eq!(transition.next.snapshot().revision, 2);
    assert!(!source.snapshot().values.contains_key("WORKER_BATCH_SIZE"));
    assert_eq!(
        transition
            .next
            .snapshot()
            .values
            .get("WORKER_BATCH_SIZE")
            .map(String::as_str),
        Some("10")
    );

    source
        .apply(update("3", &[("PROVIDER_TIMEOUT_MS", "4000")]))
        .unwrap();
    assert_eq!(source.snapshot().revision, 3);
    assert_eq!(transition.next.snapshot().revision, 2);
    assert_eq!(
        transition
            .next
            .snapshot()
            .values
            .get("WORKER_BATCH_SIZE")
            .map(String::as_str),
        Some("10")
    );
}

#[test]
fn revision_lexical_and_cross_runtime_bounds_fail_closed() {
    for revision in [
        "01",
        "+1",
        " 1",
        "1 ",
        "1_000",
        "9007199254740992",
        "18446744073709551615",
    ] {
        let mut state = RuntimeState::new(&plan());
        let result = state.apply(update(revision, &[]));
        assert!(
            matches!(result, Err(Error::InvalidRuntimeUpdate(_))),
            "revision {revision:?} unexpectedly passed"
        );
        assert_eq!(state.snapshot().revision, 0);
    }

    let mut state = RuntimeState::new(&plan());
    let accepted = state.apply(update(&MAX_SAFE_REVISION.to_string(), &[]));
    assert!(accepted.is_ok());
    assert_eq!(state.snapshot().revision, MAX_SAFE_REVISION);
}

#[test]
fn public_runtime_key_classifier_rejects_embedded_secret_markers() {
    for key in [
        "API_TOKEN_ROTATION_MS",
        "DATABASE_URL_TIMEOUT_MS",
        "PRIVATE_KEY_CACHE_TTL",
        "PASSWORD_POLICY_VERSION",
        "CREDENTIAL_REFRESH_SECONDS",
    ] {
        assert!(!is_runtime_key_allowed(key), "{key} must stay forbidden");
    }

    for key in [
        "PROVIDER_TIMEOUT_MS",
        "WORKER_BATCH_SIZE",
        "_INTERNAL_RETRY_LIMIT",
    ] {
        assert!(is_runtime_key_allowed(key), "{key} should be admitted");
    }

    assert!(!is_runtime_key_allowed("lowercase_key"));
    assert!(!is_runtime_key_allowed("9STARTS_WITH_DIGIT"));
}
