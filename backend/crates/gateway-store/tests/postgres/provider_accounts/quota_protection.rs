use super::*;
use gateway_core::account::AccountQuotaProtection;

#[tokio::test]
async fn quota_protection_round_trips_and_survives_unrelated_updates() {
    let Some(database) = TestDatabase::create("quota_protection").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let id = ProviderAccountId::new("acct_protected").unwrap();
    let input = account(id.as_str(), "protected-user");
    repository
        .insert_provider_account(input.clone())
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        AccountQuotaProtection::default()
    );
    let store = admin_account_store(&database.pool);
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "quota-protection-test".to_owned(),
    };
    let policy = AccountQuotaProtection::new(true, 90).unwrap();
    let single = UpdateAccount {
        quota_protection: Some(policy),
        name: None,
        account_id: id.to_string(),
        notes: None,
        enabled: true,
        concurrency_limit: None,
        weight: gateway_core::account::AccountWeight::DEFAULT,
        model_access: None,
        group_ids: vec![],
        outbound_proxy: None,
    };
    store
        .update_account(single.clone(), &context)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        policy
    );
    assert_eq!(
        store
            .load_account(id.as_str(), AccountRuntimeSnapshot::default())
            .await
            .unwrap()
            .unwrap()
            .account
            .quota_protection,
        policy
    );
    let page = store
        .list_accounts(
            AccountListQuery {
                archived: false,
                page: 1,
                page_size: PageSize::new(20).unwrap(),
                provider_kind: None,
                group_filter: None,
                search: None,
                status: None,
                sort: None,
            },
            AccountRuntimeSnapshot::default(),
        )
        .await
        .unwrap();
    assert_eq!(page.items[0].account.quota_protection, policy);
    store
        .update_account(
            UpdateAccount {
                quota_protection: None,
                notes: Some("kept".to_owned()),
                ..single
            },
            &context,
        )
        .await
        .unwrap();
    repository
        .import_provider_accounts(ImportProviderAccounts {
            scope: ProviderAccountAdminScope {
                provider_kind: "openai".to_owned(),
            },
            accounts: vec![input],
            settings: None,
            outbound_proxy: None,
            audit: audit("audit_quota_protection", "import", id.as_str()),
        })
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        policy
    );
    let disabled = AccountQuotaProtection::new(false, 85).unwrap();
    store
        .batch_update_accounts(
            BatchUpdateAccounts {
                quota_protection: Some(disabled),
                expiry_priority: None,
                restore_archived: false,
                account_ids: vec![id.to_string()],
                enabled: None,
                concurrency_limit: None,
                weight: None,
                model_access: None,
                group_ids: None,
                outbound_proxy: None,
            },
            &context,
        )
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        disabled
    );
    let audited: i64 = sqlx::query_scalar(
        "select count(*) from admin_audit_events where 'quota_protection'=any(changed_fields)",
    )
    .fetch_one(&database.pool)
    .await
    .unwrap();
    assert_eq!(audited, 2);
    for bad in [
        json!({"enabled":true,"usedPercentLimit":0}),
        json!({"enabled":true,"usedPercentLimit":100}),
        json!({"enabled":true,"usedPercentLimit":90.5}),
        json!({"enabled":true}),
    ] {
        assert!(
            sqlx::query("update provider_accounts set quota_protection_json=$2 where id=$1")
                .bind(id.as_str())
                .bind(bad)
                .execute(&database.pool)
                .await
                .is_err()
        );
    }
    database.close().await;
}
