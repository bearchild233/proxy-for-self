//! Worker leader lease 通过 StoreBundle 的中立能力进行集成验证。

use std::collections::BTreeSet;
use std::str::FromStr;
use std::time::Duration;

use gateway_core::task::{WorkerId, WorkerKind, WorkerLeaseAcquisition, WorkerLeaseRequest};
use gateway_store::{StoreConfig, initialize};
use sqlx::{
    ConnectOptions as _, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use uuid::Uuid;

#[tokio::test]
async fn store_bundle_worker_plan_and_leader_lease_are_single_use_and_fenced() {
    let (Some(database_url), Some(redis_url)) = (
        crate::support::test_env("CPR_TEST_DATABASE_URL"),
        crate::support::test_env("CPR_TEST_REDIS_URL"),
    ) else {
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    let runtime_data = tempfile::tempdir().expect("Store runtime data");
    let pool = gateway_store::postgres::connect_and_migrate(&database.url, Default::default())
        .await
        .expect("initialize shared schema");
    pool.close().await;
    let mut config = store_config(&database.url, &redis_url, "a");
    config
        .resolve_and_validate(runtime_data.path())
        .expect("resolved Store config");
    let mut first = initialize(config.clone())
        .await
        .expect("first Store bundle");
    first.complete_startup().await.expect("first slot ready");

    assert!(runtime_data.path().join("backup-staging").is_dir());

    let kinds = first
        .take_worker_contributions()
        .into_iter()
        .map(|contribution| contribution.kind())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        kinds,
        BTreeSet::from([
            WorkerKind::StaleModelRequestRecovery,
            WorkerKind::Retention,
            WorkerKind::OpsFlush,
        ])
    );
    assert!(first.take_worker_contributions().is_empty());

    let worker = WorkerId::try_new(
        WorkerKind::Retention,
        format!("test-{}", Uuid::new_v4().simple()),
    )
    .expect("worker ID");
    let request =
        WorkerLeaseRequest::try_new(worker, Duration::from_secs(5)).expect("worker lease request");
    let first_port = first.worker_leader_lease();
    let mut first_guard = match first_port
        .try_acquire(request.clone())
        .await
        .expect("first acquisition")
    {
        WorkerLeaseAcquisition::Acquired(guard) => guard,
        WorkerLeaseAcquisition::Busy { .. } => panic!("fresh worker lease must be available"),
    };
    let first_token = first_guard.fencing_token();
    // B 加入时必须保留 A 已持有的后台任务租约。
    let mut config = store_config(&database.url, &redis_url, "b");
    config
        .resolve_and_validate(runtime_data.path())
        .expect("second slot config");
    let mut second = initialize(config).await.expect("second Store bundle");
    second.complete_startup().await.expect("second slot ready");
    let second_port = second.worker_leader_lease();
    assert!(matches!(
        second_port
            .try_acquire(request.clone())
            .await
            .expect("contended acquisition"),
        WorkerLeaseAcquisition::Busy { .. }
    ));
    first_guard.renew().await.expect("leader lease renewal");
    first_guard.release().await.expect("leader lease release");

    let second_guard = match second_port
        .try_acquire(request)
        .await
        .expect("acquisition after release")
    {
        WorkerLeaseAcquisition::Acquired(guard) => guard,
        WorkerLeaseAcquisition::Busy { .. } => panic!("released worker lease must be reusable"),
    };
    assert!(second_guard.fencing_token() > first_token);
    second_guard.release().await.expect("second lease release");

    drop(first);
    drop(second);
    database.close().await;
}

struct TestDatabase {
    admin: PgPool,
    name: String,
    url: String,
}

#[tokio::test]
async fn slot_schema_validation_rejects_mismatch_without_repairing_database() {
    let Some(database_url) = crate::support::test_env("CPR_TEST_DATABASE_URL") else {
        return;
    };
    let database = TestDatabase::create(&database_url).await;
    assert!(
        gateway_store::postgres::connect_for_slot(&database.url, Default::default())
            .await
            .is_err()
    );
    let pool = gateway_store::postgres::connect_and_migrate(&database.url, Default::default())
        .await
        .unwrap();
    let validated = gateway_store::postgres::connect_for_slot(&database.url, Default::default())
        .await
        .unwrap();
    validated.close().await;
    sqlx::query("update _sqlx_migrations set success=false where version=(select min(version) from _sqlx_migrations)")
        .execute(&pool).await.unwrap();
    assert!(
        gateway_store::postgres::connect_for_slot(&database.url, Default::default())
            .await
            .is_err()
    );
    let unsuccessful: i64 =
        sqlx::query_scalar("select count(*) from _sqlx_migrations where not success")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        unsuccessful, 1,
        "slot startup must not repair or modify migrations"
    );
    pool.close().await;
    database.close().await;
}

impl TestDatabase {
    async fn create(database_url: &str) -> Self {
        let name = format!("cpr_store_worker_{}", Uuid::new_v4().simple());
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(database_url)
            .await
            .expect("connect test PostgreSQL");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("create database \"{name}\"")))
            .execute(&admin)
            .await
            .expect("create worker test database");
        let url = PgConnectOptions::from_str(database_url)
            .expect("parse test PostgreSQL URL")
            .database(&name)
            .to_url_lossy()
            .to_string();
        Self { admin, name, url }
    }

    async fn close(self) {
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop database \"{}\" with (force)",
            self.name
        )))
        .execute(&self.admin)
        .await
        .expect("drop worker test database");
        self.admin.close().await;
    }
}

fn store_config(database_url: &str, redis_url: &str, slot: &str) -> StoreConfig {
    let (database_url, database_password) = split_connection_url(database_url);
    let (redis_url, redis_password) = split_connection_url(redis_url);
    serde_json::from_value(serde_json::json!({
        "deployment_slot": slot,
        "database": { "url": database_url, "password": database_password },
        "redis": { "url": redis_url, "password": redis_password },
    }))
    .expect("test Store config")
}

fn split_connection_url(value: &str) -> (String, String) {
    let mut url = url::Url::parse(value).expect("test connection URL");
    let password = url.password().expect("test connection password").to_owned();
    url.set_password(None)
        .expect("connection URL supports credentials");
    (url.to_string(), password)
}
