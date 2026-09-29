use gateway_admin::model::client_keys::{NewClientKey, SetClientKeyBinding};
use gateway_admin::model::{MutationActor, MutationContext};
use gateway_admin::ports::store::{AdminStoreErrorKind, ClientKeyStore};
use gateway_core::account::ProviderAccountId;
use gateway_core::policy::{ClientApiKeyId, RateLimits};
use gateway_store::postgres::{
    PgAdminClientKeyStore, PgRuntimeSnapshotRepository, RuntimeSnapshotRepository,
};

use super::TestDatabase;

fn key_id(value: &str) -> ClientApiKeyId {
    ClientApiKeyId::new(value).expect("key id")
}
fn account_id(value: &str) -> ProviderAccountId {
    ProviderAccountId::new(value).expect("account id")
}
fn context() -> MutationContext {
    MutationContext {
        actor: MutationActor::System,
        request_id: "binding-integration".to_owned(),
    }
}
fn key(id: &str, marker: char) -> NewClientKey {
    NewClientKey {
        openai_client_profile_override: None,
        xai_client_profile_override: None,
        id: key_id(id),
        name: id.to_owned(),
        label: None,
        group_ids: Vec::new(),
        limits: RateLimits::unlimited(),
        budget: Default::default(),
        plaintext: format!("sk_{}", marker.to_string().repeat(43)),
    }
}

async fn seed_account(database: &TestDatabase, id: &str) {
    sqlx::query(
        "insert into provider_accounts (id, provider_kind, name, authentication_kind,
          provider_credentials_json, has_refresh_token, credential_observed_at, created_at, updated_at)
         values ($1, 'openai', $1, 'oauth', '{}'::jsonb, false, now(), now(), now())",
    ).bind(id).execute(&database.pool).await.expect("seed account");
}

#[tokio::test]
async fn bound_creation_and_update_are_atomic_and_key_scoped() {
    let Some(database) = TestDatabase::create("key_binding").await else {
        return;
    };
    seed_account(&database, "acct_a").await;
    seed_account(&database, "acct_b").await;
    let store = PgAdminClientKeyStore::new(database.pool.clone());
    store
        .create_bound_client_key(
            key("key_a", 'a'),
            Some(account_id("acct_a")),
            None,
            false,
            &context(),
        )
        .await
        .expect("native key");
    store
        .create_bound_client_key(
            key("key_b", 'b'),
            Some(account_id("acct_a")),
            None,
            true,
            &context(),
        )
        .await
        .expect("Excel key");
    let before = store
        .load_client_key_bindings(&[key_id("key_a"), key_id("key_b")])
        .await
        .expect("bindings");
    assert!(!before.items[0].excel_bridge_enabled);
    assert!(before.items[1].excel_bridge_enabled);
    assert_eq!(before.items[0].account_id, before.items[1].account_id);
    let repository = PgRuntimeSnapshotRepository::new(database.pool.clone());
    let generations_before = repository.load_runtime_snapshot().await.expect("snapshot");
    assert!(generations_before.client_api_keys[0].binding_revision > 0);
    let command = SetClientKeyBinding {
        id: key_id("key_b"),
        account_id: Some(account_id("acct_b")),
        routing: None,
        excel_bridge_enabled: false,
        expected_revision: before.config_revision,
    };
    store
        .set_client_key_binding(command.clone(), &context())
        .await
        .expect("change second key");
    let stale = store
        .set_client_key_binding(command, &context())
        .await
        .expect_err("stale update");
    assert_eq!(stale.kind(), AdminStoreErrorKind::StaleRevision);
    let after = store
        .load_client_key_bindings(&[key_id("key_a"), key_id("key_b")])
        .await
        .expect("bindings");
    assert_eq!(after.items[0].account_id, Some(account_id("acct_a")));
    assert_eq!(after.items[1].account_id, Some(account_id("acct_b")));
    assert!(!after.items[0].excel_bridge_enabled);
    assert!(!after.items[1].excel_bridge_enabled);
    let generations_after = repository.load_runtime_snapshot().await.expect("snapshot");
    assert_eq!(
        generations_before.client_api_keys[0].binding_revision,
        generations_after.client_api_keys[0].binding_revision
    );
    assert!(
        generations_after.client_api_keys[1].binding_revision
            > generations_before.client_api_keys[1].binding_revision
    );
    let revision = after.config_revision;
    let missing = store
        .create_bound_client_key(
            key("key_orphan", 'c'),
            Some(account_id("acct_missing")),
            None,
            false,
            &context(),
        )
        .await
        .expect_err("unknown account");
    assert_eq!(missing.kind(), AdminStoreErrorKind::NotFound);
    let count: i64 =
        sqlx::query_scalar("select count(*) from client_api_keys where id = 'key_orphan'")
            .fetch_one(&database.pool)
            .await
            .expect("no orphan key");
    assert_eq!(count, 0);
    assert_eq!(
        store
            .load_client_key_bindings(&[key_id("key_a")])
            .await
            .expect("revision")
            .config_revision,
        revision
    );
    let routing = gateway_core::account::scope::KeyRoutingOptions {
        mode: "accounts".to_owned(),
        account_ids: vec!["acct_a".to_owned(), "acct_b".to_owned()],
        group_ids: Vec::new(),
        rotation_strategy: Some("round_robin".to_owned()),
    };
    store
        .create_bound_client_key(
            key("key_pool", 'd'),
            None,
            Some(routing.clone()),
            false,
            &context(),
        )
        .await
        .expect("account pool");
    let bindings = store
        .load_client_key_bindings(&[key_id("key_pool")])
        .await
        .unwrap();
    assert_eq!(bindings.items[0].routing, Some(routing));
    assert!(bindings.items[0].account_id.is_none());
    let snapshot = repository.load_runtime_snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .client_api_keys
            .iter()
            .find(|key| key.id.as_str() == "key_pool")
            .unwrap()
            .routing
            .as_ref()
            .unwrap()
            .rotation_strategy
            .as_deref(),
        Some("round_robin")
    );
    database.close().await;
}

#[tokio::test]
async fn removed_account_and_unbound_key_stay_unbound_in_runtime_snapshot() {
    let Some(database) = TestDatabase::create("binding_delete").await else {
        return;
    };
    seed_account(&database, "acct_a").await;
    let store = PgAdminClientKeyStore::new(database.pool.clone());
    store
        .create_bound_client_key(
            key("key_a", 'a'),
            Some(account_id("acct_a")),
            None,
            true,
            &context(),
        )
        .await
        .expect("bound key");
    store
        .create_client_key(key("key_unbound", 'b'), &context())
        .await
        .expect("unbound legacy key");
    let repository = PgRuntimeSnapshotRepository::new(database.pool.clone());
    let before = repository.load_runtime_snapshot().await.expect("snapshot");
    assert_eq!(
        before.client_api_keys[0].bound_account_id,
        Some(account_id("acct_a"))
    );
    assert!(before.client_api_keys[0].excel_bridge_enabled);
    assert!(before.client_api_keys[1].bound_account_id.is_none());
    sqlx::query("delete from provider_accounts where id = 'acct_a'")
        .execute(&database.pool)
        .await
        .expect("delete account");
    let after = repository.load_runtime_snapshot().await.expect("snapshot");
    assert!(
        after
            .client_api_keys
            .iter()
            .all(|item| item.bound_account_id.is_none())
    );
    assert!(after.client_api_keys[0].excel_bridge_enabled);
    database.close().await;
}
