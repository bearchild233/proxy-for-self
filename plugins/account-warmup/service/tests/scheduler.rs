use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use account_warmup::{
    Account, AccountOverride, Outcome, Settings, ShortTerm, Store, daily_points, jitter, validate,
};
use chrono::{DateTime, Utc};
use serde_json::Value;

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "warmup-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn open(&self) -> Store {
        Store::open(&self.0).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .unwrap()
        .with_timezone(&Utc)
}
fn account(id: &str) -> Account {
    Account {
        id: id.into(),
        provider: "openai".into(),
        authentication_kind: "oauth".into(),
        enabled: true,
        status: "active".into(),
        supported: true,
        reason_code: None,
        model_allowed: None,
        models: None,
        short_term: None,
    }
}
fn settings() -> Settings {
    Settings {
        jitter_min_seconds: 0,
        jitter_max_seconds: 0,
        ..Settings::default()
    }
}
fn accepted() -> Outcome {
    Outcome {
        outcome: "accepted".into(),
        reason_code: None,
        status: Some(200),
        duration_ms: 4,
        send_state: "sent".into(),
        short_term: None,
    }
}
fn items(store: &Store, run: &str) -> Vec<Value> {
    store.history(20, Some(run)).unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

#[test]
fn defaults_and_compare_exchange_are_persistent_and_strict() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let (revision, default) = store.settings().unwrap();
    assert_eq!(revision, 0);
    assert!(!default.enabled);
    assert!(!default.daily_enabled);
    assert!(default.reset_enabled);
    assert_eq!(
        (default.jitter_min_seconds, default.jitter_max_seconds),
        (5, 15)
    );
    let mut changed = default.clone();
    changed.reset_delay_seconds = 86400;
    store.configure(0, &changed).unwrap();
    assert_eq!(store.configure(0, &default).unwrap_err().code, "conflict");
    assert_eq!(fixture.open().settings().unwrap(), (1, changed.clone()));
    changed.reset_delay_seconds = 86401;
    assert!(validate(&changed).is_err());
    changed = default;
    changed.enabled = true;
    changed.reset_enabled = false;
    assert!(validate(&changed).is_err());
    changed.target_mode = "selected".into();
    assert!(validate(&changed).is_err());
}

#[test]
fn daily_dst_gap_and_overlap_have_one_occurrence() {
    let (last, next) =
        daily_points(at("2026-03-08T12:00:00Z"), "America/New_York", "02:30").unwrap();
    assert_eq!(last, at("2026-03-08T07:00:00Z"));
    assert_eq!(next, at("2026-03-09T06:30:00Z"));
    let (last, next) =
        daily_points(at("2026-11-01T06:00:00Z"), "America/New_York", "01:30").unwrap();
    assert_eq!(last, at("2026-11-01T05:30:00Z"));
    assert_eq!(next, at("2026-11-02T06:30:00Z"));
}

#[test]
fn shared_sqlite_claim_batch_idempotency_and_expiry_never_resend() {
    let fixture = Fixture::new();
    let old = fixture.open();
    let new = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let first = old
        .enqueue_manual(
            "submission-one",
            &[account("a"), account("b")],
            &settings(),
            now,
        )
        .unwrap();
    assert_eq!(
        new.enqueue_manual("submission-one", &[account("c")], &settings(), now)
            .unwrap()
            .id,
        first.id
    );
    let joined = new
        .enqueue_manual("submission-two", &[account("a")], &settings(), now)
        .unwrap();
    assert_eq!(
        items(&old, &first.id)[0]["id"],
        items(&new, &joined.id)[0]["id"]
    );
    let claim = old.claim("old", now).unwrap().unwrap();
    assert!(
        new.claim("new", now + chrono::Duration::seconds(20))
            .unwrap()
            .is_none()
    );
    let second = new
        .claim("new", now + chrono::Duration::seconds(61))
        .unwrap()
        .unwrap();
    assert_ne!(claim.id, second.id);
    old.finish(
        &claim.id,
        "old",
        &accepted(),
        now + chrono::Duration::seconds(62),
    )
    .unwrap();
    let history = items(&new, &first.id);
    assert_eq!(
        history.iter().find(|item| item["id"] == claim.id).unwrap()["status"],
        "unknown"
    );
    new.finish(
        &second.id,
        "new",
        &accepted(),
        now + chrono::Duration::seconds(63),
    )
    .unwrap();
    assert!(
        new.claim("new", now + chrono::Duration::minutes(10))
            .unwrap()
            .is_none()
    );
}

#[test]
fn duplicate_accounts_in_a_manual_directory_are_deduplicated_before_insert() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let run = store
        .enqueue_manual(
            "duplicate-directory",
            &[account("a"), account("a")],
            &settings(),
            now,
        )
        .unwrap();
    let task_items = items(&store, &run.id);
    assert_eq!(task_items.len(), 1);
    assert_eq!(task_items[0]["accountId"], "a");
    assert_eq!(run.total, 1);
}

#[test]
fn manual_scope_ignores_auto_participation_but_preserves_model_and_jitter() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let settings = Settings {
        account_overrides: vec![AccountOverride {
            account_id: "a".into(),
            enabled: false,
            daily_time: None,
            model: Some("chosen-model".into()),
            reset_delay_seconds: None,
        }],
        ..Settings::default()
    };
    let run = store
        .enqueue_manual("stable", &[account("a")], &settings, now)
        .unwrap();
    let data = items(&store, &run.id);
    assert_eq!(data[0]["model"], "chosen-model");
    let due = at(data[0]["dueAt"].as_str().unwrap());
    assert_eq!(
        (due - now).num_seconds(),
        i64::from(jitter("manual:stable:a", 5, 15))
    );
    let reopened = fixture.open();
    assert_eq!(items(&reopened, &run.id)[0]["dueAt"], data[0]["dueAt"]);
    assert!(reopened.claim("new", now).unwrap().is_none());
    assert!(reopened.claim("new", due).unwrap().is_some());
}

#[test]
fn automatic_daily_merges_recent_success_and_missed_days_never_catch_up_all() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let settings = Settings {
        enabled: true,
        daily_enabled: true,
        reset_enabled: false,
        ..settings()
    };
    store.configure(0, &settings).unwrap();
    let manual = store
        .enqueue_manual("manual", &[account("a")], &settings, now)
        .unwrap();
    let task = store.claim("one", now).unwrap().unwrap();
    store.finish(&task.id, "one", &accepted(), now).unwrap();
    store
        .schedule(
            &[account("a")],
            &settings,
            now + chrono::Duration::seconds(10),
        )
        .unwrap();
    let history = store.history(20, None).unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 2);
    let daily = history["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["id"] != manual.id)
        .unwrap();
    assert_eq!(
        items(&store, daily["id"].as_str().unwrap())[0]["reasonCode"],
        "recently_accepted"
    );
    store
        .schedule(
            &[account("a")],
            &settings,
            now + chrono::Duration::seconds(20),
        )
        .unwrap();
    assert_eq!(
        store.history(20, None).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    store
        .schedule(
            &[account("b")],
            &settings,
            now + chrono::Duration::days(5) + chrono::Duration::hours(7),
        )
        .unwrap();
    let history = store.history(20, None).unwrap();
    assert_eq!(history["runs"].as_array().unwrap().len(), 3);
    assert_eq!(
        items(&store, history["runs"][0]["id"].as_str().unwrap())[0]["reasonCode"],
        "missed_schedule"
    );
}

#[test]
fn reset_requires_real_five_hour_observation_and_does_not_predict_windows() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let settings = Settings {
        enabled: true,
        reset_delay_seconds: 30,
        ..settings()
    };
    store.configure(0, &settings).unwrap();
    store.schedule(&[account("a")], &settings, now).unwrap();
    assert!(
        store.history(20, None).unwrap()["runs"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut observed = account("a");
    observed.short_term = Some(ShortTerm {
        observed_at: Some(now - chrono::Duration::hours(1)),
        window_seconds: 86400,
        used_percent: Some(10.0),
        reset_at: Some(now),
    });
    store.observe(&[observed.clone()]).unwrap();
    store
        .schedule(
            &[account("a")],
            &settings,
            now + chrono::Duration::seconds(31),
        )
        .unwrap();
    assert!(
        store
            .claim("one", now + chrono::Duration::seconds(31))
            .unwrap()
            .is_none()
    );
    observed.short_term.as_mut().unwrap().window_seconds = 18000;
    store.observe(&[observed]).unwrap();
    store
        .schedule(
            &[account("a")],
            &settings,
            now + chrono::Duration::seconds(29),
        )
        .unwrap();
    assert!(
        store
            .claim("one", now + chrono::Duration::seconds(29))
            .unwrap()
            .is_none()
    );
    store
        .schedule(
            &[account("a")],
            &settings,
            now + chrono::Duration::seconds(31),
        )
        .unwrap();
    let task = store
        .claim("one", now + chrono::Duration::seconds(31))
        .unwrap()
        .unwrap();
    assert_eq!(task.source, "reset");
    store
        .finish(
            &task.id,
            "one",
            &accepted(),
            now + chrono::Duration::seconds(32),
        )
        .unwrap();
    store
        .schedule(&[account("a")], &settings, now + chrono::Duration::hours(5))
        .unwrap();
    assert!(
        store
            .claim("one", now + chrono::Duration::hours(5))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.history(20, None).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cancelling_a_joined_batch_preserves_other_batch_and_never_cancels_sent_items() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let first = store
        .enqueue_manual("first", &[account("a")], &settings(), now)
        .unwrap();
    let second = store
        .enqueue_manual("second", &[account("a")], &settings(), now)
        .unwrap();
    store.cancel(&first.id, now).unwrap();
    let task = store.claim("one", now).unwrap().unwrap();
    store.cancel(&second.id, now).unwrap();
    assert_eq!(items(&store, &second.id)[0]["status"], "running");
    store.finish(&task.id, "one", &accepted(), now).unwrap();
    let third = store
        .enqueue_manual("third", &[account("b")], &settings(), now)
        .unwrap();
    assert_eq!(store.cancel(&third.id, now).unwrap().status, "cancelled");
    assert!(store.claim("one", now).unwrap().is_none());
}

#[test]
fn persisted_gap_prevents_fast_responses_from_bursting_overdue_batch_across_instances() {
    let fixture = Fixture::new();
    let first = fixture.open();
    let second = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let settings = Settings {
        jitter_min_seconds: 10,
        jitter_max_seconds: 10,
        ..Settings::default()
    };
    first
        .enqueue_manual("fast-batch", &[account("a"), account("b")], &settings, now)
        .unwrap();
    let sent_at = now + chrono::Duration::seconds(30);
    let task = first.claim("old", sent_at).unwrap().unwrap();
    first.finish(&task.id, "old", &accepted(), sent_at).unwrap();
    assert!(second.claim("new", sent_at).unwrap().is_none());
    assert!(
        second
            .claim("new", sent_at + chrono::Duration::seconds(9))
            .unwrap()
            .is_none()
    );
    let runtime = second.runtime(sent_at, &[], &settings).unwrap();
    assert_eq!(
        at(runtime["nextRunAt"].as_str().unwrap()),
        sent_at + chrono::Duration::seconds(10)
    );
    let reopened = fixture.open();
    assert!(
        reopened
            .claim("third", sent_at + chrono::Duration::seconds(10))
            .unwrap()
            .is_some()
    );
}

#[test]
fn near_five_hour_windows_are_accepted_and_null_observation_never_breaks_directory() {
    let now = at("2026-10-25T00:00:00Z");
    for seconds in [17100, 18000, 18900] {
        let window = ShortTerm {
            observed_at: Some(now),
            window_seconds: seconds,
            used_percent: Some(1.0),
            reset_at: Some(now + chrono::Duration::hours(5)),
        };
        assert!(window.observed_reset().is_some());
    }
    for seconds in [17099, 18901, 86400] {
        let window = ShortTerm {
            observed_at: Some(now),
            window_seconds: seconds,
            used_percent: Some(1.0),
            reset_at: Some(now + chrono::Duration::hours(5)),
        };
        assert!(window.observed_reset().is_none());
    }
    let account: Account = serde_json::from_value(serde_json::json!({"id":"a","provider":"openai","authenticationKind":"oauth","enabled":true,"status":"active","supported":true,"reasonCode":null,"modelAllowed":null,"models":null,"shortTerm":{"observedAt":null,"windowSeconds":18000,"usedPercent":null,"resetAt":"2026-10-25T05:00:00Z"}})).unwrap();
    assert!(account.short_term.unwrap().observed_reset().is_none());
}
