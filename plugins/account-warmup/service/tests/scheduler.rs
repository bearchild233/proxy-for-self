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
            reasoning_effort: None,
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

#[test]
fn prompt_and_reasoning_settings_validate_utf8_bytes_and_preserve_text() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let mut config = settings();
    for effort in ["auto", "none", "minimal", "low", "medium", "high", "xhigh"] {
        config.reasoning_effort = effort.into();
        assert!(validate(&config).is_ok());
    }
    for effort in ["", "AUTO", "max", " high", "high "] {
        config.reasoning_effort = effort.into();
        assert!(validate(&config).is_err());
    }
    config.reasoning_effort = "auto".into();
    for prompt in [
        String::new(),
        " \r\n\t".into(),
        "x\0".into(),
        "x\u{000B}".into(),
        "x\u{0085}".into(),
        "中".repeat(1366),
    ] {
        config.prompt = prompt;
        assert!(validate(&config).is_err());
    }
    config.prompt = "中".repeat(1365) + "a";
    assert_eq!(config.prompt.len(), 4096);
    assert!(validate(&config).is_ok());
    config.prompt.push(' ');
    assert!(validate(&config).is_err());
    config.prompt = "  早上好\n第二行\r\n\t结尾  ".into();
    store.configure(0, &config).unwrap();
    assert_eq!(fixture.open().settings().unwrap().1.prompt, config.prompt);
    config.account_overrides = vec![AccountOverride {
        account_id: "a".into(),
        enabled: true,
        daily_time: None,
        model: None,
        reasoning_effort: Some("invalid".into()),
        reset_delay_seconds: None,
    }];
    assert!(validate(&config).is_err());
}

#[test]
fn manual_and_automatic_tasks_freeze_prompt_and_account_effort_across_configure() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let mut config = Settings {
        enabled: true,
        daily_enabled: true,
        reset_enabled: false,
        prompt: "  original\n\tprompt  ".into(),
        reasoning_effort: "low".into(),
        account_overrides: vec![AccountOverride {
            account_id: "automatic".into(),
            enabled: true,
            daily_time: None,
            model: Some("account-model".into()),
            reasoning_effort: Some("high".into()),
            reset_delay_seconds: None,
        }],
        ..settings()
    };
    store.configure(0, &config).unwrap();
    let manual = store
        .enqueue_manual("frozen-manual", &[account("manual")], &config, now)
        .unwrap();
    store
        .schedule(&[account("automatic")], &config, now)
        .unwrap();
    config.prompt = "changed".into();
    config.reasoning_effort = "none".into();
    config.default_model = "changed-model".into();
    config.account_overrides[0].reasoning_effort = Some("xhigh".into());
    config.account_overrides[0].model = Some("changed-account-model".into());
    store.configure(1, &config).unwrap();
    let reopened = fixture.open();
    let first = reopened.claim("executor", now).unwrap().unwrap();
    assert_eq!(first.prompt, "  original\n\tprompt  ");
    reopened
        .finish(&first.id, "executor", &accepted(), now)
        .unwrap();
    let second = reopened.claim("executor", now).unwrap().unwrap();
    assert_eq!(second.prompt, "  original\n\tprompt  ");
    let (manual_task, automatic_task) = if first.account_id == "manual" {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(manual_task.reasoning_effort, "low");
    assert_eq!(automatic_task.reasoning_effort, "high");
    assert_eq!(automatic_task.model, "account-model");
    assert_eq!(
        items(&reopened, &manual.id)[0]["prompt"],
        "  original\n\tprompt  "
    );
    assert_eq!(reopened.settings().unwrap().1, config);
}

#[test]
fn old_sqlite_settings_and_queued_task_json_keep_hi_auto_defaults() {
    let fixture = Fixture::new();
    let now = at("2026-10-25T00:00:00Z");
    let store = fixture.open();
    let run = store
        .enqueue_manual("old-fixture", &[account("legacy")], &settings(), now)
        .unwrap();
    let mut old_settings = serde_json::to_value(Settings {
        account_overrides: vec![AccountOverride {
            account_id: "legacy".into(),
            enabled: true,
            daily_time: None,
            model: None,
            reasoning_effort: None,
            reset_delay_seconds: None,
        }],
        ..settings()
    })
    .unwrap();
    old_settings.as_object_mut().unwrap().remove("prompt");
    old_settings
        .as_object_mut()
        .unwrap()
        .remove("reasoningEffort");
    old_settings["accountOverrides"][0]
        .as_object_mut()
        .unwrap()
        .remove("reasoningEffort");
    let mut old_task = items(&store, &run.id).remove(0);
    old_task.as_object_mut().unwrap().remove("prompt");
    old_task.as_object_mut().unwrap().remove("reasoningEffort");
    drop(store);
    let db = rusqlite::Connection::open(fixture.0.join("state.sqlite3")).unwrap();
    db.execute("UPDATE settings SET data=?", [old_settings.to_string()])
        .unwrap();
    db.execute("UPDATE tasks SET data=?", [old_task.to_string()])
        .unwrap();
    drop(db);
    let reopened = fixture.open();
    let mut changed = reopened.settings().unwrap().1;
    assert_eq!(changed.prompt, "hi");
    assert_eq!(changed.reasoning_effort, "auto");
    assert_eq!(changed.account_overrides[0].reasoning_effort, None);
    changed.prompt = "new configuration".into();
    changed.reasoning_effort = "high".into();
    reopened.configure(0, &changed).unwrap();
    let task = reopened.claim("new-service", now).unwrap().unwrap();
    assert_eq!(task.prompt, "hi");
    assert_eq!(task.reasoning_effort, "auto");
}

#[test]
fn account_states_follow_active_real_current_window_and_expiry() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let target = account("a");
    assert_eq!(
        store
            .account_states(now, std::slice::from_ref(&target))
            .unwrap()["a"]
            .status,
        "pending"
    );
    store
        .enqueue_manual("state-one", std::slice::from_ref(&target), &settings(), now)
        .unwrap();
    assert_eq!(
        store
            .account_states(now, std::slice::from_ref(&target))
            .unwrap()["a"]
            .status,
        "queued"
    );
    let task = store.claim("one", now).unwrap().unwrap();
    assert_eq!(
        store
            .account_states(now, std::slice::from_ref(&target))
            .unwrap()["a"]
            .status,
        "running"
    );
    store.finish(&task.id, "one", &accepted(), now).unwrap();
    assert_eq!(
        store
            .account_states(now, std::slice::from_ref(&target))
            .unwrap()["a"]
            .status,
        "unconfirmed"
    );
    store
        .enqueue_manual("state-two", std::slice::from_ref(&target), &settings(), now)
        .unwrap();
    let task = store.claim("one", now).unwrap().unwrap();
    let mut confirmed = accepted();
    confirmed.short_term = Some(ShortTerm {
        observed_at: Some(now),
        window_seconds: 18000,
        used_percent: Some(1.0),
        reset_at: Some(now + chrono::Duration::hours(5)),
    });
    store.finish(&task.id, "one", &confirmed, now).unwrap();
    let reopened = fixture.open();
    let states = reopened
        .account_states(now, std::slice::from_ref(&target))
        .unwrap();
    assert_eq!(states["a"].status, "warmed");
    assert_eq!(states["a"].task_id.as_deref(), Some(task.id.as_str()));
    assert_eq!(states["a"].reset_at, Some(now + chrono::Duration::hours(5)));
    let expired = reopened
        .account_states(now + chrono::Duration::hours(5), &[target])
        .unwrap();
    assert_eq!(expired["a"].status, "pending");
    assert_eq!(expired["a"].reset_at, None);
}

#[test]
fn account_states_keep_unknown_and_accept_business_window_without_claiming_manual_success() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let mut target = account("a");
    store
        .enqueue_manual(
            "state-unknown",
            std::slice::from_ref(&target),
            &settings(),
            now,
        )
        .unwrap();
    let task = store.claim("lost-executor", now).unwrap().unwrap();
    store.recover(now + chrono::Duration::seconds(61)).unwrap();
    let states = store
        .account_states(
            now + chrono::Duration::seconds(61),
            std::slice::from_ref(&target),
        )
        .unwrap();
    assert_eq!(states["a"].status, "unknown");
    assert_eq!(states["a"].task_id.as_deref(), Some(task.id.as_str()));
    assert_eq!(
        states["a"].reason_code.as_deref(),
        Some("executor_lease_expired")
    );
    target.short_term = Some(ShortTerm {
        observed_at: Some(now),
        window_seconds: 18000,
        used_percent: Some(1.0),
        reset_at: Some(now + chrono::Duration::hours(5)),
    });
    let states = store
        .account_states(
            now + chrono::Duration::seconds(61),
            std::slice::from_ref(&target),
        )
        .unwrap();
    assert_eq!(states["a"].status, "warmed");
    assert_eq!(states["a"].task_id, None);
    let history = store.history(20, None).unwrap();
    assert_eq!(history["runs"][0]["unknown"], 1);
    assert!(
        store
            .claim("new", now + chrono::Duration::minutes(10))
            .unwrap()
            .is_none()
    );
}

#[test]
fn invalid_or_future_windows_never_confirm_a_timer_or_account_state() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    for (id, seconds, observed, reset) in [
        (
            "future",
            18000,
            now + chrono::Duration::seconds(1),
            now + chrono::Duration::hours(5),
        ),
        (
            "wrong-duration",
            86400,
            now,
            now + chrono::Duration::hours(5),
        ),
        ("reversed", 18000, now, now),
    ] {
        let mut target = account(id);
        target.short_term = Some(ShortTerm {
            observed_at: Some(observed),
            window_seconds: seconds,
            used_percent: None,
            reset_at: Some(reset),
        });
        assert_eq!(
            store
                .account_states(now, std::slice::from_ref(&target))
                .unwrap()[id]
                .status,
            "pending"
        );
        let run = store
            .enqueue_manual(id, std::slice::from_ref(&target), &settings(), now)
            .unwrap();
        let task = store.claim("executor", now).unwrap().unwrap();
        let mut outcome = accepted();
        outcome.short_term = target.short_term;
        store.finish(&task.id, "executor", &outcome, now).unwrap();
        assert_eq!(items(&store, &run.id)[0]["timerConfirmed"], false);
        assert_eq!(items(&store, &run.id)[0]["resetAt"], Value::Null);
    }
    let future = account("future");
    assert_eq!(
        store.account_states(now, &[future]).unwrap()["future"].status,
        "unconfirmed"
    );
    let mut unsupported = account("unsupported");
    unsupported.supported = false;
    unsupported.reason_code = Some("unsupported_provider".into());
    assert_eq!(
        store.account_states(now, &[unsupported]).unwrap()["unsupported"].status,
        "unsupported"
    );
}

#[test]
fn history_filters_source_before_limit_and_rejects_a_mismatched_run_id() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let config = Settings {
        enabled: true,
        daily_enabled: true,
        reset_enabled: false,
        ..settings()
    };
    store.configure(0, &config).unwrap();
    store
        .schedule(&[account("automatic")], &config, now)
        .unwrap();
    let auto = store.history_filtered(1, None, Some("automatic")).unwrap()["runs"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for index in 1..=3 {
        store
            .enqueue_manual(
                &format!("manual-{index}"),
                &[account(&format!("manual-{index}"))],
                &config,
                now + chrono::Duration::minutes(index),
            )
            .unwrap();
    }
    let automatic = store.history_filtered(1, None, Some("automatic")).unwrap();
    assert_eq!(automatic["runs"].as_array().unwrap().len(), 1);
    assert_eq!(automatic["runs"][0]["id"], auto);
    assert_eq!(automatic["runs"][0]["source"], "daily");
    let manual = store.history_filtered(1, None, Some("manual")).unwrap();
    assert_eq!(manual["runs"][0]["source"], "manual");
    assert!(
        store
            .history_filtered(20, Some(&auto), Some("manual"))
            .is_err()
    );
    assert!(
        store
            .history_filtered(20, manual["runs"][0]["id"].as_str(), Some("automatic"))
            .is_err()
    );
    assert_eq!(
        store.history(20, None).unwrap()["runs"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn read_summaries_project_only_state_fields_and_use_account_indexes() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let run = store
        .enqueue_manual("summary-only", &[account("a")], &settings(), now)
        .unwrap();
    let task = store.claim("executor", now).unwrap().unwrap();
    store
        .finish(&task.id, "executor", &accepted(), now)
        .unwrap();
    let db = rusqlite::Connection::open(fixture.0.join("state.sqlite3")).unwrap();
    // 摘要不能依赖解析整条 TaskItem；提示词列也不应出现在账号 read 的摘要里。
    db.execute("UPDATE tasks SET data=json_set(data,'$.prompt',42)", [])
        .unwrap();
    let plan: String = db.query_row("EXPLAIN QUERY PLAN SELECT id FROM tasks WHERE account='a' AND finished IS NOT NULL ORDER BY finished DESC,id DESC LIMIT 1", [], |row| row.get(3)).unwrap();
    assert!(plan.contains("task_account_finished"));
    let states = store.account_states(now, &[account("a")]).unwrap();
    assert_eq!(states["a"].status, "unconfirmed");
    assert!(!serde_json::to_string(&states).unwrap().contains("prompt"));
    assert_eq!(
        store.runtime(now, &[], &settings()).unwrap()["lastRun"]["id"],
        run.id
    );
}

#[test]
fn active_tasks_take_priority_over_business_windows_and_terminal_failures_stay_distinct() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let mut target = account("a");
    target.short_term = Some(ShortTerm {
        observed_at: Some(now),
        window_seconds: 18000,
        used_percent: Some(2.0),
        reset_at: Some(now + chrono::Duration::hours(5)),
    });
    store
        .enqueue_manual(
            "active-window",
            std::slice::from_ref(&target),
            &settings(),
            now,
        )
        .unwrap();
    assert_eq!(
        store
            .account_states(now, std::slice::from_ref(&target))
            .unwrap()["a"]
            .status,
        "queued"
    );
    let task = store.claim("executor", now).unwrap().unwrap();
    assert_eq!(
        store
            .account_states(now, std::slice::from_ref(&target))
            .unwrap()["a"]
            .status,
        "running"
    );
    let mut failure = accepted();
    failure.outcome = "failed".into();
    failure.reason_code = Some("upstream_failed".into());
    store.finish(&task.id, "executor", &failure, now).unwrap();
    target.short_term = None;
    let state = store
        .account_states(now, std::slice::from_ref(&target))
        .unwrap();
    assert_eq!(state["a"].status, "failed");
    assert_eq!(state["a"].reason_code.as_deref(), Some("upstream_failed"));
    let run = store
        .enqueue_manual(
            "cancelled",
            &[target],
            &settings(),
            now + chrono::Duration::seconds(1),
        )
        .unwrap();
    store
        .cancel(&run.id, now + chrono::Duration::seconds(1))
        .unwrap();
    assert_eq!(
        store
            .account_states(now + chrono::Duration::seconds(1), &[account("a")])
            .unwrap()["a"]
            .status,
        "skipped"
    );
}

#[test]
fn newer_expired_observations_do_not_revive_an_older_window() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    let mut target = account("a");
    target.short_term = Some(ShortTerm {
        observed_at: Some(now),
        window_seconds: 18000,
        used_percent: Some(1.0),
        reset_at: Some(now + chrono::Duration::hours(5)),
    });
    store.observe(std::slice::from_ref(&target)).unwrap();
    target.short_term.as_mut().unwrap().observed_at = Some(now + chrono::Duration::hours(1));
    target.short_term.as_mut().unwrap().reset_at = Some(now + chrono::Duration::hours(2));
    store.observe(std::slice::from_ref(&target)).unwrap();
    assert_eq!(
        store
            .account_states(now + chrono::Duration::minutes(90), &[account("a")])
            .unwrap()["a"]
            .status,
        "warmed"
    );
    assert_eq!(
        store
            .account_states(now + chrono::Duration::hours(3), &[account("a")])
            .unwrap()["a"]
            .status,
        "pending"
    );
}

#[test]
fn runtime_separates_manual_queues_from_automatic_forecasts_and_due_tasks() {
    let fixture = Fixture::new();
    let store = fixture.open();
    let now = at("2026-10-25T00:00:00Z");
    store
        .enqueue_manual("manual-only", &[account("manual")], &settings(), now)
        .unwrap();
    let runtime = store
        .runtime(now, &[account("automatic")], &settings())
        .unwrap();
    assert_eq!(at(runtime["nextRunAt"].as_str().unwrap()), now);
    assert_eq!(runtime["nextAutomaticRunAt"], Value::Null);
    let config = Settings {
        enabled: true,
        daily_enabled: true,
        reset_enabled: false,
        daily_time: "09:00".into(),
        ..settings()
    };
    store.configure(0, &config).unwrap();
    let runtime = store
        .runtime(now, &[account("automatic")], &config)
        .unwrap();
    assert_eq!(
        at(runtime["nextAutomaticRunAt"].as_str().unwrap()),
        now + chrono::Duration::hours(1)
    );
    store
        .schedule(
            &[account("automatic")],
            &config,
            now + chrono::Duration::hours(1),
        )
        .unwrap();
    let runtime = store
        .runtime(
            now + chrono::Duration::hours(1),
            &[account("automatic")],
            &config,
        )
        .unwrap();
    assert_eq!(
        at(runtime["nextAutomaticRunAt"].as_str().unwrap()),
        now + chrono::Duration::hours(1)
    );
}
