//! 统计与页面读取的参数校验及空库形状，移植自 `test_gateway_pool_history.py` 中的查询校验用例，
//! 并补充空库下的返回结构。依赖运行状态机写入的统计数据由 009 移植，这里不构造。

mod common;

use common::{Harness, message};
use gateway_pool::Failure;
use serde_json::{Value, json};

/// 对应 `test_invalid_queries_rejected_and_private_material_not_exposed` 中的参数校验部分。
#[test]
fn invalid_statistics_queries_are_rejected_before_reading() {
    let harness = Harness::new("stats-invalid", 10_000.0);
    let cases = [
        (json!({"from": "x"}), "统计时间范围无效"),
        (json!({"from": 9000, "to": 8000}), "统计时间范围无效"),
        (json!({"to": 10061}), "统计时间范围无效"),
        (json!({"page": true}), "分页参数无效"),
        (json!({"pageSize": 101}), "分页参数无效"),
        (json!({"view": "secret"}), "统计视图无效"),
        (
            json!({"view": "cooling", "status": "bogus"}),
            "恢复状态无效",
        ),
        (json!({"accountId": 3}), "筛选参数无效"),
        (json!({"gateway": "x".repeat(161)}), "筛选参数无效"),
    ];
    for (query, expected) in cases {
        assert_eq!(
            message(harness.call("history.read", query.clone())),
            expected,
            "query {query} should be rejected"
        );
    }
}

/// 管理方法名与页面能力的映射；未注册的方法返回 not_supported。
#[test]
fn unknown_management_method_is_not_supported() {
    let harness = Harness::new("stats-method", 10_000.0);
    match harness.call("history.unknown", json!({})) {
        Err(Failure::NotSupported(method)) => assert_eq!(method, "history.unknown"),
        other => panic!("expected not supported, got {other:?}"),
    }
}

/// 空库统计的返回结构与 Python 实现一致：整数零、空列表、冷却分析为空。
#[test]
fn empty_statistics_have_the_page_shape() {
    let harness = Harness::new("stats-empty", 10_000.0);
    let view = harness.call_ok("history.read", json!({}));
    assert_eq!(view["version"], json!(2));
    assert_eq!(view["collectionGaps"], json!(0));
    assert_eq!(view["recordedSince"].as_f64(), Some(10_000.0));
    assert_eq!(view["from"].as_f64(), Some(10_000.0));
    assert_eq!(view["to"].as_f64(), Some(10_000.0));
    assert_eq!(
        view["page"],
        json!({"number": 1, "size": 20, "total": 0, "pages": 1})
    );
    assert_eq!(view["summary"]["businessSeconds"], json!(0));
    assert_eq!(view["summary"]["requests"], json!(0));
    assert_eq!(view["summary"]["records"], json!(0));
    assert_eq!(view["summary"]["incomplete"], json!(0));
    assert_eq!(view["gateways"], json!([]));
    assert_eq!(view["coolingAnalysis"], Value::Null);
    assert_eq!(view["records"], json!([]));
    assert_eq!(view["batches"], json!([]));
    assert_eq!(view["recoveries"], json!([]));
    assert_eq!(
        view["options"],
        json!({"accounts": [], "gateways": [], "batches": []})
    );
}

/// 汇总视图在空库上返回五个固定分桶与空中位数。
#[test]
fn summary_cooling_analysis_on_empty_database_has_fixed_bins() {
    let harness = Harness::new("stats-summary", 10_000.0);
    let view = harness.call_ok("history.read", json!({"view": "summary", "pageSize": 1}));
    let analysis = &view["coolingAnalysis"];
    assert_eq!(analysis["total"], json!(0));
    assert_eq!(analysis["gateways"], json!(0));
    assert_eq!(analysis["missingInterval"], json!(0));
    assert_eq!(analysis["recoveredMedianSeconds"], Value::Null);
    let labels: Vec<&str> = analysis["bins"]
        .as_array()
        .expect("bins")
        .iter()
        .map(|bin| bin["label"].as_str().expect("label"))
        .collect();
    assert_eq!(
        labels,
        vec!["不足 2h", "2–4h", "4–8h", "8–16h", "16h 及以上"]
    );
    assert_eq!(view["batches"], json!([]));
}

/// 页面读取的组合与观察列表在空库上为空，且不返回凭据或材料字段。
#[test]
fn page_read_on_empty_database_hides_private_material() {
    let harness = Harness::new("read-empty", 10_000.0);
    let view = harness.call_ok("read", json!({"accountId": "A", "limit": 5}));
    assert_eq!(view["pairs"], json!([]));
    assert_eq!(view["observations"], json!([]));
    assert_eq!(view["switches"], json!([]));
    assert_eq!(view["catalogs"], json!({}));
    assert_eq!(view["groupEvents"], json!([]));
    let text = view.to_string();
    // 只匹配带引号的键名：poolTokenConfigured 是允许出现的布尔标记。
    for field in ["cookie", "poolToken", "material", "turn-state"] {
        assert!(
            !text.contains(&format!("\"{field}\"")),
            "{field} must not be exposed"
        );
    }
}
