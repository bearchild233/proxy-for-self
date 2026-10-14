//! `route.select` 的行为测试：由旧网关测试 `shared_pool_queue_controls_selection_with_scope_and_quota_guards`、
//! `pool_pinned_continuation_yields_to_active_window_account` 与 `mixed_key_...` 中属于选号的部分移植而来。
//! 插件服务是真实进程内的 Unix socket 服务，内核能力由模拟内核即时应答。

mod common;
mod hooks_support;

use common::object_of;
use gateway_pool::dispatch;
use hooks_support::{ACCOUNT, FakeAccount, Rig};
use plugin_sdk::protocol::*;
use serde_json::json;
use std::time::Duration;

const SECOND: &str = "acct_queue_second";
const HOLDER: &str = "acct_window_holder";

fn blocked_codes(result: &RouteSelectResult) -> Vec<(String, String)> {
    result
        .blocked
        .iter()
        .map(|item| (item.account_id.clone(), item.reason.code.clone()))
        .collect()
}

/// 队首由队列顺序决定（优先级不是排队），停用、额度耗尽的账号不在 preferred 里并给出原因。
#[tokio::test]
async fn queue_order_disabled_and_exhausted_accounts_control_the_preferred_list() {
    // 队列顺序：SECOND 在前，即使它的权重更低也排在最前。
    let rig = Rig::start("select-order", json!({})).await;
    rig.fake.add_account(SECOND, FakeAccount::normal());
    let first = rig.select("req_a", &[ACCOUNT, SECOND], false, None).await;
    assert_eq!(first.preferred, vec![ACCOUNT, SECOND]);
    let update =
        object_of(json!({"revision": rig.queue_revision(), "accountIds": [SECOND, ACCOUNT]}));
    dispatch(&rig.store, "queue.update", &update).expect("reorder");
    let reordered = rig.select("req_b", &[ACCOUNT, SECOND], false, None).await;
    assert_eq!(reordered.preferred, vec![SECOND, ACCOUNT]);
    assert!(reordered.blocked.is_empty());

    // 队列里停用 SECOND：只剩 ACCOUNT，SECOND 带原因码。
    let update =
        object_of(json!({"revision": rig.queue_revision(), "accountId": SECOND, "enabled": false}));
    dispatch(&rig.store, "queue.update", &update).expect("disable");
    let disabled = rig.select("req_c", &[ACCOUNT, SECOND], false, None).await;
    assert_eq!(disabled.preferred, vec![ACCOUNT]);
    assert_eq!(
        blocked_codes(&disabled),
        vec![(SECOND.to_owned(), "queue_disabled".to_owned())]
    );
    assert!(disabled.blocked[0].reason.message.contains("停用"));
}

/// Key 的授权范围外的账号不会进入候选，也不会被登记进队列。
#[tokio::test]
async fn accounts_outside_the_key_scope_never_enter_the_queue() {
    let rig = Rig::start("select-scope", json!({})).await;
    rig.fake.add_account(SECOND, FakeAccount::normal());
    let result = rig.select("req_scope", &[ACCOUNT], false, None).await;
    assert_eq!(result.preferred, vec![ACCOUNT]);
    let asked = rig.fake.calls_to("accounts.read");
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0]["ids"], json!([ACCOUNT]));
    let queued = rig.query("SELECT account FROM gp_account_queue");
    assert_eq!(queued, vec![vec![json!(ACCOUNT)]]);
}

/// 额度已耗尽的账号（调度信号排名最低且快照确认耗尽）被挡在 preferred 外。
#[tokio::test]
async fn exhausted_quota_blocks_the_account() {
    let rig = Rig::start("select-quota", json!({})).await;
    rig.fake.add_account(SECOND, FakeAccount::normal());
    rig.fake.quota_mut(SECOND, |quota| {
        quota.exhausted = true;
        quota.rank = Some(0);
    });
    let result = rig
        .select("req_quota", &[SECOND, ACCOUNT], false, None)
        .await;
    assert_eq!(result.preferred, vec![ACCOUNT]);
    assert_eq!(
        blocked_codes(&result),
        vec![(SECOND.to_owned(), "quota_exhausted".to_owned())]
    );

    // 快照说耗尽但排名不是最低：旧路径不读快照，账号仍可调度（资格检查在 route.prepare 里再判断）。
    rig.fake.quota_mut(SECOND, |quota| quota.rank = Some(3));
    let ranked = rig
        .select("req_rank", &[SECOND, ACCOUNT], false, None)
        .await;
    assert!(ranked.preferred.contains(&SECOND.to_owned()));

    // 账号状态本身是额度耗尽：不必读快照。
    rig.fake
        .account_mut(SECOND, |account| account.status = "quota_exhausted".into());
    let status = rig
        .select("req_status", &[SECOND, ACCOUNT], false, None)
        .await;
    assert_eq!(status.preferred, vec![ACCOUNT]);
}

/// 池只支持 OAuth 账号；内核已查不到的账号按已删除处理。两者都不会登记进队列。
#[tokio::test]
async fn non_oauth_and_missing_accounts_are_blocked_with_reasons() {
    let rig = Rig::start("select-kinds", json!({})).await;
    rig.fake.add_account(
        SECOND,
        FakeAccount {
            kind: "api_key".into(),
            ..FakeAccount::normal()
        },
    );
    let result = rig
        .select("req_kinds", &[ACCOUNT, SECOND, "acct_gone"], false, None)
        .await;
    assert_eq!(result.preferred, vec![ACCOUNT]);
    assert_eq!(
        blocked_codes(&result),
        vec![
            (SECOND.to_owned(), "not_oauth".to_owned()),
            ("acct_gone".to_owned(), "account_missing".to_owned())
        ]
    );
    let queued = rig.query("SELECT account FROM gp_account_queue");
    assert_eq!(queued, vec![vec![json!(ACCOUNT)]]);
}

/// 原生续接固定在原账号上时的去留：别的账号持窗则让位，原账号自己持窗或没有退路则留下。
#[tokio::test]
async fn pinned_continuation_yields_to_active_window_account() {
    // 窗口在同一 Key 的另一个账号上：让位。
    let rig = Rig::start("pinned-yield", json!({})).await;
    rig.fake.add_account(HOLDER, FakeAccount::normal());
    rig.seed_ready(HOLDER, "unified-142", "holder-generation");
    let yielded = rig
        .select("req_p1", &[ACCOUNT, HOLDER], false, Some(ACCOUNT))
        .await;
    assert_eq!(yielded.live, vec![HOLDER]);
    assert_eq!(yielded.preferred, vec![HOLDER, ACCOUNT]);
    assert_eq!(yielded.pinned_decision, Some(PinnedDecision::Escape));

    // 原账号自己持窗：留在原账号。
    let rig = Rig::start("pinned-stay", json!({})).await;
    rig.fake.add_account(HOLDER, FakeAccount::normal());
    rig.seed_ready(ACCOUNT, "unified-142", "own-generation");
    let stay = rig
        .select("req_p2", &[ACCOUNT, HOLDER], false, Some(ACCOUNT))
        .await;
    assert_eq!(stay.live, vec![ACCOUNT]);
    assert_eq!(stay.pinned_decision, Some(PinnedDecision::Stay));

    // Key 里只有这一个降智账号：没有退路，不让位（网关全部静置，由路由阶段给出具体原因）。
    let rig = Rig::start("pinned-alone", json!({"gateways": ["unified-142"]})).await;
    rig.seed_cooling(ACCOUNT, "unified-142", 720.0);
    let alone = rig.select("req_p3", &[ACCOUNT], false, Some(ACCOUNT)).await;
    assert!(alone.preferred.is_empty());
    assert_eq!(
        blocked_codes(&alone),
        vec![(ACCOUNT.to_owned(), "all_resting".to_owned())]
    );
    assert_eq!(alone.pinned_decision, Some(PinnedDecision::Stay));

    // 同样的处境但 Key 里还有普通账号可作退路：让位。
    let rig = Rig::start("pinned-ordinary", json!({"gateways": ["unified-142"]})).await;
    rig.seed_cooling(ACCOUNT, "unified-142", 720.0);
    let ordinary = rig.select("req_p4", &[ACCOUNT], true, Some(ACCOUNT)).await;
    assert_eq!(ordinary.pinned_decision, Some(PinnedDecision::Escape));

    // 没有续接固定账号时不给去留。
    let none = rig.select("req_p5", &[ACCOUNT], true, None).await;
    assert_eq!(none.pinned_decision, None);
}

/// 内核能力读取失败时整体不可用，让内核按“插件不可用”让本插件的账号失败关闭。
#[tokio::test]
async fn capability_failures_make_selection_unavailable() {
    let rig = Rig::start("select-fail", json!({})).await;
    for method in ["accounts.read", "quota.read"] {
        rig.fake
            .failing
            .lock()
            .expect("failing")
            .insert(method.to_owned());
        let error = rig
            .kernel
            .call::<_, RouteSelectResult>(
                "route.select",
                &RouteSelectParams {
                    request_id: "req_fail".into(),
                    model: "gpt-6-astra".into(),
                    accounts: vec![ACCOUNT.into()],
                    ordinary_available: false,
                    pinned: None,
                },
                Duration::from_secs(5),
            )
            .await
            .expect_err("selection must fail");
        assert_eq!(error.code, UNAVAILABLE, "{method}");
        rig.fake.failing.lock().expect("failing").remove(method);
    }
}
