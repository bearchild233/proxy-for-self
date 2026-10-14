//! 共用账号队列与运行状态机的配合，移植自 `services/plugin_host/test_gateway_pool_queue.py`。
//! 初始状态沿用历史用例的 setUp：网关 142/143，初始静置 1 秒，最大静置 8 秒。

mod common;

use common::Harness;
use serde_json::{Value, json};

fn register(harness: &Harness) -> Value {
    harness.rt_raw(json!({"action": "active", "candidates": [
        {"id": "A", "exhausted": false}, {"id": "B", "exhausted": false}, {"id": "C", "exhausted": false}]}))
}

fn reorder(harness: &Harness, accounts: Value) {
    harness.call_ok(
        "queue.update",
        json!({"revision": harness.queue_revision(), "accountIds": accounts}),
    );
}

fn set_enabled(harness: &Harness, account: &str, enabled: bool) {
    harness.call_ok(
        "queue.update",
        json!({"revision": harness.queue_revision(), "accountId": account, "enabled": enabled}),
    );
}

#[test]
fn manual_order_persists_and_key_scope_stays_restricted() {
    let mut harness = Harness::history();
    assert_eq!(register(&harness)["accounts"], json!(["A", "B", "C"]));
    reorder(&harness, json!(["C", "A", "B"]));
    harness.reopen();
    assert_eq!(harness.queue_order(), vec!["C", "A", "B"]);
    let result = harness.rt_raw(json!({"action": "active", "candidates": [
        {"id": "A", "exhausted": false}, {"id": "C", "exhausted": false}]}));
    assert_eq!(result["accounts"], json!(["C", "A"]));
}

#[test]
fn disable_stops_new_requests_but_keeps_sent_lease() {
    let harness = Harness::history();
    register(&harness);
    let pair = harness.probe("21", json!({}));
    let (sent, access) = harness.business(&pair);
    let unsent = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 100.0}),
    )["leaseId"]
        .clone();
    set_enabled(&harness, "A", false);
    assert_eq!(
        harness.rt("sent", json!({"leaseId": unsent}))["accepted"],
        json!(false)
    );
    assert_eq!(
        harness.rt(
            "reserve",
            json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 100.0}),
        )["reserved"],
        json!(false)
    );
    assert_eq!(
        harness.rt("claim", json!({"demand": true}))["claimed"],
        json!(false)
    );
    let blocked = harness.rt("plan", json!({"demand": true}))["blocked"].clone();
    assert_eq!(blocked["code"], json!("queue_disabled"));
    assert!(
        blocked["message"]
            .as_str()
            .is_some_and(|text| text.contains("停用"))
    );
    harness.finish_access(&sent, &access, 10.0);
    harness.rt("finish", json!({"leaseId": unsent}));
    assert_eq!(harness.count("leases"), 0);
    assert_eq!(harness.queue_order(), vec!["A", "B", "C"]);
    assert_eq!(register(&harness)["accounts"], json!(["B", "C"]));
}

#[test]
fn full_gateway_round_moves_once_and_stale_probe_cannot_restart_it() {
    let harness = Harness::history();
    register(&harness);
    let turn = harness.rt("plan", json!({"demand": true}))["queueTurn"].clone();
    let first = harness.probe("21", json!({}));
    harness.retire_pair(&first);
    assert_eq!(harness.queue_order(), vec!["A", "B", "C"]);
    let second = harness.probe("21", json!({}));
    harness.advance(181.0);
    // 本 Key 里还有别的可用账号：轮完一遍后让位，账号沉到最后。
    assert_eq!(
        harness.rt("plan", json!({"demand": true, "alternatives": ["B", "C"]}))["yieldAccount"],
        json!(true)
    );
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
    assert_eq!(
        harness.rt(
            "claim",
            json!({"demand": true, "queueTurn": turn, "alternatives": ["B", "C"]})
        )["claimed"],
        json!(false)
    );
    harness.rt("finish", json!({"leaseId": second["generation"]}));
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
}

#[test]
fn quota_moves_once_recovers_at_tail_and_cas_rejects_stale_drag() {
    let harness = Harness::history();
    register(&harness);
    let old = harness.queue_revision();
    harness.rt("quota", json!({"exhausted": true}));
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
    let revision = harness.queue_revision();
    harness.rt("quota", json!({"exhausted": true}));
    assert_eq!(revision, harness.queue_revision());
    assert_eq!(
        common::message(harness.call(
            "queue.update",
            json!({"revision": old, "accountIds": ["C", "B", "A"]})
        )),
        "账号队列已变化，请刷新后重试"
    );
    harness.rt("quota", json!({"exhausted": false}));
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
    assert_eq!(register(&harness)["accounts"], json!(["B", "C", "A"]));
}

#[test]
fn reordered_account_can_prepare_without_interrupting_old_inflight() {
    let harness = Harness::history();
    register(&harness);
    let pair = harness.probe("21", json!({}));
    let (lease, access) = harness.business(&pair);
    reorder(&harness, json!(["B", "A", "C"]));
    let claim = harness.rt("claim", json!({"accountId": "B", "demand": true}));
    // B 排到前面后可以独立找票；A 的窗口属于别的 Key，不受影响。
    assert_eq!(claim["claimed"], json!(true));
    assert_eq!(
        harness.view(json!({"accountId": "A"}))["pairs"][0]["state"],
        json!("ready")
    );
    harness.finish_access(&lease, &access, 10.0);
    assert_eq!(
        harness.view(json!({"accountId": "A"}))["pairs"][0]["inFlight"],
        json!(0)
    );
}

#[test]
fn simultaneous_quota_reports_only_move_once() {
    let harness = Harness::history();
    register(&harness);
    let revision = harness.queue_revision();
    std::thread::scope(|scope| {
        let reports: Vec<_> = (0..2)
            .map(|_| scope.spawn(|| harness.rt("quota", json!({"exhausted": true}))))
            .collect();
        for report in reports {
            report.join().expect("quota report");
        }
    });
    assert_eq!(harness.queue_revision(), revision + 1);
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
}

#[test]
fn unsent_failure_does_not_consume_gateway_round() {
    let harness = Harness::history();
    register(&harness);
    for _ in 0..3 {
        let claim = harness.rt("claim", json!({"demand": true}));
        harness.rt(
            "probeResult",
            json!({"leaseId": claim["leaseId"], "failed": true}),
        );
        harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
        harness.advance(6.0);
    }
    assert_eq!(harness.queue_order(), vec!["A", "B", "C"]);
}

#[test]
fn disable_between_bootstrap_and_replay_stops_next_access() {
    let harness = Harness::history();
    register(&harness);
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.send(&claim["leaseId"], "bootstrap");
    set_enabled(&harness, "A", false);
    assert_eq!(
        harness.rt(
            "sent",
            json!({"leaseId": claim["leaseId"], "purpose": "replay"})
        )["accepted"],
        json!(false)
    );
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    assert_ne!(harness.view(json!({}))["pairs"][0]["state"], json!("ready"));
}

#[test]
fn gateway_configuration_restarts_round_without_removed_candidates() {
    let harness = Harness::history();
    register(&harness);
    let first = harness.probe("21", json!({}));
    harness.retire_pair(&first);
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    harness.advance(10.0);
    harness.probe("21", json!({}));
    harness.advance(181.0);
    assert_eq!(
        harness.rt("plan", json!({"demand": true, "alternatives": ["B"]}))["yieldAccount"],
        json!(true)
    );
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
}

#[test]
fn all_cooling_skips_account_without_probing_and_restores_after_cooldown() {
    let harness = Harness::history();
    harness.call_ok(
        "configure",
        json!({"initialRestSeconds": 7200, "maximumRestSeconds": 14400}),
    );
    register(&harness);
    harness.probe("29", json!({}));
    harness.probe("29", json!({}));
    let before = harness.view(json!({}))["observations"].clone();
    assert_eq!(register(&harness)["accounts"], json!(["B", "C"]));
    assert_eq!(harness.view(json!({}))["observations"], before);
    harness.advance(7201.0);
    assert_eq!(register(&harness)["accounts"], json!(["B", "C", "A"]));
    assert_eq!(harness.view(json!({}))["observations"], before);
}

#[test]
fn selection_finishes_last_window_before_returning_queue_order() {
    let harness = Harness::history();
    register(&harness);
    let first = harness.probe("21", json!({}));
    harness.retire_pair(&first);
    harness.probe("21", json!({}));
    harness.advance(181.0);
    assert_eq!(register(&harness)["accounts"], json!(["B", "C", "A"]));
    let revision = harness.queue_revision();
    register(&harness);
    assert_eq!(harness.queue_revision(), revision);
}

#[test]
fn other_accounts_window_never_blocks_a_candidate() {
    let harness = Harness::history();
    register(&harness);
    harness.probe("21", json!({}));
    let only_b = json!({"action": "active", "candidates": [{"id": "B", "exhausted": false}]});
    assert_eq!(harness.rt_raw(only_b.clone())["accounts"], json!(["B"]));
    reorder(&harness, json!(["B", "A", "C"]));
    assert_eq!(harness.rt_raw(only_b)["accounts"], json!(["B"]));
}

#[test]
fn priority_is_not_a_queue_any_listed_account_may_take_the_turn() {
    let harness = Harness::history();
    register(&harness);
    // A 不在队首也无妨：B 持有窗口时本 Key 复用 B；A 可探测但排在持窗账号之后。
    let claim = harness.rt("claim", json!({"accountId": "B", "demand": true}));
    harness.rt(
        "sent",
        json!({"accountId": "B", "leaseId": claim["leaseId"]}),
    );
    harness.rt(
        "probeResult",
        json!({"accountId": "B", "leaseId": claim["leaseId"], "status": 200, "stateReturned": false,
               "paired": true, "gatewayMatches": true}),
    );
    harness.rt(
        "finish",
        json!({"accountId": "B", "leaseId": claim["leaseId"]}),
    );
    let result = register(&harness);
    assert_eq!(result["accounts"], json!(["B", "A", "C"]));
    assert_eq!(result["live"], json!(["B"]));
}

#[test]
fn blocked_accounts_carry_a_reason_and_message() {
    let harness = Harness::history();
    register(&harness);
    set_enabled(&harness, "B", false);
    harness.probe("29", json!({}));
    harness.probe("29", json!({}));
    let result = harness.rt_raw(json!({"action": "active", "candidates": [
        {"id": "A", "exhausted": false}, {"id": "B", "exhausted": false}, {"id": "C", "exhausted": true}]}));
    let reasons = result["blocked"].as_array().expect("blocked");
    let reason = |account: &str| {
        reasons
            .iter()
            .find(|item| item["accountId"] == json!(account))
            .unwrap_or_else(|| panic!("no reason for {account}"))
            .clone()
    };
    assert_eq!(reason("A")["code"], json!("all_resting"));
    assert!(
        reason("A")["message"]
            .as_str()
            .is_some_and(|text| text.contains("静置"))
    );
    assert!(reason("A")["nextRecoveryAt"].as_f64().expect("next") > harness.now());
    assert_eq!(reason("B")["code"], json!("queue_disabled"));
    assert_eq!(reason("C")["code"], json!("quota_exhausted"));
    assert_eq!(result["accounts"], json!([]));
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["blocked"]["code"], json!("all_resting"));
}

#[test]
fn single_account_key_restarts_its_round_instead_of_yielding() {
    let harness = Harness::history();
    register(&harness);
    let first = harness.probe("21", json!({}));
    harness.retire_pair(&first);
    harness.probe("21", json!({}));
    harness.advance(181.0);
    let plan = harness.rt("plan", json!({"demand": true}));
    assert!(plan.get("yieldAccount").is_none());
    assert!(!plan["probe"].is_null());
    assert_eq!(harness.queue_order(), vec!["B", "C", "A"]);
    let claim = harness.rt("claim", json!({"demand": true}));
    assert_eq!(claim["claimed"], json!(true));
}
