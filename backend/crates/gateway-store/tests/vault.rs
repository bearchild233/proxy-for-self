//! 从真实 Store 启动入口验证加密；独立子进程隔离一次性主密钥，不影响其他测试。
use gateway_admin::{
    model::{MutationActor, MutationContext, client_keys::NewClientKey},
    ports::store::ClientKeyStore as _,
};
use gateway_core::policy::{ClientApiKeyId, RateLimits};
use gateway_store::{
    StoreConfig,
    postgres::{ClientApiKeyRepository as _, PgAdminClientKeyStore, PgClientApiKeyRepository},
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;

#[tokio::test]
async fn encrypted_credentials_are_randomized_authenticated_and_scope_bound() {
    const CHILD_DATABASE: &str = "CPR_VAULT_TEST_DATABASE";
    let Some(database_url) = crate::support::test_env("CPR_TEST_DATABASE_URL") else {
        return;
    };
    let Some(redis_url) = crate::support::test_env("CPR_TEST_REDIS_URL") else {
        return;
    };
    let mut database_url = url::Url::parse(&database_url).unwrap();
    let Ok(database) = std::env::var(CHILD_DATABASE) else {
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(database_url.as_str())
            .await
            .unwrap();
        let database = format!("vault_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "create database \"{database}\""
        )))
        .execute(&admin)
        .await
        .unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "vault::encrypted_credentials_are_randomized_authenticated_and_scope_bound",
                "--nocapture",
            ])
            .env(CHILD_DATABASE, &database)
            .env_remove("CPR_DATABASE_URL")
            .env_remove("CPR_DATABASE_PASSWORD")
            .env_remove("CPR_REDIS_URL")
            .env_remove("CPR_REDIS_PASSWORD")
            .status()
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "drop database \"{database}\" with (force)"
        )))
        .execute(&admin)
        .await
        .unwrap();
        assert!(status.success(), "isolated vault integration test failed");
        return;
    };
    database_url.set_path(&database);
    let root = tempfile::tempdir().unwrap();
    let key_file = root.path().join("vault.hex");
    std::fs::write(&key_file, "07".repeat(32)).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let connection = |mut url: url::Url| {
        let password = url.password().unwrap().to_owned();
        url.set_password(None).unwrap();
        json!({"url":url.as_str(), "password":password})
    };
    let mut config: StoreConfig = serde_json::from_value(json!({
        "database":connection(database_url.clone()),
        "redis":connection(url::Url::parse(&redis_url).unwrap()),
        "vault_key_file":key_file, "pool":{"max_connections":2}
    }))
    .unwrap();
    config.resolve_and_validate(root.path()).unwrap();
    let _bundle = gateway_store::initialize(config).await.unwrap();
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(database_url.as_str())
        .await
        .unwrap();
    let admin = PgAdminClientKeyStore::new(pool.clone());
    let repository = PgClientApiKeyRepository::new(pool.clone());
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "vault-test".into(),
    };
    let secret = "synthetic-vault-secret";
    let mut encrypted = Vec::new();
    for _ in 0..2 {
        admin
            .create_client_key(
                NewClientKey {
                    id: ClientApiKeyId::new("vault_a").unwrap(),
                    name: "Vault A".into(),
                    label: None,
                    group_ids: Vec::new(),
                    limits: RateLimits::unlimited(),
                    budget: Default::default(),
                    plaintext: secret.into(),
                    openai_client_profile_override: None,
                    xai_client_profile_override: None,
                },
                &context,
            )
            .await
            .unwrap();
        let value: String =
            sqlx::query_scalar("select key from client_api_keys where id='vault_a'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(value.starts_with("hubenc1:"));
        assert!(!value.contains(secret));
        assert_eq!(
            repository
                .reveal_client_api_key("vault_a")
                .await
                .unwrap()
                .unwrap()
                .key,
            secret
        );
        encrypted.push(value);
        sqlx::query("delete from client_api_keys where id='vault_a'")
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_ne!(encrypted[0], encrypted[1]);
    sqlx::query("insert into client_api_keys(id,name,key,created_at,updated_at) values ('vault_a','Vault A',$1,now(),now()), ('vault_b','Vault B',$2,now(),now())")
        .bind(&encrypted[0]).bind(&encrypted[1]).execute(&pool).await.unwrap();
    assert!(repository.reveal_client_api_key("vault_b").await.is_err());
    let mut tampered = encrypted[0].clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == '0' { '1' } else { '0' });
    for invalid in [
        tampered,
        encrypted[0][..encrypted[0].len() - 2].into(),
        secret.into(),
        "hubenc1:00".into(),
    ] {
        sqlx::query("update client_api_keys set key=$1 where id='vault_a'")
            .bind(invalid)
            .execute(&pool)
            .await
            .unwrap();
        assert!(repository.reveal_client_api_key("vault_a").await.is_err());
    }
}
