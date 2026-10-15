mod common;
mod hooks_support;
use hooks_support::*;
use plugin_sdk::protocol::*;
use serde_json::{Value, json};
use std::time::Duration;

const NORMAL: &str = "grp_00000000000000000000000000000050";
fn settings() -> Value {
    json!({"dynamicGroups":true,"normalGroupId":NORMAL,"degradedGroupId":GROUP})
}
fn observe() -> ResponseObserveParams {
    ResponseObserveParams {
        request_id: "req_observe".into(),
        account_id: ACCOUNT.into(),
        attempt: 1,
        model: "gpt-6-astra".into(),
        sse: true,
        request_had_turn_state: true,
        status: Some(200),
        error_kind: None,
        state_returned: true,
        turn_state: Some("returned-state-a".into()),
        set_cookies: vec!["route=new; Path=/".into()],
        replay_safe: true,
        can_switch: true,
    }
}
async fn call(rig: &Rig, params: &ResponseObserveParams) -> ResponseObserveResult {
    rig.kernel
        .call("response.observe", params, Duration::from_secs(3))
        .await
        .unwrap()
}

#[tokio::test]
async fn normal_account_degradation_is_confirmed_replayed_elsewhere_and_regrouped() {
    for (carries_state, can_switch, regroup) in [
        (true, true, true),
        (true, false, true),
        (false, true, false),
    ] {
        let rig = Rig::start("observe", settings()).await;
        rig.fake
            .account_mut(ACCOUNT, |a| a.groups = vec![NORMAL.into()]);
        rig.fake
            .set_probe(|_| probe_result(Some(200), Some("confirmed")));
        let mut params = observe();
        params.request_had_turn_state = carries_state;
        params.can_switch = can_switch;
        let result = call(&rig, &params).await;
        assert_eq!(
            matches!(result, ResponseObserveResult::SwitchAccount { .. }),
            carries_state && can_switch
        );
        if regroup {
            rig.until(|| {
                rig.query("SELECT status FROM group_events") == vec![vec![json!("moved")]]
            })
            .await;
            let probes = rig.fake.calls_to("upstream.probe");
            assert_eq!(probes.len(), 1);
            assert_eq!(probes[0]["turnState"], "returned-state-a");
            assert_eq!(probes[0]["includeAccountCookies"], true);
            assert_eq!(probes[0]["cookie"], "route=new");
            assert_eq!(probes[0]["timeoutMs"], 15000);
            let moves = rig.fake.calls_to("groups.move");
            assert_eq!(moves.len(), 1);
            assert_eq!(moves[0]["accountId"], ACCOUNT);
            assert_eq!(moves[0]["removeGroup"], NORMAL);
            assert_eq!(moves[0]["addGroup"], GROUP);
            assert_eq!(call(&rig, &params).await, ResponseObserveResult::Accept);
            assert_eq!(rig.fake.calls_to("upstream.probe").len(), 1);
        } else {
            assert!(rig.fake.calls_to("upstream.probe").is_empty());
        }
    }
}

#[tokio::test]
async fn observe_guards_normal_unknown_and_move_failure_preserve_response() {
    let rig = Rig::start("observe-guards", settings()).await;
    for n in 0..4 {
        let mut p = observe();
        match n {
            0 => p.sse = false,
            1 => p.status = Some(429),
            2 => p.turn_state = None,
            _ => p.request_had_turn_state = false,
        }
        assert_eq!(call(&rig, &p).await, ResponseObserveResult::Accept);
    }
    assert!(rig.fake.calls_to("upstream.probe").is_empty());
    for status in [Some(200), Some(429), None] {
        let rig = Rig::start("observe-normal", settings()).await;
        rig.fake.set_probe(move |_| probe_result(status, None));
        assert_eq!(call(&rig, &observe()).await, ResponseObserveResult::Accept);
        assert!(rig.fake.calls_to("groups.move").is_empty());
    }
    let rig = Rig::start("observe-move-failure", settings()).await;
    rig.fake
        .set_probe(|_| probe_result(Some(200), Some("confirmed")));
    rig.fake
        .failing
        .lock()
        .unwrap()
        .insert("groups.move".into());
    assert!(matches!(
        call(&rig, &observe()).await,
        ResponseObserveResult::SwitchAccount { .. }
    ));
    rig.until(|| rig.query("SELECT status FROM group_events") == vec![vec![json!("failed")]])
        .await;
}

#[tokio::test]
async fn degraded_group_account_recovers_after_old_state_and_fresh_checks_stay_full_power() {
    for failed in [false, true] {
        let rig = Rig::start("recovery", settings()).await;
        rig.seed_ready(ACCOUNT, "unified-142", "old");
        let lease = rig.drive("req_recovery", ACCOUNT, "gpt-6-astra", 10).await;
        let RoutePrepareResult::Use { lease_id, .. } = lease else {
            panic!("use");
        };
        rig.finish("req_recovery", &lease_id, RequestOutcome::Completed)
            .await;
        // 初次扫描尚未够 8 分钟，不应把 recovering 永久占住。
        tokio::time::sleep(Duration::from_millis(1100)).await;
        let mut pair = rig.pair(ACCOUNT, "unified-142");
        pair["firstContactAt"] = json!(now_secs() - 500.0);
        rig.seed_pair(ACCOUNT, "unified-142", pair);
        rig.fake.set_probe(move |p| match p["turnState"].as_str() {
            Some("state-old") if failed => probe_result(Some(200), Some("new-state")),
            Some("state-old" | "fresh-state") => probe_result(Some(200), None),
            None => {
                let mut r = probe_result(Some(200), Some("fresh-state"));
                r["setCookies"] = json!(["fresh=cookie; Path=/"]);
                r
            }
            other => panic!("unexpected {other:?}"),
        });
        rig.until(|| {
            !rig.query("SELECT last_outcome FROM recovery_checks WHERE last_outcome IS NOT NULL")
                .is_empty()
        })
        .await;
        assert_eq!(
            rig.query("SELECT last_outcome FROM recovery_checks")[0][0],
            if failed { "failed" } else { "recovered" }
        );
        let probes = rig.fake.calls_to("upstream.probe");
        assert_eq!(probes.len(), if failed { 1 } else { 5 });
        assert_eq!(probes[0]["turnState"], "state-old");
        assert_eq!(probes[0]["timeoutMs"], 20000);
        if failed {
            assert!(rig.fake.calls_to("groups.move").is_empty());
        } else {
            rig.until(|| {
                rig.query("SELECT status FROM group_events") == vec![vec![json!("moved")]]
            })
            .await;
            let moves = rig.fake.calls_to("groups.move");
            assert_eq!(moves[0]["removeGroup"], GROUP);
            assert_eq!(moves[0]["addGroup"], NORMAL);
            assert!(probes[1]["cookie"].is_null());
            assert_eq!(probes[2]["cookie"], "fresh=cookie");
        }
    }
}

#[tokio::test]
async fn protocol_six_eligibility_uses_probe_model_and_refreshes_missing_quota() {
    let rig = Rig::start("eligibility-six", json!({})).await;
    rig.fake.account_mut(ACCOUNT, |a| a.model_allowed = false);
    let select = rig.select("req_model", &[ACCOUNT], false, None).await;
    assert_eq!(select.blocked[0].reason.code, "no_probe_model_access");
    assert!(
        matches!(rig.drive("req_model",ACCOUNT,"gpt-6.1-sol",5).await,RoutePrepareResult::Fail{reason} if reason.code=="no_probe_model_access")
    );
    assert!(
        rig.fake
            .calls_to("accounts.read")
            .iter()
            .all(|p| p["model"] == "gpt-6-astra")
    );
    rig.fake.account_mut(ACCOUNT, |a| a.model_allowed = true);
    rig.fake.quota_mut(ACCOUNT, |q| q.present = false);
    let select = rig.select("req_quota", &[ACCOUNT], false, None).await;
    assert_eq!(select.blocked[0].reason.code, "quota_unreadable");
    assert!(
        rig.fake
            .calls_to("quota.read")
            .iter()
            .all(|p| p["refreshIfMissing"] == true)
    );
}

#[tokio::test]
async fn confirmation_expires_after_twenty_seconds_and_watchers_follow_configuration() {
    let clock = gateway_pool::ManualClock::new(now_secs());
    let rig = Rig::start_clock("observe-expiry", settings(), clock.clock()).await;
    assert_eq!(rig.store.ownership().unwrap().watched_groups, vec![NORMAL, "*"]);
    rig.fake.set_probe(|_| probe_result(Some(200), None));
    assert_eq!(call(&rig, &observe()).await, ResponseObserveResult::Accept);
    clock.advance(19.0);
    call(&rig, &observe()).await;
    assert_eq!(rig.fake.calls_to("upstream.probe").len(), 1);
    clock.advance(1.0);
    call(&rig, &observe()).await;
    assert_eq!(rig.fake.calls_to("upstream.probe").len(), 2);
    rig.store
        .configure(json!({"dynamicGroups":false}).as_object().unwrap())
        .unwrap();
    assert!(rig.store.ownership().unwrap().watched_groups.is_empty());
}
