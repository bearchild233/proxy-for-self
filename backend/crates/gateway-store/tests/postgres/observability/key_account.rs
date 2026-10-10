use super::*;

#[tokio::test]
async fn key_account_breakdown_keeps_all_pairs_and_separates_currencies() {
    let Some(database) = TestDatabase::create("key_account_breakdown").await else {
        return;
    };
    let now = Utc::now();
    // 前两条属于同一组合、不同币种，其余 200 条各自属于不同账号。
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, provider_account_ref, config_revision,
           protocol, operation, endpoint, client_transport, provider_kind,
           upstream_transport, attempt_count, upstream_send_state, requested_model_id,
           outcome, client_status_code, downstream_committed_at, total_tokens,
           cost_source, cost_amount, cost_currency, started_at, deadline_at,
           completed_at, routing_scope
         ) select 'quota_req_' || n, 'quota_key', 'quota_account_' || greatest(n-1,0), 1,
           'openai', 'responses', '/v1/responses', 'http_sse', 'openai', 'http_sse',
           1, 'sent', 'test-model', 'succeeded', 200, $1, 100, 'provider_reported',
           case when n=1 then 7 else 1 end, case when n=1 then 'CNY' else 'USD' end,
           $1, $1 + interval '1 minute', $1, 'all'
           from generate_series(0,201) n",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed all pairs and currencies");
    sqlx::query(
        "insert into model_requests (
           id, client_api_key_ref, provider_account_ref, config_revision,
           protocol, operation, endpoint, client_transport, provider_kind,
           upstream_transport, attempt_count, upstream_send_state, requested_model_id,
           outcome, client_status_code, downstream_committed_at, total_tokens,
           cost_source, cost_amount, cost_currency, started_at, deadline_at,
           completed_at, routing_scope, request_kind
         ) values ('quota_prewarm', 'quota_key', 'quota_account_0', 1, 'openai',
           'responses', '/v1/responses', 'http_sse', 'openai', 'http_sse', 1, 'sent',
           'test-model', 'succeeded', 200, $1, 999, 'provider_reported', 999, 'USD',
           $1, $1 + interval '1 minute', $1, 'all', 'prewarm')",
    )
    .bind(now)
    .execute(&database.pool)
    .await
    .expect("seed excluded prewarm usage");
    let range = ObservabilityRange::new(now - TimeDelta::seconds(1), now + TimeDelta::seconds(1))
        .expect("range");
    let store = admin_observability_store(&database.pool);
    let result = store
        .usage_key_account_breakdown(
            admin_observability::TimeRange::new(range.start, range.end).expect("admin range"),
            admin_observability::UsageFilter::default(),
        )
        .await
        .expect("all key account pairs through admin adapter");
    assert_eq!(result.len(), 201);
    let mixed = result
        .iter()
        .find(|item| item.provider_account_ref == "quota_account_0")
        .expect("mixed pair");
    assert_eq!(mixed.request_count, 3);
    assert_eq!(mixed.total_tokens, 200);
    assert_eq!(mixed.cost_amount, None);
    assert_eq!(mixed.cost_currency, None);
    assert_eq!(
        mixed
            .costs
            .iter()
            .map(|cost| (cost.currency.as_str(), cost.amount.as_str()))
            .collect::<Vec<_>>(),
        vec![("CNY", "7"), ("USD", "1")]
    );
    let single = result
        .iter()
        .find(|item| item.provider_account_ref == "quota_account_200")
        .expect("pair past the old limit");
    assert_eq!(single.cost_currency.as_deref(), Some("USD"));
    assert_eq!(
        single.cost_amount.as_ref().map(|cost| cost.as_str()),
        Some("1")
    );
    database.close().await;
}
