use chrono::{DateTime, Duration, Utc};
use gateway_store::postgres::{
    ClientAdmissionRecentRequest, ClientAdmissionRecovery, ClientAdmissionRecoveryRepository,
    ClientAdmissionRunningRequest, PgClientAdmissionRecoveryRepository,
};
use sqlx::PgPool;

use super::TestDatabase;
// TestDatabase 隔离 schema；advisory lock 属于整个数据库，实例锁场景必须串行。
static INSTANCE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn ab_slots_preserve_live_requests_and_recover_only_after_both_exit() {
    let _serial = INSTANCE_TEST_LOCK.lock().await;
    let Some(database) = TestDatabase::create("ab_slots").await else {
        return;
    };
    let repository = PgClientAdmissionRecoveryRepository::new(database.pool.clone());
    let mut a = repository.acquire_slot("a").await.expect("cold A");
    assert!(a.needs_recovery());
    assert!(
        repository.acquire_slot("b").await.is_err(),
        "cold recovery is a startup barrier"
    );
    a.complete_startup().await.unwrap();
    let now = Utc::now();
    seed_request(
        &database.pool,
        "live-a",
        now,
        now + Duration::minutes(10),
        "running",
    )
    .await;
    let mut b = repository.acquire_slot("b").await.expect("join B");
    assert!(!b.needs_recovery());
    b.complete_startup().await.unwrap();
    assert!(
        repository.acquire_slot("a").await.is_err(),
        "duplicate A rejected"
    );
    let outcome: String =
        sqlx::query_scalar("select outcome from model_requests where id='live-a'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(outcome, "running", "B cannot mark A requests interrupted");
    a.close().await.unwrap();
    let mut next_a = repository
        .acquire_slot("a")
        .await
        .expect("next release uses A");
    assert!(!next_a.needs_recovery());
    next_a.complete_startup().await.unwrap();
    b.close().await.unwrap();
    next_a.close().await.unwrap();
    let mut cold = repository.acquire_slot("b").await.unwrap();
    assert!(cold.needs_recovery());
    let outcome: String =
        sqlx::query_scalar("select outcome from model_requests where id='live-a'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(outcome, "incomplete");
    cold.complete_startup().await.unwrap();
    cold.close().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn ab_slot_refuses_legacy_exclusive_gateway_without_mutation() {
    let _serial = INSTANCE_TEST_LOCK.lock().await;
    use sqlx::Connection;
    let Some(database) = TestDatabase::create("ab_legacy").await else {
        return;
    };
    let repository = PgClientAdmissionRecoveryRepository::new(database.pool.clone());
    let legacy = repository.recover_after_restart().await.unwrap();
    let now = Utc::now();
    seed_request(
        &database.pool,
        "legacy-live",
        now,
        now + Duration::minutes(10),
        "running",
    )
    .await;
    assert!(repository.acquire_slot("b").await.is_err());
    let outcome: String =
        sqlx::query_scalar("select outcome from model_requests where id='legacy-live'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(outcome, "running");
    legacy.close().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn restart_recovers_future_deadline_and_refuses_a_second_instance() {
    let _serial = INSTANCE_TEST_LOCK.lock().await;
    use sqlx::Connection;
    let Some(database) = TestDatabase::create("restart_recovery").await else {
        return;
    };
    let now = Utc::now();
    seed_request(
        &database.pool,
        "interrupted",
        now,
        now + Duration::minutes(10),
        "running",
    )
    .await;
    seed_request(
        &database.pool,
        "complete",
        now,
        now + Duration::minutes(10),
        "succeeded",
    )
    .await;
    let repository = PgClientAdmissionRecoveryRepository::new(database.pool.clone());
    let guard = repository
        .recover_after_restart()
        .await
        .expect("first instance");
    let outcome: (String, String) =
        sqlx::query_as("select outcome,error_kind from model_requests where id='interrupted'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(outcome, ("incomplete".into(), "process_interrupted".into()));
    seed_request(
        &database.pool,
        "live",
        now,
        now + Duration::minutes(10),
        "running",
    )
    .await;
    assert!(repository.recover_after_restart().await.is_err());
    let outcome: String = sqlx::query_scalar("select outcome from model_requests where id='live'")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        outcome, "running",
        "duplicate startup must not touch live requests"
    );
    guard.close().await.unwrap();
    let restarted = repository
        .recover_after_restart()
        .await
        .expect("restart after close");
    let outcome: String =
        sqlx::query_scalar("select outcome from model_requests where id='complete'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(outcome, "succeeded");
    restarted.close().await.unwrap();
    database.close().await;
}

#[tokio::test]
async fn recovery_loads_precise_window_and_running_request_facts() {
    let Some(database) = TestDatabase::create("admission_recovery").await else {
        return;
    };
    let now = DateTime::from_timestamp_micros(Utc::now().timestamp_micros())
        .expect("current time is representable at PostgreSQL precision");
    let window_started_at = now - Duration::seconds(60);
    seed_request(
        &database.pool,
        "old-running",
        now - Duration::seconds(120),
        now + Duration::seconds(30),
        "running",
    )
    .await;
    seed_request(
        &database.pool,
        "old-complete",
        now - Duration::seconds(90),
        now - Duration::seconds(30),
        "succeeded",
    )
    .await;
    seed_request(
        &database.pool,
        "recent-complete",
        now - Duration::seconds(20),
        now + Duration::seconds(10),
        "succeeded",
    )
    .await;
    seed_request(
        &database.pool,
        "recent-running",
        now - Duration::seconds(10),
        now + Duration::seconds(40),
        "running",
    )
    .await;

    let repository = PgClientAdmissionRecoveryRepository::new(database.pool.clone());
    let actual = repository
        .load_client_admission_recovery(window_started_at)
        .await
        .expect("load precise admission recovery facts");
    let expected = vec![ClientAdmissionRecovery {
        client_api_key_ref: "key-recovery".to_owned(),
        recent_requests: vec![
            ClientAdmissionRecentRequest {
                model_request_id: "recent-complete".to_owned(),
                started_at: now - Duration::seconds(20),
            },
            ClientAdmissionRecentRequest {
                model_request_id: "recent-running".to_owned(),
                started_at: now - Duration::seconds(10),
            },
        ],
        running_requests: vec![
            ClientAdmissionRunningRequest {
                model_request_id: "old-running".to_owned(),
                deadline_at: now + Duration::seconds(30),
            },
            ClientAdmissionRunningRequest {
                model_request_id: "recent-running".to_owned(),
                deadline_at: now + Duration::seconds(40),
            },
        ],
    }];
    assert_eq!(actual, expected);

    database.close().await;
}

async fn seed_request(
    pool: &PgPool,
    id: &str,
    started_at: DateTime<Utc>,
    deadline_at: DateTime<Utc>,
    outcome: &str,
) {
    let completed_at = (outcome != "running").then_some(started_at + Duration::seconds(1));
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, config_revision, protocol, operation, endpoint,
           client_transport, requested_model_id, outcome,
           started_at, deadline_at, completed_at,
           routing_scope, routing_group_refs, routing_group_names_snapshot
         ) values (
           $1, 'key-recovery', 1, 'openai', 'responses', '/v1/responses',
           'http_sse', 'coding', $2, $3, $4, $5,
           'all', '{}'::text[], '[]'::jsonb
         )",
    )
    .bind(id)
    .bind(outcome)
    .bind(started_at)
    .bind(deadline_at)
    .bind(completed_at)
    .execute(pool)
    .await
    .expect("seed model request recovery fact");
}
