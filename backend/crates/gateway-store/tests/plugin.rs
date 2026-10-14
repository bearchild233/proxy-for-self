use gateway_core::{
    account::ProviderAccountId,
    provider_ports::{AccountGroupMove, AccountGroupMoveOutcome},
};
use serde_json::json;

use crate::postgres::TestDatabase;

#[tokio::test]
async fn directory_matches_admin_status_and_reads_current_group_membership() {
    // 取得 Bundle 会占用网关实例锁，需与同进程其他 Bundle 测试串行。
    let _instance = crate::support::gateway_instance_guard().await;
    let Some(redis_url) = crate::support::test_env("CPR_TEST_REDIS_URL") else {
        return;
    };
    let Some(database) = TestDatabase::create("plugin_directory").await else {
        return;
    };
    let mut database_url =
        url::Url::parse(&crate::support::test_env("CPR_TEST_DATABASE_URL").unwrap()).unwrap();
    database_url
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", database.schema));
    let connection = |mut url: url::Url| {
        let password = url.password().unwrap().to_owned();
        url.set_password(None).unwrap();
        json!({"url": url.as_str(), "password": password})
    };
    let mut config: gateway_store::StoreConfig = serde_json::from_value(json!({
        "database": connection(database_url),
        "redis": connection(url::Url::parse(&redis_url).unwrap()),
    }))
    .unwrap();
    let runtime = tempfile::tempdir().unwrap();
    config.resolve_and_validate(runtime.path()).unwrap();
    let bundle = gateway_store::initialize(config).await.unwrap();
    for (id, enabled) in [("acct_plugin_a", true), ("acct_plugin_b", false)] {
        sqlx::query("insert into provider_accounts (
            id, provider_kind, name, upstream_user_id, upstream_account_id,
            authentication_kind, provider_credentials_json, credential_revision,
            has_refresh_token, enabled, credential_state, credential_observed_at, created_at, updated_at
          ) values ($1, 'openai', $1, $1, 'fixture-upstream', 'oauth',
            '{\"access_token\":\"must-not-be-returned\"}'::jsonb, 1, false, $2, 'ready', now(), now(), now())")
            .bind(id).bind(enabled).execute(&database.pool).await.unwrap();
    }
    let g = "grp_00000000000000000000000000000001";
    let h = "grp_00000000000000000000000000000002";
    for group in [g, h] {
        sqlx::query(
            "insert into account_groups (id, name, color, created_at, updated_at)
            values ($1, $1, '#2563EBFF', now(), now())",
        )
        .bind(group)
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query("insert into account_group_accounts
            (account_group_id, provider_account_id, created_at) values ($1, 'acct_plugin_a', now())")
            .bind(group).execute(&database.pool).await.unwrap();
    }
    let directory = bundle.plugin_accounts();
    let rows = directory.read(None).await.unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        let runtime = bundle
            .admin_ports()
            .account_runtime()
            .account_runtime(std::slice::from_ref(&row.id))
            .await
            .unwrap();
        let admin = bundle
            .admin_ports()
            .accounts()
            .load_account(&row.id, runtime)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, admin.projection.status.as_str());
        assert_eq!(row.enabled, admin.account.enabled);
        assert_eq!(row.authentication_kind, "oauth");
        assert_eq!(row.upstream_account_id.as_deref(), Some("fixture-upstream"));
        assert_eq!(row.user_id.as_deref(), Some(row.id.as_str()));
    }
    let ids = vec![
        "acct_plugin_a".into(),
        "acct_missing".into(),
        "acct_plugin_a".into(),
    ];
    let rows = directory.read(Some(&ids)).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].groups.len(), 2);
    assert!(rows[0].groups.contains(&g.to_owned()));
    assert!(rows[0].groups.contains(&h.to_owned()));
    let wire = serde_json::to_string(&rows).unwrap();
    assert!(!wire.contains("access_token"));
    assert!(!wire.contains("must-not-be-returned"));
    let request = AccountGroupMove {
        account: ProviderAccountId::new("acct_plugin_a").unwrap(),
        remove_group: Some(g.into()),
        add_group: h.into(),
        reason: "plugin:fixture:move".into(),
    };
    let mover = bundle.provider_ports().group_mover().unwrap();
    assert_eq!(
        mover.move_account_group(&request).await.unwrap(),
        AccountGroupMoveOutcome::Moved
    );
    assert_eq!(
        mover.move_account_group(&request).await.unwrap(),
        AccountGroupMoveOutcome::Unchanged
    );
    assert_eq!(directory.read(Some(&ids)).await.unwrap()[0].groups, vec![h]);
    let actors = sqlx::query_scalar::<_, String>(
        "select actor_ref from admin_audit_events where entity_ref = $1 and action = 'regroup'",
    )
    .bind("acct_plugin_a")
    .fetch_all(&database.pool)
    .await
    .unwrap();
    assert_eq!(actors, vec!["plugin:fixture:move"]);
    drop(directory);
    drop(mover);
    drop(bundle);
    database.close().await;
}
