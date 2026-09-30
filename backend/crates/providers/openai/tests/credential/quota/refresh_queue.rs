//! 通过额度服务验证合并、取消和失败重试，不公开内部队列来迁就测试。
use super::*;
use std::time::Duration;

#[tokio::test]
async fn manual_refresh_keeps_capacity_while_background_sync_is_busy() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_a_background").await;
    create_account(&store, "acct_b_manual").await;
    let server = MockServer::start().await;
    Mock::given(path("/api/codex/usage"))
        .and(header("chatgpt-account-id", "chatgpt-acct_a_background"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(800))
                .set_body_json(json!({"rate_limit":{"allowed":true}})),
        )
        .mount(&server)
        .await;
    Mock::given(path("/api/codex/usage"))
        .and(header("chatgpt-account-id", "chatgpt-acct_b_manual"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"rate_limit":{"allowed":true}})),
        )
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
    );
    let manual = async {
        tokio::time::timeout(Duration::from_secs(2), async {
            while server.received_requests().await.unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let account = store.account("acct_b_manual").unwrap();
        tokio::time::timeout(
            Duration::from_millis(400),
            service.refresh_account(account.id()),
        )
        .await
        .unwrap()
        .unwrap();
    };
    let (sync, ()) = tokio::join!(service.synchronize(), manual);
    sync.unwrap();
}

#[tokio::test]
async fn overlapping_refreshes_share_upstream_but_later_manual_refresh_is_fresh() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_queue_account").await;
    let account = store.account("acct_queue_account").unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(80))
                .set_body_json(json!({"rate_limit":{"allowed":true}})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
    );
    let (a, b) = tokio::join!(
        service.refresh_account(account.id()),
        service.refresh_account(account.id())
    );
    assert!(a.is_ok() && b.is_ok());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    service.refresh_account(account.id()).await.unwrap();
}

#[tokio::test]
async fn cancelling_refresh_does_not_block_followers() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_cancel_account").await;
    let account = store.account("acct_cancel_account").unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(json!({"rate_limit":{"allowed":true}})),
        )
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            service.refresh_account(account.id())
        )
        .await
        .is_err()
    );
    tokio::time::timeout(
        Duration::from_secs(2),
        service.refresh_account(account.id()),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn overlapping_failures_are_shared_and_later_refresh_retries() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_failure_account").await;
    let account = store.account("acct_failure_account").unwrap();
    let server = MockServer::start().await;
    Mock::given(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(400).set_delay(Duration::from_millis(80)))
        .mount(&server)
        .await;
    let service = quota_service_with_base_url(
        &store,
        reqwest::Client::builder().no_proxy().build().unwrap(),
        server.uri(),
    );
    let (a, b) = tokio::join!(
        service.refresh_account(account.id()),
        service.refresh_account(account.id())
    );
    assert!(a.is_err() && b.is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    server.reset().await;
    Mock::given(path("/api/codex/usage"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"rate_limit":{"allowed":true}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    service.refresh_account(account.id()).await.unwrap();
}
