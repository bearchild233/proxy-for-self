use super::*;

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
