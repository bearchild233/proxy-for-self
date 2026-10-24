use super::*;
use gateway_admin::{
    model::account_groups::{AccountGroupColor, NewAccountGroup},
    ports::store::AccountGroupStore,
};
use gateway_core::account::AccountQuotaProtection;
use gateway_store::postgres::PgAccountGroupRepository;

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
    let stored_default: serde_json::Value =
        sqlx::query_scalar("select quota_protection_json from provider_accounts where id=$1")
            .bind(id.as_str())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        stored_default,
        json!({"enabled":false,"usedPercentLimit":90})
    );
    assert_eq!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        AccountQuotaProtection::default()
    );
    // 真实旧行保留原统一阈值，并把它映射到两个新增窗口。
    sqlx::query("update provider_accounts set quota_protection_json=$2 where id=$1")
        .bind(id.as_str())
        .bind(json!({"enabled":true,"usedPercentLimit":83}))
        .execute(&database.pool)
        .await
        .unwrap();
    assert_eq!(
        repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        AccountQuotaProtection::new(true, 83).unwrap()
    );
    let store = admin_account_store(&database.pool);
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "quota-protection-test".to_owned(),
    };
    let policy = AccountQuotaProtection::new(true, 87)
        .unwrap()
        .with_window_limits(75, 95)
        .unwrap();
    let single = UpdateAccount {
        quota_protection: Some(policy),
        name: None,
        account_id: id.to_string(),
        notes: Some("kept".to_owned()),
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
    let stored: serde_json::Value =
        sqlx::query_scalar("select quota_protection_json from provider_accounts where id=$1")
            .bind(id.as_str())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        json!({
            "enabled":true,"usedPercentLimit":87,
            "shortTermUsedPercentLimit":75,"weeklyUsedPercentLimit":95
        })
    );
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
    assert_eq!(page.items[0].account.notes.as_deref(), Some("kept"));
    store
        .update_account(
            UpdateAccount {
                quota_protection: None,
                notes: Some("updated memo".to_owned()),
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
    let notes: Option<String> =
        sqlx::query_scalar("select notes from provider_accounts where id=$1")
            .bind(id.as_str())
            .fetch_one(&database.pool)
            .await
            .unwrap();
    assert_eq!(notes.as_deref(), Some("updated memo"));
    let disabled = AccountQuotaProtection::new(false, 85)
        .unwrap()
        .with_window_limits(65, 93)
        .unwrap();
    let second_id = ProviderAccountId::new("acct_batch_policy").unwrap();
    repository
        .insert_provider_account(account(second_id.as_str(), "batch-user"))
        .await
        .unwrap();
    store
        .batch_update_accounts(
            BatchUpdateAccounts {
                quota_protection: Some(disabled),
                expiry_priority: None,
                restore_archived: false,
                account_ids: vec![id.to_string(), second_id.to_string()],
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
    assert_eq!(
        repository
            .get_account(&second_id)
            .await
            .unwrap()
            .unwrap()
            .quota_protection(),
        disabled
    );
    let batch_saved = store
        .load_account(id.as_str(), AccountRuntimeSnapshot::default())
        .await
        .unwrap()
        .unwrap()
        .account;
    assert_eq!(batch_saved.quota_protection, disabled);
    assert_eq!(batch_saved.notes.as_deref(), Some("updated memo"));
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

#[tokio::test]
async fn quota_protection_uniform_and_split_policies_keep_their_storage_format_on_update_and_rotation()
 {
    let Some(database) = TestDatabase::create("quota_protection_storage_format").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let store = admin_account_store(&database.pool);
    let id = ProviderAccountId::new("acct_policy_storage").unwrap();
    repository
        .insert_provider_account(account(id.as_str(), "policy-storage-user"))
        .await
        .unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "quota-protection-storage-test".to_owned(),
    };
    let split = AccountQuotaProtection::new(true, 87)
        .unwrap()
        .with_window_limits(70, 95)
        .unwrap();
    // 最后恢复统一阈值，确保已有四字段行也会收敛回兼容旧程序的两字段格式。
    for (index, (policy, expected)) in [
        (
            AccountQuotaProtection::new(true, 78).unwrap(),
            json!({"enabled":true,"usedPercentLimit":78}),
        ),
        (
            split,
            json!({
                "enabled":true,"usedPercentLimit":87,
                "shortTermUsedPercentLimit":70,"weeklyUsedPercentLimit":95
            }),
        ),
        (
            AccountQuotaProtection::new(false, 84).unwrap(),
            json!({"enabled":false,"usedPercentLimit":84}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let settings = UpdateAccount {
            quota_protection: Some(policy),
            name: None,
            account_id: id.to_string(),
            notes: Some("kept".to_owned()),
            enabled: true,
            concurrency_limit: None,
            weight: gateway_core::account::AccountWeight::DEFAULT,
            model_access: None,
            group_ids: vec![],
            outbound_proxy: None,
        };
        store
            .update_account(settings.clone(), &context)
            .await
            .unwrap();
        let after_update: serde_json::Value =
            sqlx::query_scalar("select quota_protection_json from provider_accounts where id=$1")
                .bind(id.as_str())
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(after_update, expected);
        assert_eq!(
            repository
                .get_account(&id)
                .await
                .unwrap()
                .unwrap()
                .quota_protection(),
            policy
        );
        let revision = repository
            .get_account(&id)
            .await
            .unwrap()
            .unwrap()
            .revision();
        repository
            .rotate_provider_account(RotateProviderAccount {
                reactivate_archived: false,
                settings: Some(UpdateAccount {
                    notes: None,
                    ..settings
                }),
                scope: ProviderAccountAdminScope {
                    provider_kind: "openai".to_owned(),
                },
                profile: profile(id.as_str(), "policy storage rotation"),
                replacement_identity: None,
                credential: credential_update(id.as_str(), revision.get(), "storage-test-secret"),
                audit: audit(
                    &format!("audit_quota_storage_{index}"),
                    "rotate",
                    id.as_str(),
                ),
            })
            .await
            .unwrap();
        let after_rotation: serde_json::Value =
            sqlx::query_scalar("select quota_protection_json from provider_accounts where id=$1")
                .bind(id.as_str())
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(after_rotation, expected);
        let loaded = store
            .load_account(id.as_str(), AccountRuntimeSnapshot::default())
            .await
            .unwrap()
            .unwrap()
            .account;
        assert_eq!(loaded.quota_protection, policy);
        assert_eq!(loaded.notes.as_deref(), Some("kept"));
    }
    database.close().await;
}

#[tokio::test]
async fn quota_protection_group_members_use_the_same_legacy_and_split_policy_decoder() {
    let Some(database) = TestDatabase::create("quota_protection_group_members").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let groups = PgAccountGroupRepository::new(database.pool.clone());
    let group_id = AccountGroupId::new("grp_00000000000000000000000000000001").unwrap();
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "quota-protection-members-test".to_owned(),
    };
    groups
        .create_account_group(
            NewAccountGroup {
                disable_fast: false,
                id: group_id.clone(),
                name: "Quota protection members".to_owned(),
                description: None,
                color: AccountGroupColor::parse("#2563EBFF").unwrap(),
            },
            &context,
        )
        .await
        .unwrap();
    let legacy = AccountQuotaProtection::new(true, 81).unwrap();
    let split = AccountQuotaProtection::new(true, 89)
        .unwrap()
        .with_window_limits(70, 96)
        .unwrap();
    for (account_id, wire, expected) in [
        (
            "acct_legacy_policy",
            json!({"enabled":true,"usedPercentLimit":81}),
            legacy,
        ),
        (
            "acct_split_policy",
            serde_json::to_value(split).unwrap(),
            split,
        ),
    ] {
        repository
            .insert_provider_account(account(account_id, account_id))
            .await
            .unwrap();
        sqlx::query("update provider_accounts set quota_protection_json=$2 where id=$1")
            .bind(account_id)
            .bind(wire)
            .execute(&database.pool)
            .await
            .unwrap();
        sqlx::query("insert into account_group_accounts (account_group_id, provider_account_id, created_at) values ($1, $2, now())")
            .bind(group_id.as_str()).bind(account_id).execute(&database.pool).await.unwrap();
        let id = ProviderAccountId::new(account_id).unwrap();
        assert_eq!(
            repository
                .get_account(&id)
                .await
                .unwrap()
                .unwrap()
                .quota_protection(),
            expected
        );
    }
    let members = groups
        .load_account_group_members(std::slice::from_ref(&group_id))
        .await
        .unwrap();
    assert_eq!(members.len(), 2);
    assert!(
        members
            .iter()
            .any(|member| member.account_id.as_str() == "acct_legacy_policy")
    );
    assert!(
        members
            .iter()
            .any(|member| member.account_id.as_str() == "acct_split_policy")
    );

    // 冻结 SQL 仅验证旧字段；所有读入口仍须由共享领域 decoder 拒绝损坏的新字段。
    sqlx::query("update provider_accounts set quota_protection_json=$2 where id=$1")
        .bind("acct_split_policy")
        .bind(json!({"enabled":true,"usedPercentLimit":89,"shortTermUsedPercentLimit":70}))
        .execute(&database.pool)
        .await
        .unwrap();
    let id = ProviderAccountId::new("acct_split_policy").unwrap();
    assert!(repository.get_account(&id).await.is_err());
    assert!(
        groups
            .load_account_group_members(&[group_id])
            .await
            .is_err()
    );
    database.close().await;
}
