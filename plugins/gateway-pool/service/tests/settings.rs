//! 设置校验与配置副作用，移植自 `test_gateway_pool.py` 中与设置、存储相关的用例。

mod common;

use common::{Harness, merge, message};
use serde_json::json;

/// 对应 `test_dynamic_group_settings_require_two_different_groups_inside_the_pool_scope`。
#[test]
fn dynamic_group_settings_require_two_different_groups_inside_the_pool_scope() {
    let harness = Harness::new("dynamic-groups", 10_000.0);
    let base = json!({"groupIds": ["grp_degraded"]});
    for bad in [
        json!({"dynamicGroups": true}),
        json!({"dynamicGroups": true, "normalGroupId": "grp_normal"}),
        json!({"dynamicGroups": true, "normalGroupId": "grp_degraded", "degradedGroupId": "grp_degraded"}),
        json!({"dynamicGroups": true, "normalGroupId": "grp_normal", "degradedGroupId": "grp_other"}),
        json!({"dynamicGroups": "yes", "normalGroupId": "grp_normal", "degradedGroupId": "grp_degraded"}),
        json!({"normalGroupId": "bad id"}),
    ] {
        assert!(harness.call("configure", merge(base.clone(), bad)).is_err());
    }
    let message_for_missing_pair = message(harness.call(
        "configure",
        merge(base.clone(), json!({"dynamicGroups": true})),
    ));
    assert_eq!(
        message_for_missing_pair,
        "动态分组需要指定不同的正常分组与降智分组"
    );
    let saved = harness.call_ok(
        "configure",
        merge(
            base,
            json!({"dynamicGroups": true, "normalGroupId": "grp_normal", "degradedGroupId": "grp_degraded"}),
        ),
    );
    assert_eq!(saved["dynamicGroups"], json!(true));
}

/// 对应 `test_member_identity_mapping_accepts_complete_upstream_member`。
#[test]
fn member_identity_mapping_accepts_complete_upstream_member() {
    let harness = Harness::new("member-mapping", 10_000.0);
    let identity = "gwpool-member:account-uuid/user.member_1";
    harness.call_ok("configure", json!({"poolAccountIds": {"A": identity}}));
    let settings = harness.call_ok("read", json!({}))["settings"].clone();
    assert_eq!(settings["poolAccountIds"]["A"], json!(identity));
    assert_eq!(
        message(harness.call(
            "configure",
            json!({"poolAccountIds": {"A": "member\ninvalid"}})
        )),
        "池账号映射无效"
    );
}

/// 对应 `test_probe_interval_stays_integer_and_default_is_50ms` 中的设置部分。
#[test]
fn probe_interval_stays_integer_and_default_is_50ms() {
    let harness = Harness::new("probe-interval", 10_000.0);
    let settings = harness.call_ok("read", json!({}))["settings"].clone();
    assert_eq!(settings["probeIntervalMs"], json!(50));
    let saved = harness.call_ok("configure", json!({"probeIntervalMs": 50}));
    assert!(saved["probeIntervalMs"].is_i64());
    assert_eq!(
        message(harness.call("configure", json!({"probeIntervalMs": 50.5}))),
        "候选间隔必须是整数毫秒"
    );
}

/// 对应 `test_maximum_backoff_and_configuration_is_not_reset` 中的配置校验部分。
#[test]
fn invalid_configuration_is_rejected_with_the_original_messages() {
    let harness = Harness::new("config-invalid", 10_000.0);
    let cases = [
        (
            json!({"initialRestSeconds": 90000}),
            "initialRestSeconds无效",
        ),
        (
            json!({"poolUrl": "http://example.com"}),
            "池服务地址必须是 HTTPS 根地址",
        ),
        (
            json!({"maximumRestSeconds": 1, "initialRestSeconds": 2}),
            "静置范围或模式无效",
        ),
        (json!({"gateways": ["bad"]}), "gateways无效"),
        (json!({"poolToken": "a\nb"}), "池服务凭证无效"),
        (json!({"unknownField": 1}), "网关池设置字段无效"),
        (json!({"mode": "sometimes"}), "静置范围或模式无效"),
        (json!({"probeModel": "claude-x"}), "打票模型无效"),
        (json!({"poolDirect": "yes"}), "池服务直连设置无效"),
        (json!({"windowSeconds": true}), "windowSeconds无效"),
    ];
    for (data, expected) in cases {
        assert_eq!(message(harness.call("configure", data)), expected);
    }
}

/// 池服务地址与 `urlsplit` 的判定一致：只接受 https 根地址。
#[test]
fn pool_url_accepts_only_https_roots() {
    let harness = Harness::new("pool-url", 10_000.0);
    for url in [
        "https://pool.example.com",
        "https://pool.example.com/",
        "HTTPS://pool.example.com:8443",
        "https://pool.example.com?",
    ] {
        harness.call_ok("configure", json!({"poolUrl": url}));
    }
    for url in [
        "https://",
        "https://user:pass@pool.example.com",
        "https://pool.example.com/path",
        "https://pool.example.com/?x=1",
        "https://pool.example.com/#frag",
        "http://pool.example.com",
    ] {
        assert_eq!(
            message(harness.call("configure", json!({"poolUrl": url}))),
            "池服务地址必须是 HTTPS 根地址",
            "{url} should be rejected"
        );
    }
}

/// 对应 `test_restart_keeps_cooldown_configuration_and_samples` 中与存储相关的部分：
/// 重启后设置保留，凭据不出现在页面读取中，只暴露是否已配置。
#[test]
fn configuration_survives_restart_and_never_exposes_the_token() {
    let mut harness = Harness::new("restart-config", 10_000.0);
    harness.call_ok(
        "configure",
        json!({"initialRestSeconds": 1800, "poolToken": "fake-pool-secret"}),
    );
    harness.reopen();
    let visible = harness.call_ok("read", json!({}));
    assert!(!visible.to_string().contains("fake-pool-secret"));
    assert_eq!(visible["settings"]["poolTokenConfigured"], json!(true));
    assert_eq!(
        visible["settings"]["initialRestSeconds"].as_f64(),
        Some(1800.0)
    );
    assert_eq!(visible["observations"], json!([]));
}

/// 限定网关变化时，不在范围内的服务中组合立即退役，未开始的候选轮次被清空。
#[test]
fn removing_a_gateway_retires_serving_pairs_outside_the_new_scope() {
    let harness = Harness::new("gateway-scope", 10_000.0);
    harness.call_ok(
        "configure",
        json!({"gateways": ["unified-142", "unified-143"]}),
    );
    harness.seed_pair(
        "A",
        "unified-143",
        json!({"accountId": "A", "gateway": "unified-143", "state": "ready", "inFlight": 0,
               "generation": "g1", "firstContactAt": 9000.0, "lastContactAt": 9500.0, "restSeconds": 3600.0}),
    );
    harness.seed_pair(
        "A",
        "unified-142",
        json!({"accountId": "A", "gateway": "unified-142", "state": "ready", "inFlight": 0,
               "generation": "g2", "firstContactAt": 9000.0, "lastContactAt": 9500.0, "restSeconds": 3600.0}),
    );
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    let pairs = harness.pairs();
    let removed = pairs
        .iter()
        .find(|pair| pair["gateway"] == json!("unified-143"))
        .expect("removed pair stays stored");
    assert_eq!(removed["state"], json!("cooling"));
    assert_eq!(removed["retiredReason"], json!("configuration"));
    let kept = pairs
        .iter()
        .find(|pair| pair["gateway"] == json!("unified-142"))
        .expect("kept pair stays stored");
    assert_eq!(kept["state"], json!("ready"));
}

/// 归属：ownedGroups 来自 groupIds；动态分组开启时 watchedGroups 为正常分组，否则为空。
/// 实际广播由 `GatewayPoolService` 通过内核句柄完成，这里只验证计算结果。
#[test]
fn ownership_follows_group_scope_and_the_dynamic_normal_group() {
    let harness = Harness::new("ownership", 10_000.0);
    let (_, ownership) = gateway_pool::dispatch(
        &harness.store,
        "configure",
        &common::object_of(json!({"groupIds": ["g1", "g2"]})),
    )
    .expect("configure");
    let ownership = ownership.expect("configure reports ownership");
    assert_eq!(ownership.owned_groups, vec!["g1", "g2"]);
    assert!(ownership.watched_groups.is_empty());

    let (_, ownership) = gateway_pool::dispatch(
        &harness.store,
        "configure",
        &common::object_of(
            json!({"dynamicGroups": true, "normalGroupId": "g1", "degradedGroupId": "g2"}),
        ),
    )
    .expect("configure dynamic groups");
    // 默认监控全部普通账号：正常分组之外再登记 `*`。
    assert_eq!(
        ownership.expect("ownership").watched_groups,
        vec!["g1", "*"]
    );
    let (_, ownership) = gateway_pool::dispatch(
        &harness.store,
        "configure",
        &common::object_of(json!({"watchScope": "normal", "unwatchedGroupIds": ["g3"]})),
    )
    .expect("configure watch scope");
    assert_eq!(
        ownership.expect("ownership").watched_groups,
        vec!["g1", "!g3"]
    );
    let (_, ownership) = gateway_pool::dispatch(
        &harness.store,
        "configure",
        &common::object_of(json!({"watchScope": "all"})),
    )
    .expect("configure watch scope");
    assert_eq!(
        ownership.expect("ownership").watched_groups,
        vec!["g1", "*", "!g3"]
    );

    let (_, ownership) =
        gateway_pool::dispatch(&harness.store, "read", &common::object_of(json!({})))
            .expect("read");
    assert!(ownership.is_none(), "reads never change ownership");
}

/// 退避设置：默认加性、步长 3600 秒；方式只接受两种，步长是 60–86400 的整数秒。
#[test]
fn backoff_settings_default_to_additive_and_validate_mode_and_step() {
    let harness = Harness::new("backoff-settings", 10_000.0);
    let settings = harness.view(json!({}))["settings"].clone();
    assert_eq!(settings["backoffMode"], json!("additive"));
    assert_eq!(settings["backoffStepSeconds"], json!(3600));
    let saved = harness.call_ok(
        "configure",
        json!({"backoffMode": "multiplicative", "backoffStepSeconds": 600.0}),
    );
    assert_eq!(saved["backoffMode"], json!("multiplicative"));
    assert_eq!(saved["backoffStepSeconds"], json!(600));
    assert_eq!(
        message(harness.call("configure", json!({"backoffMode": "linear"}))),
        "退避方式无效"
    );
    for bad in [
        json!(59),
        json!(86401),
        json!(90.5),
        json!("3600"),
        json!(true),
        json!(null),
    ] {
        assert_eq!(
            message(harness.call("configure", json!({"backoffStepSeconds": bad}))),
            "退避步长必须是 60 到 86400 的整数秒"
        );
    }
    // 边界值。
    for ok in [60, 86400] {
        harness.call_ok("configure", json!({"backoffStepSeconds": ok}));
    }
}
