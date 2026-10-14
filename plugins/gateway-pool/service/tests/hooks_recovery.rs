mod common;
mod hooks_support;
use gateway_pool::{ManualClock, Store};
use hooks_support::*;
use plugin_sdk::protocol::*;
use serde_json::{Value, json};
use std::time::Duration;

#[tokio::test]
async fn continuous_ticks_rotate_active_accounts_but_lazy_expired_and_drained_do_not() {
    for (mode, expired, drain) in [
        ("continuous", false, false),
        ("lazy", false, false),
        ("continuous", true, false),
        ("continuous", false, true),
    ] {
        let clock = ManualClock::new(now_secs());
        let rig = Rig::start_clock("background", json!({"mode":mode}), clock.clock()).await;
        rig.seed_ready(ACCOUNT, "unified-142", "first");
        let RoutePrepareResult::Use { lease_id, .. } =
            rig.drive("req_active", ACCOUNT, "gpt-6-astra", 10).await
        else {
            panic!("use");
        };
        rig.finish("req_active", &lease_id, RequestOutcome::Completed)
            .await;
        if drain {
            let _: DrainResult = rig
                .kernel
                .call("drain", &EmptyParams {}, Duration::from_secs(3))
                .await
                .unwrap();
        }
        // 令当前窗口需要轮换；active 过期则不应再读取目录或打票。
        let mut pair = rig.pair(ACCOUNT, "unified-142");
        pair["firstContactAt"] = json!((clock.clock())() - 200.0);
        rig.seed_pair(ACCOUNT, "unified-142", pair);
        if expired {
            clock.advance(1800.0);
        }
        if mode == "continuous" && !expired && !drain {
            rig.until(|| rig.pair(ACCOUNT, "unified-143")["state"] == "ready")
                .await;
            assert_eq!(rig.fake.calls_to("upstream.probe").len(), 2);
        } else {
            tokio::time::sleep(Duration::from_millis(1200)).await;
            assert!(rig.fake.calls_to("upstream.probe").is_empty());
        }
    }
}

#[tokio::test]
async fn new_instance_adopts_head_and_finish_with_private_facts_and_timing() {
    let rig = Rig::start("adoption", json!({})).await;
    // 未调用 select：接管缺失请求状态也按没有退路准备。
    let RoutePrepareResult::Use { lease_id, .. } =
        rig.drive("req_adopt", ACCOUNT, "gpt-6-astra", 10).await
    else {
        panic!("use");
    };
    let facts: Value = serde_json::from_str(
        rig.query("SELECT data FROM gp_hook_leases")[0][0]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(facts["account"], ACCOUNT);
    assert_eq!(facts["gateway"], "unified-142");
    assert!(facts["access"]["startedAt"].is_number());
    let (new, task) = rig.peer().await;
    let result: ResponseHeadResult = new
        .call(
            "response.head",
            &ResponseHeadParams {
                request_id: "req_adopt".into(),
                lease_id: lease_id.clone(),
                attempt: 1,
                status: Some(200),
                error_kind: None,
                state_returned: false,
                turn_state: None,
                set_cookies: vec![],
                duration_ms: 7,
                replay_safe: true,
            },
            Duration::from_secs(3),
        )
        .await
        .unwrap();
    assert_eq!(result, ResponseHeadResult::Accept);
    new.notify(
        "request.finish",
        &RequestFinishParams {
            request_id: "req_adopt".into(),
            lease_id: lease_id.clone(),
            outcome: RequestOutcome::Completed,
            duration_ms: 10,
            first_byte_ms: Some(1),
            usage: None,
        },
    )
    .await
    .unwrap();
    rig.until(|| rig.lease_count(&lease_id) == 0).await;
    rig.until(|| rig.query("SELECT id FROM gp_hook_leases").is_empty())
        .await;
    let access = rig.query("SELECT outcome,ended FROM gp_accesses WHERE kind='business'");
    assert_eq!(access[0][0], "completed");
    assert!(access[0][1].as_f64().unwrap() > 0.0);
    task.abort();
}

#[test]
fn interrupted_background_probe_is_reaped_and_can_be_claimed_after_rest() {
    let h = common::Harness::new("reap", 10000.0);
    h.call_ok(
        "configure",
        json!({"gateways":["unified-142"],"initialRestSeconds":1,"maximumRestSeconds":8}),
    );
    let claim = h.rt("claim", json!({"demand":true}));
    assert_eq!(claim["claimed"], true);
    h.rt("sent", json!({"leaseId":claim["leaseId"]}));
    let expires: f64 = h
        .connection()
        .query_row("SELECT expires FROM leases", [], |r| r.get(0))
        .unwrap();
    h.clock.set(expires + 1.0);
    let new = Store::open(h.dir.path(), h.clock.clock()).unwrap();
    new.runtime(&json!({"action":"config"})).unwrap();
    assert_eq!(h.count("leases"), 0);
    assert_eq!(h.pair()["retiredReason"], "probe_interrupted");
    h.clock.set(h.pair()["restUntil"].as_f64().unwrap() + 1.0);
    let next = new
        .runtime(&json!({"action":"claim","accountId":"A","demand":true}))
        .unwrap();
    assert_eq!(next["claimed"], true);
    assert_ne!(next["pair"]["generation"], claim["pair"]["generation"]);
}

#[tokio::test]
async fn expired_adoption_facts_follow_original_lease_reaping() {
    let clock = ManualClock::new(now_secs());
    let rig = Rig::start_clock("adoption-expiry", json!({}), clock.clock()).await;
    rig.seed_ready(ACCOUNT, "unified-142", "expiry");
    let RoutePrepareResult::Use { lease_id, .. } =
        rig.drive("req_expiry", ACCOUNT, "gpt-6-astra", 10).await
    else {
        panic!("use");
    };
    assert_eq!(rig.query("SELECT id FROM gp_hook_leases").len(), 1);
    clock.advance(11.0);
    rig.store.runtime(&json!({"action":"config"})).unwrap();
    assert_eq!(rig.lease_count(&lease_id), 0);
    assert!(rig.query("SELECT id FROM gp_hook_leases").is_empty());
}
