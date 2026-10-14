//! `route.prepare`、`response.head`、`request.finish` 的行为测试：由旧网关测试
//! `pool_29_cancels_before_delivery_and_replays_original_input_on_same_account`、
//! `pool_skips_account_with_confirmed_exhausted_quota`、`pool_fetches_model_ticket_within_shared_window`、
//! `pool_only_key_reaches_routing_and_reports_the_reason`、`pool_bootstrap_*_retries_once_*`
//! 等移植而来；断言与旧测试一致，访问内核与池服务的部分改为核对模拟内核记录的能力调用。

mod common;
mod hooks_support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use common::object_of;
use gateway_pool::dispatch;
use hooks_support::{
    ACCOUNT, FakeAccount, POOL_TOKEN, Rig, cookie_for, gateway_of, now_secs, probe_result,
};
use plugin_sdk::protocol::*;
use serde_json::{Value, json};

fn used(result: RoutePrepareResult) -> (String, Upstream) {
    match result {
        RoutePrepareResult::Use {
            lease_id, upstream, ..
        } => (lease_id, upstream),
        other => panic!("expected use, got {other:?}"),
    }
}

fn failed(result: RoutePrepareResult) -> Reason {
    match result {
        RoutePrepareResult::Fail { reason } => reason,
        other => panic!("expected fail, got {other:?}"),
    }
}

/// 选号后取得一个租约（窗口 + 材料 + 票）。
async fn lease(rig: &Rig, request: &str, model: &str) -> (String, Upstream) {
    rig.select(request, &[ACCOUNT], false, None).await;
    used(rig.drive(request, ACCOUNT, model, 10).await)
}

fn gateway(upstream: &Upstream) -> String {
    gateway_of(upstream.cookie.as_deref().expect("cookie")).expect("gateway")
}

/// 打票成功后业务请求带上插件给的 Cookie 与 state；29 且网关一致时换网关重发，新窗口在另一个网关上。
#[tokio::test]
async fn switch_after_29_moves_to_another_gateway_and_finishes_both_leases() {
    let rig = Rig::start("route-29", json!({})).await;
    let (first, upstream) = lease(&rig, "req_29", "gpt-6-astra").await;
    assert_eq!(gateway(&upstream), "unified-142");
    assert_eq!(upstream.turn_state.as_deref(), Some("state-unified-142"));
    assert_eq!(rig.pair(ACCOUNT, "unified-142")["state"], "ready");

    // 同一个 s0 返回了新的 state：降智，换网关。
    let verdict = rig.head("req_29", &first, Some(200), true, vec![]).await;
    assert_eq!(verdict, ResponseHeadResult::Switch);
    // 29 先退役组合并立即归还租约：组合进入静置。
    assert_eq!(rig.lease_count(&first), 0);
    assert_eq!(rig.pair(ACCOUNT, "unified-142")["state"], "cooling");

    let (second, upstream) = used(rig.drive("req_29", ACCOUNT, "gpt-6-astra", 10).await);
    assert_eq!(gateway(&upstream), "unified-143");
    assert_eq!(upstream.turn_state.as_deref(), Some("state-unified-143"));
    assert_eq!(
        rig.head("req_29", &second, Some(200), false, vec![]).await,
        ResponseHeadResult::Accept
    );
    rig.finish("req_29", &second, RequestOutcome::Completed)
        .await;
    let business: Vec<Value> = rig
        .observations()
        .into_iter()
        .filter(|item| item["source"] == "business")
        .collect();
    assert_eq!(business.len(), 2);
    assert_eq!(business[0]["classification"], "29");
    assert_eq!(business[1]["classification"], "21");
    // 先前的 29 窗口不会再被选中。
    assert_eq!(rig.pair(ACCOUNT, "unified-143")["state"], "ready");
}

/// 额度确认耗尽的账号不会被检测：没有任何上游探测，原因码是 quota_exhausted。
#[tokio::test]
async fn account_with_confirmed_exhausted_quota_is_skipped() {
    let rig = Rig::start("route-exhausted", json!({})).await;
    rig.fake.quota_mut(ACCOUNT, |quota| quota.exhausted = true);
    rig.select("req_q", &[ACCOUNT], false, None).await;
    let reason = failed(rig.drive("req_q", ACCOUNT, "gpt-6-astra", 5).await);
    assert_eq!(reason.code, "quota_exhausted");
    assert!(rig.fake.calls_to("upstream.probe").is_empty());
    // 额度事实同步给了运行状态机，队列里账号标为耗尽。
    let queue = rig.query("SELECT quota_exhausted FROM gp_account_queue");
    assert_eq!(queue, vec![vec![json!(1)]]);
}

/// 账号不满足网关池准入条件时给出具体原因。
#[tokio::test]
async fn ineligible_accounts_report_specific_reasons() {
    let rig = Rig::start("route-ineligible", json!({})).await;
    rig.select("req_i", &[ACCOUNT], false, None).await;
    for (change, code) in [
        (FakeAccountChange::Disabled, "account_disabled"),
        (FakeAccountChange::Kind("api_key"), "not_oauth"),
        (FakeAccountChange::Status("rate_limited"), "rate_limited"),
        (
            FakeAccountChange::Status("quota_exhausted"),
            "quota_exhausted",
        ),
        (FakeAccountChange::Status("error"), "account_error"),
        (FakeAccountChange::NoQuota, "quota_unreadable"),
    ] {
        rig.fake.add_account(ACCOUNT, FakeAccount::normal());
        change.apply(&rig);
        let reason = failed(rig.drive("req_i", ACCOUNT, "gpt-6-astra", 5).await);
        assert_eq!(reason.code, code, "{}", reason.message);
    }
    // 账号被删除：内核不再返回它。
    rig.fake.accounts.lock().expect("accounts").clear();
    let reason = failed(rig.drive("req_i", ACCOUNT, "gpt-6-astra", 5).await);
    assert_eq!(reason.code, "account_missing");
    assert!(rig.fake.calls_to("upstream.probe").is_empty());
}

enum FakeAccountChange {
    Disabled,
    Kind(&'static str),
    Status(&'static str),
    NoQuota,
}
impl FakeAccountChange {
    fn apply(&self, rig: &Rig) {
        match self {
            Self::Disabled => rig.fake.account_mut(ACCOUNT, |a| a.enabled = false),
            Self::Kind(kind) => rig.fake.account_mut(ACCOUNT, |a| a.kind = (*kind).into()),
            Self::Status(status) => rig
                .fake
                .account_mut(ACCOUNT, |a| a.status = (*status).into()),
            Self::NoQuota => rig.fake.quota_mut(ACCOUNT, |q| q.present = false),
        }
    }
}

/// 缺上游账号或用户标识且设置里没有指定映射时不能生成池成员标识；指定映射后可用。
#[tokio::test]
async fn member_identity_comes_from_the_mapping_or_upstream_ids() {
    let rig = Rig::start("route-member", json!({})).await;
    rig.fake.account_mut(ACCOUNT, |a| a.user = None);
    let reason = failed(rig.drive("req_m", ACCOUNT, "gpt-6-astra", 5).await);
    assert_eq!(reason.code, "member_unknown");
    assert!(reason.message.contains("池成员标识"));

    let rig = Rig::start(
        "route-member-map",
        json!({"poolAccountIds": {ACCOUNT: "demo"}}),
    )
    .await;
    rig.fake.account_mut(ACCOUNT, |a| a.user = None);
    used(rig.drive("req_m", ACCOUNT, "gpt-6-astra", 10).await);
    let fetches = rig.fake.fetches("/gateways");
    assert_eq!(
        fetches[0]["url"],
        "https://pool.example.invalid/gateways?account=demo"
    );
}

/// 打票模型为 sol 时，Astra 请求在同一窗口内先用 Astra 发 `hi` 只拿 state，不做第二轮判定；
/// 业务请求带 Astra 自己的票，不使用 sol 的 S0。
#[tokio::test]
async fn model_ticket_is_fetched_within_the_shared_window() {
    let rig = Rig::start("route-ticket", json!({"probeModel": "gpt-6.1-sol"})).await;
    rig.seed_ready(ACCOUNT, "unified-142", "first");
    rig.fake.set_probe(|params| {
        assert_eq!(params["model"], "gpt-6-astra");
        assert!(params["turnState"].is_null(), "拿票不带 state");
        probe_result(Some(200), Some("astra-ticket"))
    });
    let (lease, upstream) = lease(&rig, "req_t", "gpt-6-astra").await;
    assert_eq!(upstream.turn_state.as_deref(), Some("astra-ticket"));
    assert_ne!(
        upstream.turn_state.as_deref(),
        Some("state-first"),
        "sol 的 S0 不能用于 Astra"
    );
    assert_eq!(gateway(&upstream), "unified-142");
    // 拿票只发一次探测，带的是窗口的 Cookie（不是票）。
    let probes = rig.fake.calls_to("upstream.probe");
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0]["includeAccountCookies"], false);
    assert_eq!(probes[0]["timeoutMs"], 15000);
    assert_eq!(
        gateway_of(probes[0]["cookie"].as_str().expect("cookie")).as_deref(),
        Some("unified-142")
    );

    assert_eq!(
        rig.head("req_t", &lease, Some(200), false, vec![]).await,
        ResponseHeadResult::Accept
    );
    rig.finish("req_t", &lease, RequestOutcome::Completed).await;

    let observations = rig.observations();
    let ticket = observations
        .iter()
        .find(|item| item["source"] == "ticket")
        .expect("ticket recorded");
    assert_eq!(ticket["model"], "gpt-6-astra");
    let business = observations
        .iter()
        .find(|item| item["source"] == "business")
        .expect("business recorded");
    assert_eq!(business["model"], "gpt-6-astra");
    let stored = rig.query("SELECT model, data FROM model_tickets");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0][0], "gpt-6-astra");
    let material: Value = serde_json::from_str(stored[0][1].as_str().expect("data")).expect("json");
    assert_eq!(material["state"], "astra-ticket");
    // 拿票的访问与业务访问是两次独立记录：前者已结束，后者以请求终态结束。
    let accesses =
        rig.query("SELECT kind, outcome, ended, elapsed FROM gp_accesses ORDER BY observed, kind");
    let kinds: Vec<&str> = accesses.iter().filter_map(|row| row[0].as_str()).collect();
    assert!(
        kinds.contains(&"ticket") && kinds.contains(&"business"),
        "{kinds:?}"
    );
    let business_access = accesses
        .iter()
        .find(|row| row[0] == "business")
        .expect("business");
    assert_eq!(business_access[1], "completed");
    assert!(business_access[2].as_f64().expect("ended") > 0.0);
    let ticket_access = accesses
        .iter()
        .find(|row| row[0] == "ticket")
        .expect("ticket");
    assert_eq!(ticket_access[1], "headers");
    // 拿票只是材料获取，不单独判定，也不触发找票。
    assert!(!observations.iter().any(|item| item["source"] == "probe"));
    assert_eq!(
        rig.query("SELECT COUNT(*) FROM leases WHERE kind='probe'")[0][0],
        0
    );
}

/// 拿票比钩子预算慢时窗口预约保留在请求状态里，下一次调用继续并复用同一个租约。
#[tokio::test]
async fn slow_model_ticket_keeps_the_reservation_between_calls() {
    let rig = Rig::start("route-slow-ticket", json!({"probeModel": "gpt-6.1-sol"})).await;
    rig.seed_ready(ACCOUNT, "unified-142", "first");
    rig.fake.set_probe(|_| {
        std::thread::sleep(Duration::from_millis(1800));
        probe_result(Some(200), Some("slow-ticket"))
    });
    rig.select("req_s", &[ACCOUNT], false, None).await;
    let deadline = (now_secs() * 1000.0) as u64 + 30_000;
    let first = rig
        .prepare_once("req_s", ACCOUNT, "gpt-6-astra", deadline)
        .await;
    let RoutePrepareResult::Wait { reason, .. } = first else {
        panic!("expected wait, got {first:?}");
    };
    assert!(reason.message.contains("gpt-6-astra"), "{}", reason.message);
    let held = rig.query("SELECT id FROM leases WHERE kind='business'");
    assert_eq!(held.len(), 1);
    let (lease, upstream) = used(rig.drive("req_s", ACCOUNT, "gpt-6-astra", 10).await);
    assert_eq!(json!(lease), held[0][0]);
    assert_eq!(upstream.turn_state.as_deref(), Some("slow-ticket"));
    assert_eq!(
        rig.query("SELECT COUNT(*) FROM leases WHERE kind='business'")[0][0],
        1
    );
    assert_eq!(rig.fake.calls_to("upstream.probe").len(), 1);
}

/// 拿不到票时归还窗口预约并以 ticket_unavailable 失败。
#[tokio::test]
async fn failed_model_ticket_releases_the_reservation() {
    let rig = Rig::start("route-ticket-fail", json!({"probeModel": "gpt-6.1-sol"})).await;
    rig.seed_ready(ACCOUNT, "unified-142", "first");
    rig.fake.set_probe(|_| probe_result(Some(429), None));
    rig.select("req_tf", &[ACCOUNT], false, None).await;
    let reason = failed(rig.drive("req_tf", ACCOUNT, "gpt-6-astra", 10).await);
    assert_eq!(reason.code, "ticket_unavailable");
    assert_eq!(rig.query("SELECT COUNT(*) FROM leases")[0][0], 0);
    // 模型标识不合法时不发探测。
    let before = rig.fake.calls_to("upstream.probe").len();
    let reason = failed(rig.drive("req_tf2", ACCOUNT, "GPT 6", 10).await);
    assert_eq!(reason.code, "ticket_unavailable");
    assert_eq!(rig.fake.calls_to("upstream.probe").len(), before);
}

/// 只绑定一个降智账号的 Key：账号即使不在可调度名单里也进入路由，失败时给出具体原因。
#[tokio::test]
async fn pool_only_key_reaches_routing_and_reports_the_reason() {
    // 网关全部在静置。
    let gateways = ["unified-142", "unified-143", "unified-144"];
    let rig = Rig::start("reason-resting", json!({"gateways": gateways})).await;
    *rig.fake.gateways.lock().expect("gateways") = gateways.map(str::to_owned).to_vec();
    for name in gateways {
        rig.seed_cooling(ACCOUNT, name, 720.0);
    }
    rig.select("req_r", &[ACCOUNT], false, None).await;
    let reason = failed(rig.drive("req_r", ACCOUNT, "gpt-6-astra", 3).await);
    assert_eq!(reason.code, "all_resting");
    assert!(reason.message.contains("全部在静置"), "{}", reason.message);
    assert!(reason.message.contains("3 个网关"), "{}", reason.message);
    assert!(reason.message.contains("12 分钟"), "{}", reason.message);

    // 队列里停用。
    let rig = Rig::start("reason-queue", json!({})).await;
    rig.select("req_q", &[ACCOUNT], false, None).await;
    let update = object_of(
        json!({"revision": rig.queue_revision(), "accountId": ACCOUNT, "enabled": false}),
    );
    dispatch(&rig.store, "queue.update", &update).expect("disable");
    let reason = failed(rig.drive("req_q", ACCOUNT, "gpt-6-astra", 3).await);
    assert_eq!(reason.code, "queue_disabled");
    assert!(reason.message.contains("停用"), "{}", reason.message);

    // 另一个请求正在检测：等待到期前给出具体说明。
    let rig = Rig::start("reason-preparing", json!({"gateways": ["unified-142"]})).await;
    *rig.fake.gateways.lock().expect("gateways") = vec!["unified-142".into()];
    rig.seed_pair(
        ACCOUNT,
        "unified-142",
        json!({"accountId": ACCOUNT, "gateway": "unified-142", "state": "preparing", "generation": "other",
            "inFlight": 1, "firstContactAt": null}),
    );
    rig.select("req_p", &[ACCOUNT], false, None).await;
    let started = std::time::Instant::now();
    let reason = failed(rig.drive("req_p", ACCOUNT, "gpt-6-astra", 3).await);
    assert_eq!(reason.code, "wait_timeout");
    assert!(
        reason.message.contains("另一个请求正在为该账号检测网关"),
        "{}",
        reason.message
    );
    // 比请求期限早 1.5 秒放弃。
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
}

/// 走完整找票流程，探测错误最多重试一次并保留实际错误。
async fn probe_retries(status: u16, phase: &'static str, recovers: bool) {
    let rig = Rig::start("route-retries", json!({"gateways": ["unified-142"]})).await;
    *rig.fake.gateways.lock().expect("gateways") = vec!["unified-142".into()];
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&attempts);
    rig.fake.set_probe(move |params| {
        let replay = !params["turnState"].is_null();
        let in_phase = replay == (phase == "replay");
        let attempt = if in_phase {
            count.fetch_add(1, Ordering::SeqCst)
        } else {
            0
        };
        if in_phase && (!recovers || attempt == 0) {
            let mut result = probe_result(Some(status), None);
            result["retryAfterMs"] = json!(0);
            result
        } else if !replay {
            probe_result(Some(200), Some("verified-bootstrap-state"))
        } else {
            probe_result(Some(200), None)
        }
    });
    rig.select("req_retry", &[ACCOUNT], false, None).await;
    let result = rig.drive("req_retry", ACCOUNT, "gpt-6-astra", 10).await;
    rig.until(|| rig.query("SELECT COUNT(*) FROM leases WHERE kind='probe'")[0][0] == 0)
        .await;
    if recovers {
        let (_, upstream) = used(result);
        assert_eq!(
            upstream.turn_state.as_deref(),
            Some("verified-bootstrap-state")
        );
    } else {
        assert_eq!(failed(result).code, "all_resting");
    }
    let observations = rig.observations();
    let attempts: Vec<&Value> = observations
        .iter()
        .filter(|item| item["source"] == "probe-attempt" && item["phase"] == phase)
        .collect();
    assert_eq!(attempts.len(), 2, "exactly one retry");
    assert_eq!(attempts[0]["status"], status);
    assert_eq!(attempts[0]["retry"], 0);
    assert_eq!(attempts[1]["retry"], 1);
    // 每次尝试是独立的访问记录，起止时间完整。
    let accesses = rig.query(&format!(
        "SELECT id, started, ended FROM gp_accesses WHERE kind='{phase}' ORDER BY observed"
    ));
    assert_eq!(accesses.len(), 2);
    assert_ne!(accesses[0][0], accesses[1][0]);
    for row in &accesses {
        assert!(row[1].as_f64().expect("started") > 0.0);
        assert!(!row[2].is_null(), "access must end");
    }
    let last = observations
        .iter()
        .rev()
        .find(|item| item["source"] == "probe")
        .expect("probe result");
    assert_eq!(last["status"], if recovers { 200 } else { status });
    assert_eq!(last["stateReturned"], false);
    // 池 Cookie 只取一次。
    assert_eq!(rig.fake.fetches("/cookie").len(), 1);
}

#[tokio::test]
async fn bootstrap_402_retries_once_and_keeps_actual_error() {
    probe_retries(402, "bootstrap", false).await;
}

#[tokio::test]
async fn bootstrap_429_retries_once_and_keeps_actual_error() {
    probe_retries(429, "bootstrap", false).await;
}

#[tokio::test]
async fn replay_402_retries_once_and_keeps_actual_error() {
    probe_retries(402, "replay", false).await;
}

#[tokio::test]
async fn bootstrap_transient_error_recovers_then_replays_material() {
    probe_retries(402, "bootstrap", true).await;
}

#[tokio::test]
async fn bootstrap_401_retries_once_and_keeps_actual_error() {
    probe_retries(401, "bootstrap", false).await;
}

/// 探测请求的参数：bootstrap 不带 state、带路由 Cookie；重放带返回的 state 与合并后的 Cookie。
#[tokio::test]
async fn probe_requests_carry_bootstrap_then_replay_materials() {
    let rig = Rig::start("route-probe-shape", json!({})).await;
    rig.fake.set_probe(|params| {
        if params["turnState"].is_null() {
            let mut result = probe_result(Some(200), Some("bootstrap-state"));
            result["setCookies"] = json!(["__cflb=refreshed; Path=/"]);
            result
        } else {
            probe_result(Some(200), None)
        }
    });
    let (_, upstream) = lease(&rig, "req_shape", "gpt-6-astra").await;
    let probes = rig.fake.calls_to("upstream.probe");
    assert_eq!(probes.len(), 2);
    assert_eq!(probes[0]["accountId"], ACCOUNT);
    assert_eq!(probes[0]["model"], "gpt-6-astra");
    assert!(probes[0]["turnState"].is_null());
    assert_eq!(probes[0]["includeAccountCookies"], false);
    assert_eq!(probes[0]["timeoutMs"], 15000);
    assert_eq!(probes[1]["turnState"], "bootstrap-state");
    // 重放用 bootstrap 返回的 Cookie 合并后的值；业务请求拿到同样的 Cookie。
    let replay_cookie = probes[1]["cookie"].as_str().expect("cookie");
    assert!(
        replay_cookie.ends_with("__cflb=refreshed"),
        "{replay_cookie}"
    );
    assert_eq!(upstream.cookie.as_deref(), Some(replay_cookie));
    assert_eq!(upstream.turn_state.as_deref(), Some("bootstrap-state"));
    // 登记的访问：探测期间 bootstrap 与重放各一次，随后是业务请求。
    let kinds = rig.query("SELECT kind FROM gp_accesses ORDER BY observed");
    assert_eq!(
        kinds,
        vec![
            vec![json!("bootstrap")],
            vec![json!("replay")],
            vec![json!("business")]
        ]
    );
}

/// 池服务请求与旧网关客户端逐项一致：路径、查询、请求头、认证、出口、超时，目录缓存 60 秒。
#[tokio::test]
async fn pool_service_requests_match_the_old_gateway_client() {
    let rig = Rig::start(
        "route-pool-http",
        json!({"poolUrl": "https://pool.example.invalid/"}),
    )
    .await;
    lease(&rig, "req_http", "gpt-6-astra").await;
    let catalog = rig.fake.fetches("/gateways");
    assert_eq!(catalog.len(), 1, "目录在缓存期内只读一次");
    assert_eq!(
        catalog[0]["url"],
        "https://pool.example.invalid/gateways?account=gwpool-member%3Ademo-upstream%2Fdemo-user"
    );
    assert_eq!(catalog[0]["method"], "GET");
    assert_eq!(
        catalog[0]["headers"],
        json!([
            ["user-agent", "proxy-for-self"],
            ["authorization", format!("Bearer {POOL_TOKEN}")]
        ])
    );
    assert_eq!(catalog[0]["egress"], json!({"kind": "direct"}));
    assert_eq!(catalog[0]["timeoutMs"], 5000);
    assert!(catalog[0]["body"].is_null());
    let cookies = rig.fake.fetches("/cookie");
    assert_eq!(cookies.len(), 1);
    assert_eq!(
        cookies[0]["url"],
        "https://pool.example.invalid/cookie?account=gwpool-member%3Ademo-upstream%2Fdemo-user&gateway=unified-142"
    );
    assert_eq!(cookies[0]["headers"], catalog[0]["headers"]);
    assert_eq!(cookies[0]["timeoutMs"], 5000);

    // poolDirect 关闭时改用该账号的出口。
    let rig = Rig::start("route-pool-egress", json!({"poolDirect": false})).await;
    lease(&rig, "req_http", "gpt-6-astra").await;
    for path in ["/gateways", "/cookie"] {
        assert_eq!(
            rig.fake.fetches(path)[0]["egress"],
            json!({"kind": "account", "accountId": ACCOUNT})
        );
    }
}

/// 目录只保留可配对的网关，网关名、有效期与地区都要通过校验。
#[tokio::test]
async fn catalog_keeps_only_pair_ready_valid_gateways() {
    let rig = Rig::start("route-catalog", json!({})).await;
    // 池服务只有一个合格网关：其余不可配对、名字不合法或有效期越界。
    rig.fake.set_catalog_body(json!({"gateways": [
        {"name": "unified-300", "pair_ready": false, "valid_for_s": 3600},
        {"name": "other-1", "pair_ready": true, "valid_for_s": 3600},
        {"name": "unified-1234567", "pair_ready": true, "valid_for_s": 3600},
        {"name": "unified-301", "pair_ready": true, "valid_for_s": 100000},
        {"name": "unified-302", "pair_ready": true, "valid_for_s": 3600, "datacenter_country": "us"},
    ]}));
    rig.select("req_c", &[ACCOUNT], false, None).await;
    let (_, upstream) = used(rig.drive("req_c", ACCOUNT, "gpt-6-astra", 10).await);
    assert_eq!(gateway(&upstream), "unified-302");
    let stored = rig.query("SELECT data FROM catalogs");
    let entries: Value = serde_json::from_str(stored[0][0].as_str().expect("data")).expect("json");
    assert_eq!(
        entries,
        json!([{"gateway": "unified-302", "validForS": 3600.0, "country": null}])
    );
}

/// 池服务读取失败时目录按空值处理（退回限定列表或上一份目录），读取慢于钩子预算时返回 wait。
#[tokio::test]
async fn slow_catalog_returns_wait_instead_of_blocking_the_hook() {
    let rig = Rig::start("route-slow-catalog", json!({})).await;
    rig.fake.set_catalog_delay(Duration::from_millis(1800));
    rig.select("req_w", &[ACCOUNT], false, None).await;
    let deadline = (now_secs() * 1000.0) as u64 + 30_000;
    let started = std::time::Instant::now();
    let first = rig
        .prepare_once("req_w", ACCOUNT, "gpt-6-astra", deadline)
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let RoutePrepareResult::Wait { reason, .. } = first else {
        panic!("expected wait, got {first:?}");
    };
    assert!(reason.message.contains("网关目录"), "{}", reason.message);
    rig.fake.set_catalog_delay(Duration::ZERO);
    used(rig.drive("req_w", ACCOUNT, "gpt-6-astra", 10).await);
    assert_eq!(
        rig.fake.fetches("/gateways").len(),
        1,
        "慢读取在后台完成并写入缓存，不重复读取"
    );
}

/// A/B 与重启共用私有材料：内存里没有材料时读共享存储，不退役可用组合。
#[tokio::test]
async fn window_material_is_shared_through_the_store() {
    let rig = Rig::start("route-material", json!({})).await;
    rig.seed_ready(ACCOUNT, "unified-142", "seeded");
    let (_, upstream) = lease(&rig, "req_mat", "gpt-6-astra").await;
    assert_eq!(upstream.turn_state.as_deref(), Some("state-seeded"));
    assert_eq!(gateway(&upstream), "unified-142");
    assert!(rig.fake.calls_to("upstream.probe").is_empty());
    assert_eq!(rig.pair(ACCOUNT, "unified-142")["state"], "ready");
}

/// 只有持久化状态、没有配套材料的组合不能当成可用：作废预约并重新检测。
#[tokio::test]
async fn window_without_material_is_retired_and_probed_again() {
    let rig = Rig::start("route-no-material", json!({})).await;
    let now = now_secs();
    rig.seed_pair(
        ACCOUNT,
        "unified-142",
        json!({"accountId": ACCOUNT, "gateway": "unified-142", "state": "ready", "generation": "orphan",
            "firstContactAt": now, "lastContactAt": now, "verifiedAt": now, "inFlight": 0}),
    );
    rig.select("req_nm", &[ACCOUNT], false, None).await;
    let (_, upstream) = used(rig.drive("req_nm", ACCOUNT, "gpt-6-astra", 10).await);
    // 旧窗口退役，新窗口在另一个网关上就绪。
    assert_eq!(
        rig.pair(ACCOUNT, "unified-142")["retiredReason"],
        "rotation"
    );
    assert_eq!(gateway(&upstream), "unified-143");
    assert_eq!(
        rig.query("SELECT COUNT(*) FROM leases WHERE kind='business'")[0][0],
        1
    );
}

/// 响应头的判定：21 接受，29 换网关，网关不一致失败，非 200 和没有响应时交回内核。
#[tokio::test]
async fn response_head_decisions() {
    // 非 200：归还租约并 accept，让内核透传原错误。
    let rig = Rig::start("head-status", json!({})).await;
    let (lease_id, _) = lease(&rig, "req_h", "gpt-6-astra").await;
    assert_eq!(
        rig.head(
            "req_h",
            &lease_id,
            Some(429),
            false,
            vec!["__cflb=invalid\nvalue".into()]
        )
        .await,
        ResponseHeadResult::Accept
    );
    assert_eq!(rig.lease_count(&lease_id), 0);
    let status_row = rig
        .observations()
        .into_iter()
        .rev()
        .find(|item| item["source"] == "business")
        .expect("business");
    assert_eq!(status_row["status"], 429);
    assert_eq!(status_row["classification"], Value::Null);

    // 网关不一致：失败，窗口退役。
    let rig = Rig::start("head-mismatch", json!({})).await;
    let (lease_id, _) = lease(&rig, "req_h", "gpt-6-astra").await;
    let other = cookie_for("unified-143", now_secs() + 600.0);
    let ResponseHeadResult::Fail { reason } = rig
        .head("req_h", &lease_id, Some(200), false, vec![other])
        .await
    else {
        panic!("expected fail");
    };
    assert_eq!(reason.code, "gateway_mismatch");
    assert_eq!(rig.lease_count(&lease_id), 0);
    assert_ne!(rig.pair(ACCOUNT, "unified-142")["state"], "ready");

    // 没有响应（连接失败）：交回内核，租约保持到请求结束。
    let rig = Rig::start("head-error", json!({})).await;
    let (lease_id, _) = lease(&rig, "req_h", "gpt-6-astra").await;
    assert_eq!(
        rig.head("req_h", &lease_id, None, false, vec![]).await,
        ResponseHeadResult::Accept
    );
    assert_eq!(rig.lease_count(&lease_id), 1);
    rig.finish("req_h", &lease_id, RequestOutcome::Failed).await;
    let access = rig.query("SELECT outcome FROM gp_accesses WHERE kind='business'");
    assert_eq!(access, vec![vec![json!("failed")]]);

    // 租约状态丢失（例如插件刚重启）：无法判定，失败。
    let ResponseHeadResult::Fail { reason } = rig
        .head("req_h", "unknown-lease", Some(200), false, vec![])
        .await
    else {
        panic!("expected fail");
    };
    assert_eq!(reason.code, "lease_unknown");
}

/// 请求结束带访问计时和结果；插件重启后接管的租约（没有内存状态）也能归还。
#[tokio::test]
async fn request_finish_records_the_access_and_releases_adopted_leases() {
    let rig = Rig::start("finish-adopt", json!({})).await;
    let (lease_id, _) = lease(&rig, "req_f", "gpt-6-astra").await;
    assert_eq!(
        rig.head("req_f", &lease_id, Some(200), false, vec![]).await,
        ResponseHeadResult::Accept
    );
    rig.finish("req_f", &lease_id, RequestOutcome::Cancelled)
        .await;
    let access = rig.query("SELECT outcome, ended, elapsed FROM gp_accesses WHERE kind='business'");
    assert_eq!(access[0][0], "cancelled");
    assert!(access[0][1].as_f64().expect("ended") > 0.0);

    // 另一个窗口上的租约由运行状态机预约，插件内存里没有它。
    rig.seed_ready("acct_adopted", "unified-143", "adopted");
    let reserved = rig
        .store
        .runtime(
            &json!({"action": "reserve", "accountId": "acct_adopted", "gateway": "unified-143",
            "generation": "adopted", "deadline": now_secs() + 60.0}),
        )
        .expect("reserve");
    let adopted = reserved["leaseId"].as_str().expect("lease").to_owned();
    assert_eq!(rig.lease_count(&adopted), 1);
    rig.finish("req_adopted", &adopted, RequestOutcome::Interrupted)
        .await;
    assert_eq!(rig.pair("acct_adopted", "unified-143")["inFlight"], 0);
}

/// 本轮网关轮完且窗口到期时，同一 Key 内有退路（普通账号）则账号让位，由内核改用别的账号。
#[tokio::test]
async fn account_yields_when_its_round_is_complete_and_the_key_has_a_way_out() {
    let rig = Rig::start(
        "route-yield",
        json!({"gateways": ["unified-142"], "windowSeconds": 1}),
    )
    .await;
    *rig.fake.gateways.lock().expect("gateways") = vec!["unified-142".into()];
    let (first, _) = lease(&rig, "req_y1", "gpt-6-astra").await;
    assert_eq!(
        rig.head("req_y1", &first, Some(200), false, vec![]).await,
        ResponseHeadResult::Accept
    );
    rig.finish("req_y1", &first, RequestOutcome::Completed)
        .await;
    // 选号时窗口还没到期；到路由时窗口用满（1 秒），本轮只有这一个网关：有退路时让位。
    rig.select("req_y2", &[ACCOUNT], true, None).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let reason = failed(rig.drive("req_y2", ACCOUNT, "gpt-6-astra", 5).await);
    assert_eq!(reason.code, "yielded");
    assert!(reason.message.contains("让位"), "{}", reason.message);
    assert_eq!(
        rig.pair(ACCOUNT, "unified-142")["retiredReason"],
        "rotation"
    );
}
