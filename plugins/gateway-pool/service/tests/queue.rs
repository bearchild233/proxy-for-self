//! 共用账号队列的管理侧用例，移植自 `test_gateway_pool_queue.py` 中不依赖运行状态机的部分。

mod common;

use common::{Harness, message};
use serde_json::{Value, json};

fn queue_order(harness: &Harness) -> Vec<String> {
    harness.call_ok("read", json!({}))["accountQueue"]["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|account| {
            account["accountId"]
                .as_str()
                .expect("account id")
                .to_owned()
        })
        .collect()
}

fn revision(harness: &Harness) -> i64 {
    harness.call_ok("read", json!({}))["accountQueue"]["revision"]
        .as_i64()
        .expect("revision")
}

/// 对应 `test_manual_order_persists_and_key_scope_stays_restricted` 中的排序与持久化部分。
#[test]
fn manual_order_persists_and_rejects_a_stale_revision() {
    let mut harness = Harness::new("queue-order", 10_000.0);
    let registered = harness.call_ok(
        "queue.update",
        json!({"revision": 0, "accountIds": ["A", "B", "C"]}),
    );
    assert_eq!(registered["accounts"].as_array().map(Vec::len), Some(3));
    let current = revision(&harness);
    harness.call_ok(
        "queue.update",
        json!({"revision": current, "accountIds": ["C", "A", "B"]}),
    );
    harness.reopen();
    assert_eq!(queue_order(&harness), vec!["C", "A", "B"]);

    // 旧页面的拖拽（使用过期 revision）必须被拒绝，不能覆盖最新顺序。
    assert_eq!(
        message(harness.call(
            "queue.update",
            json!({"revision": current, "accountIds": ["A", "B", "C"]}),
        )),
        "账号队列已变化，请刷新后重试"
    );
    assert_eq!(queue_order(&harness), vec!["C", "A", "B"]);
}

/// 排序与开关不能在同一次提交中混用；账号列表格式错误时整体拒绝。
#[test]
fn queue_update_rejects_mixed_and_malformed_requests() {
    let harness = Harness::new("queue-invalid", 10_000.0);
    harness.call_ok(
        "queue.update",
        json!({"revision": 0, "accountIds": ["A", "B"]}),
    );
    let current = revision(&harness);
    assert_eq!(
        message(harness.call(
            "queue.update",
            json!({"revision": current, "accountIds": ["A"], "accountId": "A", "enabled": false}),
        )),
        "不能同时排序和切换账号"
    );
    assert_eq!(
        message(harness.call(
            "queue.update",
            json!({"revision": current, "accountIds": ["A", "A"]})
        )),
        "账号列表无效"
    );
    assert_eq!(
        message(harness.call(
            "queue.update",
            json!({"revision": current, "accountIds": ["bad id"]})
        )),
        "账号列表无效"
    );
    assert_eq!(
        message(harness.call("queue.update", json!({"revision": current, "unknown": 1}))),
        "账号队列参数无效"
    );
    assert_eq!(
        message(harness.call(
            "queue.update",
            json!({"revision": current, "accountId": "A", "enabled": "no"}),
        )),
        "账号开关无效"
    );
    assert_eq!(
        message(harness.call(
            "queue.update",
            json!({"revision": 1.0, "accountIds": ["A"]})
        )),
        "账号队列已变化，请刷新后重试"
    );
}

/// 对应 `test_disable_stops_new_requests_but_keeps_sent_lease` 中的停用部分：
/// 停用账号后，服务中的组合立即退役，队列开关可以切回。
#[test]
fn disabling_an_account_retires_its_serving_pair_and_can_be_re_enabled() {
    let harness = Harness::new("queue-disable", 10_000.0);
    harness.call_ok(
        "queue.update",
        json!({"revision": 0, "accountIds": ["A", "B", "C"]}),
    );
    harness.seed_pair(
        "A",
        "unified-142",
        json!({"accountId": "A", "gateway": "unified-142", "state": "ready", "inFlight": 0,
               "generation": "g1", "firstContactAt": 9000.0}),
    );
    let current = revision(&harness);
    let updated = harness.call_ok(
        "queue.update",
        json!({"revision": current, "accountId": "A", "enabled": false}),
    );
    let account = updated["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .find(|account| account["accountId"] == json!("A"))
        .expect("account A")
        .clone();
    assert_eq!(account["enabled"], json!(false));
    let pair = harness
        .pairs()
        .into_iter()
        .find(|pair| pair["accountId"] == json!("A"))
        .expect("pair A");
    // 没有联系记录的组合退役后回到 unknown，不伪造静置时间。
    assert_eq!(pair["state"], json!("unknown"));
    assert_eq!(pair["restUntil"], Value::Null);

    let current = revision(&harness);
    harness.call_ok(
        "queue.update",
        json!({"revision": current, "accountId": "A", "enabled": true}),
    );
    let account = harness.call_ok("read", json!({}))["accountQueue"]["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .find(|account| account["accountId"] == json!("A"))
        .expect("account A")
        .clone();
    assert_eq!(account["enabled"], json!(true));
}

/// 额度与停用只通过队列视图体现原因字段；这里确认视图中的布尔与空值形状。
#[test]
fn queue_view_reports_booleans_and_null_reason() {
    let harness = Harness::new("queue-shape", 10_000.0);
    harness.call_ok("queue.update", json!({"revision": 0, "accountIds": ["A"]}));
    let view = harness.call_ok("read", json!({}))["accountQueue"].clone();
    // 注册新账号与本次提交各使 revision 加一。
    assert_eq!(view["revision"], json!(2));
    assert_eq!(
        view["accounts"][0],
        json!({"accountId": "A", "enabled": true, "quotaExhausted": false, "reason": null})
    );
}
