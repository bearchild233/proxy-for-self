use super::*;
use gateway_admin::{
    model::provider_credentials::{CredentialMutationResult, PreparedCredentialRotationFacts},
    ports::store::AdminStoreError,
};

#[tokio::test]
async fn lifecycle_cas_archive_filter_and_restore_preserve_base_weight() {
    let Some(database) = TestDatabase::create("account_lifecycle").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    let mut input = account("acct_lifecycle", "user-lifecycle");
    input.plan_type = Some("plus".to_owned());
    let base_weight = input.weight;
    repository.insert_provider_account(input).await.unwrap();
    let id = ProviderAccountId::new("acct_lifecycle").unwrap();
    let store = admin_account_store(&database.pool);
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: "lifecycle".to_owned(),
    };
    let command = BatchUpdateAccounts {
        account_ids: vec![id.to_string()],
        expiry_priority: Some(true),
        restore_archived: false,
        enabled: None,
        concurrency_limit: None,
        weight: None,
        group_ids: None,
        outbound_proxy: None,
        model_access: None,
    };
    let old = repository.get_account(&id).await.unwrap().unwrap();
    store
        .batch_update_accounts(command.clone(), &context)
        .await
        .unwrap();
    assert!(
        !repository
            .compare_and_swap_lifecycle(&old, old.lifecycle().clone())
            .await
            .unwrap()
    );
    let current = repository.get_account(&id).await.unwrap().unwrap();
    let mut facts = current.lifecycle().clone();
    facts.archived = true;
    facts.archive_reason = Some("credential_invalid".to_owned());
    facts.archived_at = Some(Utc::now());
    assert!(
        repository
            .compare_and_swap_lifecycle(&current, facts)
            .await
            .unwrap()
    );
    let archived = repository.get_account(&id).await.unwrap().unwrap();
    assert!(!archived.enabled());
    assert_eq!(archived.weight(), base_weight);
    for (archived, expected) in [(false, 0), (true, 1)] {
        let page = store
            .list_accounts(
                AccountListQuery {
                    archived,
                    page: 1,
                    page_size: PageSize::new(20).unwrap(),
                    provider_kind: None,
                    group_filter: None,
                    search: None,
                    status: None,
                    sort: None,
                },
                Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(page.total, expected);
    }
    store
        .batch_update_accounts(
            BatchUpdateAccounts {
                expiry_priority: None,
                restore_archived: true,
                ..command
            },
            &context,
        )
        .await
        .unwrap();
    let restored = repository.get_account(&id).await.unwrap().unwrap();
    assert!(!restored.lifecycle().archived);
    assert!(restored.lifecycle().expiry_priority);
    assert!(!restored.enabled());
    assert_eq!(restored.weight(), base_weight);
    // 恢复前发出的旧探测不能再次归档。
    assert!(
        !repository
            .compare_and_swap_lifecycle(&current, archived.lifecycle().clone())
            .await
            .unwrap()
    );
    database.close().await;
}

/// 用管理端 Store 提交一次 OAuth 重新授权，返回提交结果。
async fn reauthorize_account(
    database: &TestDatabase,
    account_id: &str,
    expected_revision: u64,
    request_id: &str,
) -> Result<CredentialMutationResult, AdminStoreError> {
    let context = MutationContext {
        actor: MutationActor::System,
        request_id: request_id.to_owned(),
    };
    let provider_kind = ProviderKind::new("openai").unwrap();
    let id = ProviderAccountId::new(account_id).unwrap();
    admin_account_store(&database.pool)
        .commit_authorization(
            AuthorizationCommit {
                settings: None,
                pending: PendingAuthorizationMutation::new(
                    provider_kind.clone(),
                    AuthorizationMutationTarget::Reauthorize {
                        account_id: id.clone(),
                    },
                    AuthorizationOwnerBinding::from_context(&context),
                ),
                credential: AuthorizationCredentialCommit::Reauthorize(
                    PreparedCredentialRotationFacts {
                        account_id: id,
                        provider_kind,
                        expected_credential_revision: gateway_admin::model::Revision::new(
                            expected_revision,
                        )
                        .unwrap(),
                        replacement_identity: None,
                        name: account_id.to_owned(),
                        email: Some(format!("{account_id}@example.invalid")),
                        plan_type: Some("plus".to_owned()),
                        preserve_profile: false,
                        provider_material: ProviderDocument::new(OpaqueProviderData::new(
                            [("access_token".to_owned(), json!("reauthorized-secret"))]
                                .into_iter()
                                .collect(),
                        )),
                        has_refresh_token: true,
                        access_token_expires_at: Some(Utc::now() + TimeDelta::hours(2)),
                        next_refresh_at: None,
                    },
                ),
            },
            &context,
        )
        .await
}

#[derive(sqlx::FromRow)]
struct ReauthorizedRow {
    enabled: bool,
    credential_state: String,
    credential_revision: i64,
    last_error_reason: Option<String>,
    last_error_message: Option<String>,
    archived: Option<String>,
    archive_reason: Option<String>,
    archived_at: Option<String>,
}

async fn reauthorized_row(database: &TestDatabase, id: &str) -> ReauthorizedRow {
    sqlx::query_as(
        "select enabled, credential_state, credential_revision, last_error_reason, last_error_message,
                lifecycle_json->>'archived' as archived,
                lifecycle_json->>'archiveReason' as archive_reason,
                lifecycle_json->>'archivedAt' as archived_at
         from provider_accounts where id = $1",
    )
    .bind(id)
    .fetch_one(&database.pool)
    .await
    .unwrap()
}

async fn audit_fields(database: &TestDatabase, request_id: &str) -> Vec<Vec<String>> {
    sqlx::query_scalar(
        "select changed_fields from admin_audit_events
         where admin_request_id = $1 order by created_at",
    )
    .bind(request_id)
    .fetch_all(&database.pool)
    .await
    .unwrap()
}

/// 归档账号 OAuth 重新授权成功：同一事务内离开归档、启用、重置凭据状态并写一条审计；
/// 未归档的手动停用账号重新授权不被启用；凭据 CAS 失败时归档状态整体保持。
#[tokio::test]
async fn oauth_reauthorization_of_an_archived_account_leaves_archive_and_enables_atomically() {
    let Some(database) = TestDatabase::create("account_reauth_archive").await else {
        return;
    };
    let repository = PgProviderAccountRepository::new(database.pool.clone());
    for (id, user) in [
        ("acct_reauth_archived", "user-reauth-archived"),
        ("acct_reauth_plain", "user-reauth-plain"),
        ("acct_reauth_stale", "user-reauth-stale"),
    ] {
        repository
            .insert_provider_account(account(id, user))
            .await
            .unwrap();
    }
    // 两个账号被明确判定凭据失效并归档；另一个只是管理员手动停用。
    for id in ["acct_reauth_archived", "acct_reauth_stale"] {
        sqlx::query(
            "update provider_accounts
             set enabled = false, credential_state = 'invalid',
                 last_error_reason = 'credential_invalid', last_error_message = 'refresh rejected',
                 lifecycle_json = jsonb_build_object(
                   'archived', true, 'archiveReason', 'credential_invalid',
                   'archivedAt', '2026-10-01T00:00:00Z',
                   'subscriptionExpiresAt', '2026-10-20T00:00:00Z'),
                 updated_at = now()
             where id = $1",
        )
        .bind(id)
        .execute(&database.pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "update provider_accounts set enabled = false, updated_at = now()
         where id = 'acct_reauth_plain'",
    )
    .execute(&database.pool)
    .await
    .unwrap();

    // 失败路径先做：旧版本号使凭据 CAS 失败，归档与停用必须原样保留，配置版本不变。
    let revision_before = current_revision(&database.pool).await;
    reauthorize_account(&database, "acct_reauth_stale", 99, "reauth_stale")
        .await
        .expect_err("stale credential revision must fail");
    let stale = reauthorized_row(&database, "acct_reauth_stale").await;
    assert!(!stale.enabled);
    assert_eq!(stale.archived.as_deref(), Some("true"));
    assert_eq!(stale.credential_state, "invalid");
    assert_eq!(current_revision(&database.pool).await, revision_before);
    assert!(audit_fields(&database, "reauth_stale").await.is_empty());

    let result = reauthorize_account(&database, "acct_reauth_archived", 1, "reauth_archived")
        .await
        .expect("archived account reauthorizes");
    assert_eq!(
        result.credential_revision.map(|revision| revision.get()),
        Some(2)
    );
    let archived = reauthorized_row(&database, "acct_reauth_archived").await;
    assert!(archived.enabled);
    assert_eq!(archived.archived.as_deref(), Some("false"));
    assert_eq!(archived.archive_reason, None);
    // 保留上次归档时间，与“恢复到主列表”一致。
    assert_eq!(
        archived.archived_at.as_deref(),
        Some("2026-10-01T00:00:00Z")
    );
    assert_eq!(archived.credential_state, "ready");
    assert_eq!(archived.credential_revision, 2);
    assert_eq!(archived.last_error_reason, None);
    assert_eq!(archived.last_error_message, None);
    assert_eq!(current_revision(&database.pool).await, revision_before + 1);
    let fields = audit_fields(&database, "reauth_archived").await;
    assert_eq!(fields.len(), 1);
    for expected in ["credentials", "enabled", "restore_archived"] {
        assert!(
            fields[0].iter().any(|field| field == expected),
            "{fields:?}"
        );
    }

    reauthorize_account(&database, "acct_reauth_plain", 1, "reauth_plain")
        .await
        .expect("plain account reauthorizes");
    let plain = reauthorized_row(&database, "acct_reauth_plain").await;
    assert!(!plain.enabled, "manual disable survives reauthorization");
    assert_eq!(
        audit_fields(&database, "reauth_plain").await,
        vec![vec!["credentials".to_owned()]]
    );

    let store = admin_account_store(&database.pool);
    for (archived, expected) in [(true, 1), (false, 2)] {
        let page = store
            .list_accounts(
                AccountListQuery {
                    archived,
                    page: 1,
                    page_size: PageSize::new(20).unwrap(),
                    provider_kind: None,
                    group_filter: None,
                    search: None,
                    status: None,
                    sort: None,
                },
                Default::default(),
            )
            .await
            .unwrap();
        assert_eq!(page.total, expected, "archived={archived}");
    }
    database.close().await;
}
