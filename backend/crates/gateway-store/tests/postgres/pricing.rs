use gateway_admin::model::{
    MutationActor, MutationContext,
    pricing::{PricingChange, UpdatePricing},
};
use gateway_core::metering::ModelPriceOverride;
use gateway_store::postgres::{PgRuntimeSnapshotRepository, RuntimeSnapshotRepository};
use serde_json::json;

use super::TestDatabase;

async fn pricing_store(name: &str) -> Option<(TestDatabase, gateway_store::StoreBundle)> {
    let redis_url = crate::support::test_env("CPR_TEST_REDIS_URL")?;
    let database = TestDatabase::create(name).await?;
    let mut database_url = url::Url::parse(
        &crate::support::test_env("CPR_TEST_DATABASE_URL").expect("test database URL"),
    )
    .unwrap();
    database_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", database.schema));
    let connection = |mut url: url::Url| {
        let password = url.password().expect("test password").to_owned();
        url.set_password(None).unwrap();
        json!({"url":url.as_str(), "password":password})
    };
    let mut config: gateway_store::StoreConfig = serde_json::from_value(json!({
        "database":connection(database_url),
        "redis":connection(url::Url::parse(&redis_url).unwrap()),
    }))
    .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    config.resolve_and_validate(runtime.path()).unwrap();
    let bundle = gateway_store::initialize(config).await.unwrap();
    Some((database, bundle))
}

#[tokio::test]
async fn shared_order_preserves_hidden_items_paginates_and_location_survives_reload() {
    use gateway_admin::model::settings::ReorderDisplay;
    use gateway_store::postgres::{
        ClientApiKeyListQuery, ClientApiKeyRepository as _, ClientApiKeySort,
        ClientApiKeySortDirection, ClientApiKeySortField, PgClientApiKeyRepository,
    };
    let Some((database, bundle)) = pricing_store("shared_order").await else {
        return;
    };
    let settings = bundle.admin_ports().settings();
    for id in ["a", "b", "c", "d"] {
        sqlx::query("insert into client_api_keys (id,name,key,created_at,updated_at) values ($1,$1,$2,now(),now())")
            .bind(id).bind(format!("sk_{}", id.repeat(43))).execute(&database.pool).await.unwrap();
    }
    let reorder = || ReorderDisplay {
        scope: "keys".to_owned(),
        original_ids: vec!["a".into(), "c".into()],
        ordered_ids: vec!["c".into(), "a".into()],
    };
    reorder().validate().unwrap();
    let revision = settings
        .load_runtime_settings()
        .await
        .unwrap()
        .config_revision;
    settings.reorder_display(reorder()).await.unwrap();
    assert!(
        settings.reorder_display(reorder()).await.is_err(),
        "stale edit must not overwrite newer order"
    );
    assert_eq!(
        settings
            .load_runtime_settings()
            .await
            .unwrap()
            .config_revision,
        revision,
        "display order must not change routing generation"
    );
    let repository = PgClientApiKeyRepository::new(database.pool.clone());
    let mut cursor = None;
    let mut ids = Vec::new();
    loop {
        let page = repository
            .list_client_api_keys(ClientApiKeyListQuery {
                cursor,
                page_size: 1,
                search: None,
                sort: ClientApiKeySort {
                    field: ClientApiKeySortField::Manual,
                    direction: ClientApiKeySortDirection::Asc,
                },
            })
            .await
            .unwrap();
        ids.extend(page.items.into_iter().map(|item| item.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(ids, ["c", "b", "a", "d"]);
    let prior = settings.load_runtime_settings().await.unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "location-test".to_owned(),
    };
    let location = gateway_core::account::RequestLocation::default();
    assert_eq!(location.timezone.to_string(), "America/Los_Angeles");
    settings
        .update_location(&context, true, location.clone())
        .await
        .unwrap();
    let saved = bundle
        .admin_ports()
        .settings()
        .load_runtime_settings()
        .await
        .unwrap();
    assert!(saved.request_location_enabled);
    assert_eq!(saved.request_location, location);
    assert_eq!(
        saved.max_concurrent_per_account,
        prior.max_concurrent_per_account
    );
    settings
        .update_location(&context, false, location.clone())
        .await
        .unwrap();
    let saved = settings.load_runtime_settings().await.unwrap();
    assert!(!saved.request_location_enabled);
    assert_eq!(
        saved.request_location, location,
        "disabling preserves the saved place"
    );
    drop(settings);
    drop(bundle);
    database.close().await;
}

#[tokio::test]
async fn concurrent_price_edits_preserve_other_models_and_sync_preserves_manual_prices() {
    let Some((database, bundle)) = pricing_store("pricing").await else {
        return;
    };
    let settings = bundle.admin_ports().settings();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "pricing-test".to_owned(),
    };
    let pricing: ModelPriceOverride = serde_json::from_value(json!({
        "multiplierBps":12500,
        "bands":{"standard":{"input":"2","output":"10","cacheRead":"0","cacheWrite":"1"}}
    }))
    .unwrap();
    let update = |model: &str, change| UpdatePricing {
        provider: "openai".to_owned(),
        models: vec![model.to_owned()],
        change,
    };
    let (first, second) = tokio::join!(
        settings.update_pricing(
            update("model-a", PricingChange::Replace(pricing.clone())),
            &context
        ),
        settings.update_pricing(
            update("model-b", PricingChange::Replace(pricing.clone())),
            &context
        ),
    );
    assert_ne!(first.unwrap(), second.unwrap());
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(stored.overrides["openai"].len(), 2);

    let mut source = stored.overrides.clone();
    source
        .get_mut("openai")
        .unwrap()
        .get_mut("model-a")
        .unwrap()
        .bands
        .get_mut("standard")
        .unwrap()
        .input = "9".to_owned().try_into().unwrap();
    settings
        .sync_pricing(
            source
                .iter()
                .map(|(provider, models)| {
                    (
                        provider.clone(),
                        models
                            .iter()
                            .map(|(model, price)| (model.clone(), Some(price.clone())))
                            .collect(),
                    )
                })
                .collect(),
            &context,
        )
        .await
        .unwrap();
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(stored.overrides["openai"]["model-a"], pricing);
    assert!(stored.synced_at.is_some());
    for _ in 0..2 {
        settings
            .update_pricing(
                update("model-a", PricingChange::Multiplier(20000)),
                &context,
            )
            .await
            .unwrap();
    }
    let snapshot = PgRuntimeSnapshotRepository::new(database.pool.clone());
    let frozen = snapshot.load_runtime_snapshot().await.unwrap();
    assert_eq!(
        frozen.settings.pricing["openai"]["model-a"].multiplier_bps,
        20000
    );
    assert_eq!(
        frozen.settings.pricing["openai"]["model-a"].bands,
        pricing.bands
    );

    settings
        .update_pricing(update("model-a", PricingChange::Reset), &context)
        .await
        .unwrap();
    let restored = snapshot.load_runtime_snapshot().await.unwrap();
    assert_eq!(
        restored.settings.pricing["openai"]["model-a"],
        source["openai"]["model-a"]
    );
    assert_eq!(restored.settings.pricing["openai"]["model-b"], pricing);
    assert_eq!(
        frozen.settings.pricing["openai"]["model-a"].multiplier_bps,
        20000
    );
    let audits: i64 = sqlx::query_scalar("select count(*) from admin_audit_events where action in ('pricing.update', 'pricing.sync')")
        .fetch_one(&database.pool).await.unwrap();
    assert_eq!(audits, 6);
    drop(settings);
    drop(bundle);
    database.close().await;
}

#[tokio::test]
async fn deleting_pricing_removes_both_layers_and_preserves_other_models_and_frozen_snapshots() {
    let Some((database, bundle)) = pricing_store("pricing_delete").await else {
        return;
    };
    let settings = bundle.admin_ports().settings();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "pricing-delete-test".to_owned(),
    };
    let value = json!({
        "multiplierBps":10000,
        "bands":{"standard":{"input":"2","output":"10","cacheRead":"0","cacheWrite":"1"}}
    });
    settings
        .sync_pricing(
            serde_json::from_value(json!({
                "openai":{"shared":value,"synced-only":value,"untouched":value},
                "xai":{"shared":value}
            }))
            .unwrap(),
            &context,
        )
        .await
        .unwrap();
    settings
        .update_pricing(
            UpdatePricing {
                provider: "openai".to_owned(),
                models: vec![
                    "shared".to_owned(),
                    "custom-only".to_owned(),
                    "untouched".to_owned(),
                ],
                change: PricingChange::Replace(serde_json::from_value(value.clone()).unwrap()),
            },
            &context,
        )
        .await
        .unwrap();
    let repository = PgRuntimeSnapshotRepository::new(database.pool.clone());
    let frozen = repository.load_runtime_snapshot().await.unwrap();
    let synced_at = settings.load_pricing().await.unwrap().synced_at;
    settings
        .update_pricing(
            UpdatePricing {
                provider: "openai".to_owned(),
                models: vec![
                    "shared".to_owned(),
                    "synced-only".to_owned(),
                    "custom-only".to_owned(),
                ],
                change: PricingChange::Delete,
            },
            &context,
        )
        .await
        .unwrap();
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(
        serde_json::to_value(&stored.overrides).unwrap(),
        json!({"openai":{"untouched":value}})
    );
    assert_eq!(
        serde_json::to_value(&stored.synced).unwrap(),
        json!({"openai":{"untouched":value},"xai":{"shared":value}})
    );
    assert_eq!(stored.synced_at, synced_at);
    let current = repository.load_runtime_snapshot().await.unwrap();
    assert_eq!(current.settings.pricing["openai"].len(), 1);
    assert!(current.settings.pricing["xai"].contains_key("shared"));
    assert_eq!(frozen.settings.pricing["openai"].len(), 4);
    let audits: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where action = 'pricing.update'",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audits, 2);
    drop(settings);
    drop(bundle);
    database.close().await;
}

#[tokio::test]
async fn concurrent_pricing_syncs_merge_selected_changes_and_remove_only_selected_models() {
    let Some((database, bundle)) = pricing_store("pricing_selection").await else {
        return;
    };
    let settings = bundle.admin_ports().settings();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "pricing-selection-test".to_owned(),
    };
    let price = json!({
        "multiplierBps":10000,
        "bands":{"standard":{"input":"2","output":"10","cacheRead":"0","cacheWrite":"1"}}
    });
    let patch = |value| {
        serde_json::from_value::<gateway_admin::model::pricing::PricingSyncChanges>(value).unwrap()
    };
    let (first, second) = tokio::join!(
        settings.sync_pricing(patch(json!({"openai": {"model-a": price}})), &context),
        settings.sync_pricing(
            patch(json!({"openai": {"model-b": price}, "xai": {"model-a": price}})),
            &context
        ),
    );
    assert_ne!(first.unwrap(), second.unwrap());
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(
        serde_json::to_value(&stored.synced).unwrap(),
        json!({
            "openai": {"model-a": price, "model-b": price}, "xai": {"model-a": price}
        })
    );
    settings
        .sync_pricing(patch(json!({"openai": {"model-a": null}})), &context)
        .await
        .unwrap();
    let stored = settings.load_pricing().await.unwrap();
    assert_eq!(
        serde_json::to_value(&stored.synced).unwrap(),
        json!({
            "openai": {"model-b": price}, "xai": {"model-a": price}
        })
    );
    let before = stored;
    assert!(
        settings
            .sync_pricing(
                patch(json!({
                    "openai": {"model-b": null}, "invalid": {"model-c": price}
                })),
                &context
            )
            .await
            .is_err()
    );
    assert_eq!(settings.load_pricing().await.unwrap(), before);
    let audits: i64 =
        sqlx::query_scalar("select count(*) from admin_audit_events where action = 'pricing.sync'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(audits, 3);
    drop(settings);
    drop(bundle);
    database.close().await;
}
