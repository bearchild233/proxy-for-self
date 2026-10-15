//! 历史与统计用例，移植自 `services/plugin_host/test_gateway_pool_history.py`。
//! 初始状态沿用 setUp：网关 142/143，初始静置 1 秒，最大静置 8 秒，时钟从 10000.0 起。

mod common;

use std::time::Duration;

use common::{Harness, assert_num};
use serde_json::{Value, json};

fn catalog(gateways: &[&str]) -> Value {
    Value::Array(
        gateways
            .iter()
            .map(|gateway| json!({"gateway": gateway, "validForS": 600}))
            .collect(),
    )
}

fn only_142(harness: &Harness) {
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
}

fn first_record(harness: &Harness) -> Value {
    harness.stats(json!({}))["records"][0].clone()
}

fn batch_status(harness: &Harness) -> Value {
    harness.stats(json!({"view": "summary"}))["batches"][0]["status"].clone()
}

#[test]
fn headers_do_not_finish_business_and_overlap_is_merged() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (lease, a) = harness.business(&pair);
    harness.advance(1.0);
    harness.report(
        "businessResult",
        &lease,
        &a,
        false,
        json!({"status": 200, "stateReturned": false, "paired": true, "gatewayMatches": true}),
    );
    let (lease2, b) = harness.business(&pair);
    harness.finish_access(&lease, &a, 1.0);
    harness.finish_access(&lease2, &b, 1.0);
    let row = first_record(&harness);
    assert_num(&row["businessSeconds"], 3.0);
    assert_num(&row["requestSeconds"], 4.0);
    assert_eq!(row["requests"], json!(2));
    assert_eq!(row["segments"].as_array().map(Vec::len), Some(1));
    assert_eq!(row["incomplete"], json!(false));
}

#[test]
fn ticket_is_separate_and_finish_timestamp_excludes_rpc_delay() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let lease = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 100.0}),
    )["leaseId"]
        .clone();
    let ticket = harness.send(&lease, "ticket");
    harness.advance(3.0);
    harness.report(
        "ticketResult",
        &lease,
        &ticket,
        true,
        json!({"model": "gpt-6-astra", "status": 200}),
    );
    let business = harness.send(&lease, "business");
    harness.advance(5.0);
    let mut ended = business;
    ended["elapsedMs"] = json!(5000);
    ended["ended"] = json!(true);
    ended["outcome"] = json!("completed");
    harness.advance(7.0);
    harness.rt("finish", json!({"leaseId": lease, "access": ended}));
    let row = first_record(&harness);
    assert_num(&row["businessSeconds"], 5.0);
    assert_num(&row["ticketSeconds"], 3.0);
}

#[test]
fn two_hour_boundary_persists_segments_without_retiring_window() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (lease, a) = harness.business(&pair);
    harness.finish_access(&lease, &a, 10.0);
    harness.advance(7199.0);
    let (lease, a) = harness.business(&pair);
    harness.finish_access(&lease, &a, 10.0);
    assert_eq!(
        first_record(&harness)["segments"].as_array().map(Vec::len),
        Some(1)
    );
    let last = harness.now();
    harness.advance(7200.0);
    harness.stats(json!({}));
    let (closed, last_end): (f64, f64) = harness
        .connection()
        .query_row("SELECT closed,last_end FROM gp_segments", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .expect("segment row");
    assert!((closed - (last + 7200.0)).abs() < 1e-6);
    assert!((last_end - last).abs() < 1e-6);
    let (lease, a) = harness.business(&pair);
    harness.finish_access(&lease, &a, 10.0);
    let row = first_record(&harness);
    assert_eq!(row["segments"].as_array().map(Vec::len), Some(2));
    assert_eq!(row["retiredAt"], Value::Null);
}

#[test]
fn inflight_prevents_idle_closure() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (lease, a) = harness.business(&pair);
    harness.advance(7300.0);
    assert_eq!(first_record(&harness)["segments"], json!([]));
    harness.finish_access(&lease, &a, 1.0);
    let row = first_record(&harness);
    assert_eq!(row["segments"].as_array().map(Vec::len), Some(1));
    assert_eq!(row["segments"][0]["closedAt"], Value::Null);
}

#[test]
fn repeat_candidate_stays_in_same_batch_and_restart_keeps_number() {
    let mut harness = Harness::history();
    harness.probe("29", json!({}));
    harness.advance(2.0);
    let repeated = harness.probe("29", json!({"catalog": catalog(&["unified-142"])}));
    assert_eq!(repeated["gateway"], json!("unified-142"));
    let batch = harness.stats(json!({"view": "summary"}))["batches"][0].clone();
    assert_eq!(batch["number"], json!(1));
    assert_eq!(batch["status"], json!("active"));
    assert_eq!(batch["candidates"].as_array().map(Vec::len), Some(2));
    harness.reopen();
    harness.advance(5.0);
    harness.probe("29", json!({"catalog": catalog(&["unified-143"])}));
    assert_eq!(batch_status(&harness), json!("complete"));
    harness.advance(10.0);
    harness.probe(
        "29",
        json!({"catalog": catalog(&["unified-142", "unified-143"])}),
    );
    assert_eq!(
        harness.stats(json!({"view": "summary"}))["batches"][0]["number"],
        json!(2)
    );
}

#[test]
fn retirement_drains_before_batch_and_cooling() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (lease, a) = harness.business(&pair);
    harness.advance(181.0);
    harness.probe("29", json!({}));
    harness.retire_pair(&pair);
    assert_eq!(batch_status(&harness), json!("active"));
    harness.finish_access(&lease, &a, 3.0);
    assert_eq!(batch_status(&harness), json!("complete"));
    let records = harness.stats(json!({}))["records"].clone();
    let row = records
        .as_array()
        .expect("records")
        .iter()
        .find(|row| row["id"] == pair["generation"])
        .expect("window record")
        .clone();
    assert!(
        row["drainedAt"].as_f64().expect("drained") > row["retiredAt"].as_f64().expect("retired")
    );
}

#[test]
fn recovery_requires_actual_21_and_records_each_tier() {
    let harness = Harness::history();
    harness.probe("29", json!({}));
    harness.advance(2.0);
    let cycle = harness.stats(json!({"view": "cooling"}))["recoveries"][0].clone();
    assert_eq!(cycle["state"], json!("due"));
    assert_eq!(cycle["recoveredAt"], Value::Null);
    harness.probe("29", json!({"catalog": catalog(&["unified-142"])}));
    harness.advance(3.0);
    harness.probe("21", json!({"catalog": catalog(&["unified-142"])}));
    let cycle = harness.stats(json!({"view": "cooling"}))["recoveries"][0].clone();
    assert_eq!(cycle["state"], json!("recovered"));
    let steps = cycle["steps"].as_array().expect("steps");
    assert_eq!(
        steps
            .iter()
            .map(|step| step["plannedSeconds"].as_f64())
            .collect::<Vec<_>>(),
        vec![Some(1.0), Some(2.0)]
    );
    assert_eq!(
        steps
            .iter()
            .map(|step| step["result"].clone())
            .collect::<Vec<_>>(),
        vec![json!("29"), json!("21")]
    );
}

#[test]
fn no_contact_failure_does_not_create_cooling_or_complete_candidate() {
    let harness = Harness::history();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "failed": true}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    assert_eq!(
        harness.stats(json!({"view": "cooling"}))["recoveries"],
        json!([])
    );
    assert_eq!(
        harness.stats(json!({"view": "summary"}))["batches"][0]["processed"],
        json!({})
    );
}

#[test]
fn recovery_learning_and_no_contact_retry_keep_original_cooling() {
    let harness = Harness::history();
    only_142(&harness);
    harness.probe("29", json!({}));
    harness.advance(2.0);
    harness.probe("21", json!({}));
    let ready = harness.view(json!({}))["pairs"][0].clone();
    harness.retire_pair(&ready);
    harness.advance(2.0);
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "failed": true}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    let cycles = harness.stats(json!({"view": "cooling"}))["recoveries"].clone();
    assert_eq!(cycles.as_array().map(Vec::len), Some(2));
    assert_eq!(cycles[0]["steps"].as_array().map(Vec::len), Some(1));
    harness.advance(6.0);
    harness.probe("21", json!({}));
    let cycle = harness.stats(json!({"view": "cooling"}))["recoveries"][0].clone();
    let steps = cycle["steps"].as_array().expect("steps");
    assert_num(&steps[steps.len() - 1]["learnedRestSeconds"], 1.0);
}

#[test]
fn legacy_and_expired_access_are_unknown_not_infinite_duration() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    harness.business(&pair);
    harness.advance(20001.0);
    harness.rt("active", json!({}));
    let row = first_record(&harness);
    assert_eq!(row["businessSeconds"], Value::Null);
    assert_eq!(row["incomplete"], json!(true));
    assert_num(&row["knownBusinessSeconds"], 0.0);
}

#[test]
fn duplicate_finish_is_idempotent_and_filters_keep_full_totals() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (lease, a) = harness.business(&pair);
    let start = harness.now();
    harness.finish_access(&lease, &a, 10.0);
    harness.report("finish", &lease, &a, true, json!({}));
    let row = harness.stats(json!({"from": start + 2.0, "to": start + 7.0}))["records"][0].clone();
    assert_num(&row["businessSeconds"], 5.0);
    assert_eq!(row["requests"], json!(0));
    assert_eq!(row["partial"], json!(true));
    assert_eq!(
        harness.stats(json!({"accountId": "missing"}))["page"]["total"],
        json!(0)
    );
    assert_num(
        &harness.stats(json!({"gateway": "unified-143"}))["summary"]["businessSeconds"],
        0.0,
    );
}

/// Python 用补丁让 `history.sync` 抛错；这里让窗口表拒绝写入，使同步与结束记录都失败，
/// 效果相同：租约仍被删除、缺口计数增加，之后的配置变更照常中断批次。
#[test]
fn configuration_interrupts_and_history_failure_does_not_break_finish() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (lease, a) = harness.business(&pair);
    let connection = harness.connection();
    connection
        .execute_batch(
            "CREATE TRIGGER fail_window_write BEFORE INSERT ON gp_windows BEGIN SELECT RAISE(ABORT,'test'); END;",
        )
        .expect("create failure trigger");
    harness.finish_access(&lease, &a, 10.0);
    connection
        .execute_batch("DROP TRIGGER fail_window_write")
        .expect("drop failure trigger");
    assert_eq!(harness.count("leases"), 0);
    assert!(
        harness.stats(json!({}))["collectionGaps"]
            .as_i64()
            .expect("gaps")
            > 0
    );
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    assert_eq!(batch_status(&harness), json!("interrupted"));
}

#[test]
fn read_snapshot_does_not_block_runtime_writer() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let snapshot = rusqlite::Connection::open_with_flags(
        harness.dir.path().join(gateway_pool::DB_FILE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open snapshot");
    snapshot.execute_batch("BEGIN").expect("begin snapshot");
    let _: i64 = snapshot
        .query_row("SELECT COUNT(*) FROM gp_windows", [], |row| row.get(0))
        .expect("read inside snapshot");
    let store = &harness.store;
    let (sender, receiver) = std::sync::mpsc::channel();
    let deadline = harness.now() + 20000.0;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let reserved = store.runtime(&json!({
                "action": "reserve", "accountId": "A", "gateway": pair["gateway"],
                "generation": pair["generation"], "deadline": deadline}));
            let _ = sender.send(reserved);
        });
        let reserved = receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("writer was blocked by the read snapshot")
            .expect("reserve");
        assert_eq!(reserved["reserved"], json!(true));
    });
    snapshot.execute_batch("COMMIT").expect("end snapshot");
}

/// `test_invalid_queries_rejected_and_private_material_not_exposed` 的第二半：运行产生数据后，
/// 统计输出不含任何私有材料字段（第一半的参数校验在 read.rs）。
#[test]
fn statistics_never_expose_private_material() {
    let harness = Harness::history();
    harness.probe("21", json!({}));
    let value = harness.stats(json!({})).to_string();
    for field in ["cookie", "poolToken", "material", "turn-state"] {
        assert!(!value.contains(field), "statistics leaked {field}");
    }
}

fn pair_rows(harness: &Harness) -> Vec<(String, String, String)> {
    let connection = harness.connection();
    let mut statement = connection
        .prepare("SELECT account,gateway,data FROM pairs ORDER BY rowid")
        .expect("prepare");
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

#[test]
fn card_read_after_due_is_passive_and_waits_for_actual_visit() {
    let harness = Harness::history();
    only_142(&harness);
    harness.probe("29", json!({}));
    let last_end = harness.now();
    harness.advance(10.0);
    let before = pair_rows(&harness);
    // 读取页面不得触发运行动作：组合与租约都保持原样。
    let summary = harness.view(json!({"accountId": "A"}))["pairs"][0]["coolingSummary"].clone();
    assert_eq!(summary["visits"], json!([]));
    assert_eq!(summary["current"]["probeAt"], Value::Null);
    assert_num(&summary["current"]["startedAt"], last_end);
    assert_eq!(before, pair_rows(&harness));
    assert_eq!(harness.count("leases"), 0);
    harness.probe("21", json!({}));
    let summary = harness.view(json!({"accountId": "A"}))["pairs"][0]["coolingSummary"].clone();
    assert_eq!(summary["current"], Value::Null);
    assert_num(&summary["visits"][0]["idleSeconds"], 10.0);
    assert_num(&summary["visits"][0]["afterDueSeconds"], 9.0);
    assert_eq!(summary["visits"][0]["result"], json!("21"));
    // 全量读取只带是否有再访问记录，避免响应超过管理连接单帧上限。
    let compact = harness.view(json!({}))["pairs"][0]["coolingSummary"].clone();
    assert_eq!(compact, json!({"recorded": true}));
}

#[test]
fn card_recent_visits_keep_account_scope_and_latest_failed_attempt() {
    let harness = Harness::history();
    only_142(&harness);
    harness.probe("29", json!({}));
    for _ in 0..7 {
        harness.advance(10.0);
        harness.probe("29", json!({}));
    }
    let summary = harness.view(json!({"accountId": "A"}))["pairs"][0]["coolingSummary"].clone();
    assert_eq!(summary["visits"].as_array().map(Vec::len), Some(5));
    assert_eq!(summary["hasMore"], json!(true));
    assert_eq!(summary["visits"][0]["result"], json!("29"));
    assert_eq!(summary["current"]["probeAt"], Value::Null);
    assert_num(&summary["current"]["plannedSeconds"], 8.0);
    let visits = summary["visits"].as_array().expect("visits");
    assert!(
        visits[0]["probeAt"].as_f64().expect("newest")
            > visits[visits.len() - 1]["probeAt"]
                .as_f64()
                .expect("oldest")
    );
    assert_eq!(harness.view(json!({"accountId": "B"}))["pairs"], json!([]));
}

#[test]
fn account_cooling_analysis_uses_all_visits_and_actual_interval_bins() {
    let harness = Harness::history();
    only_142(&harness);
    harness.probe("29", json!({}));
    for _ in 0..7 {
        harness.advance(7201.0);
        harness.probe("29", json!({}));
    }
    harness.advance(14401.0);
    harness.probe("21", json!({}));
    let summary = harness.stats(json!({"view": "summary", "accountId": "A", "pageSize": 1}))["coolingAnalysis"].clone();
    assert_eq!(summary["total"], json!(8));
    assert_eq!(summary["gateways"], json!(1));
    assert_eq!(summary["unrecovered"], json!(7));
    assert_eq!(summary["recovered"], json!(1));
    assert_eq!(summary["bins"][1]["unrecovered"], json!(7));
    assert_eq!(summary["bins"][2]["recovered"], json!(1));
    assert_num(&summary["recoveredMedianSeconds"], 14401.0);
    assert_eq!(
        harness.stats(json!({"view": "summary", "accountId": "B"}))["coolingAnalysis"]["total"],
        json!(0)
    );
    assert_eq!(
        harness.stats(json!({"view": "summary", "gateway": "unified-143"}))["coolingAnalysis"]["total"],
        json!(0)
    );
}

#[test]
fn cooling_analysis_filters_visit_time_and_separates_unknown_outcomes() {
    let harness = Harness::history();
    harness.clock.set(200_000.0);
    // 周期开始在筛选范围外，只有实际再访问起点决定样本归属。
    let mut steps = Vec::new();
    for (probe, idle, result) in [
        (100_000_i64, Some(7199_i64), Some("21")),
        (100_001, Some(7200), Some("29")),
        (100_002, Some(14400), Some("failed")),
        (100_003, Some(28800), None),
        (100_004, None, Some("21")),
        (100_005, Some(57600), Some("21")),
    ] {
        steps.push(json!({
            "probeAt": probe,
            "startedAt": idle.map(|idle| probe - idle),
            "result": result,
            "resultAt": result.map(|_| probe as f64 + 0.5),
        }));
    }
    harness
        .connection()
        .execute(
            "INSERT INTO gp_cooling(account,gateway,started,recovered,data) VALUES (?,?,?,?,?)",
            rusqlite::params![
                "A",
                "unified-142",
                10000,
                Option::<f64>::None,
                json!({"steps": steps}).to_string()
            ],
        )
        .expect("insert cooling cycle");
    let data =
        harness.stats(json!({"view": "summary", "accountId": "A", "from": 100_000, "to": 100_005}))
            ["coolingAnalysis"]
            .clone();
    assert_eq!(data["total"], json!(5));
    assert_eq!(data["missingInterval"], json!(1));
    assert_eq!(data["failed"], json!(1));
    assert_eq!(data["pending"], json!(1));
    let recovered: Vec<Value> = data["bins"]
        .as_array()
        .expect("bins")
        .iter()
        .map(|bin| bin["recovered"].clone())
        .collect();
    assert_eq!(
        recovered,
        vec![json!(1), json!(0), json!(0), json!(0), json!(0)]
    );
    assert_eq!(data["bins"][1]["unrecovered"], json!(1));
    assert_eq!(data["bins"][2]["failed"], json!(1));
    assert_eq!(data["bins"][3]["pending"], json!(1));
    assert_num(&data["recoveredMedianSeconds"], 7199.0);
    let future = harness.stats(json!({"view": "summary", "from": 100_005, "to": 100_005.25}))["coolingAnalysis"].clone();
    assert_eq!(future["pending"], json!(1));
    assert_eq!(future["recovered"], json!(0));
    let end = harness.stats(json!({"view": "summary", "from": 100_005, "to": 100_006}))["coolingAnalysis"].clone();
    assert_eq!(end["bins"][4]["recovered"], json!(1));
}

#[test]
fn rotation_detail_keeps_detection_ticket_and_business_in_exact_generation() {
    let harness = Harness::history();
    only_142(&harness);
    let first = harness.probe("21", json!({}));
    let lease = harness.rt(
        "reserve",
        json!({"gateway": first["gateway"], "generation": first["generation"], "deadline": harness.now() + 100.0}),
    )["leaseId"]
        .clone();
    let ticket = harness.send(&lease, "ticket");
    harness.advance(0.2);
    harness.report(
        "ticketResult",
        &lease,
        &ticket,
        true,
        json!({"model": "gpt-6-astra", "status": 200, "stateReturned": true}),
    );
    let business = harness.send(&lease, "business");
    harness.advance(0.2);
    harness.report(
        "businessResult",
        &lease,
        &business,
        false,
        json!({"status": 200, "stateReturned": false, "paired": true, "gatewayMatches": true}),
    );
    harness.finish_access(&lease, &business, 10.0);
    harness.retire_pair(&first);
    harness.advance(10.0);
    let second = harness.probe("21", json!({}));
    let records = harness.stats(json!({}))["records"].clone();
    let rows: std::collections::BTreeMap<String, Value> = records
        .as_array()
        .expect("records")
        .iter()
        .map(|row| (row["id"].as_str().expect("id").to_owned(), row.clone()))
        .collect();
    let first_row = &rows[first["generation"].as_str().expect("generation")];
    assert_eq!(first_row["probeResult"], json!("21"));
    assert_eq!(first_row["observationCount"], json!(5));
    let sources: std::collections::BTreeSet<&str> = first_row["observations"]
        .as_array()
        .expect("observations")
        .iter()
        .filter_map(|sample| sample["source"].as_str())
        .collect();
    assert_eq!(
        sources,
        ["probe-attempt", "probe", "ticket", "business"]
            .into_iter()
            .collect()
    );
    assert_eq!(
        rows[second["generation"].as_str().expect("generation")]["observationCount"],
        json!(3)
    );
    for (generation, row) in &rows {
        assert!(
            row["observations"]
                .as_array()
                .expect("observations")
                .iter()
                .all(|sample| sample["generation"] == json!(generation))
        );
    }
    assert_eq!(
        harness.stats(json!({"accountId": "B"}))["records"],
        json!([])
    );
}

#[test]
fn rotation_detail_limits_visible_detection_rows_but_keeps_total() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let connection = harness.connection();
    let (at, account, gateway, data): (f64, String, String, String) = connection
        .query_row(
            "SELECT at,account,gateway,data FROM observations ORDER BY id LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("first observation");
    for _ in 0..102 {
        connection
            .execute(
                "INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)",
                rusqlite::params![at, account, gateway, data],
            )
            .expect("copy observation");
    }
    let mut legacy: Value = serde_json::from_str(&data).expect("observation json");
    legacy.as_object_mut().expect("object").remove("generation");
    connection
        .execute(
            "INSERT INTO observations(at,account,gateway,data) VALUES (?,?,?,?)",
            rusqlite::params![at, account, gateway, legacy.to_string()],
        )
        .expect("legacy observation");
    let records = harness.stats(json!({}))["records"].clone();
    let row = records
        .as_array()
        .expect("records")
        .iter()
        .find(|row| row["id"] == pair["generation"])
        .expect("window record")
        .clone();
    assert_eq!(row["observationCount"], json!(105));
    let shown = row["observations"].as_array().expect("observations");
    assert_eq!(shown.len(), 100);
    assert!(
        shown[0]["id"].as_i64().expect("first id")
            > shown[shown.len() - 1]["id"].as_i64().expect("last id")
    );
}

/// 008 的决定：冷却视图遇到缺少 dueAt 的步骤时跳过该步骤而不报错（Python 在这里抛 KeyError）。
#[test]
fn cooling_views_skip_steps_without_a_due_time() {
    let harness = Harness::history();
    only_142(&harness);
    harness.probe("29", json!({}));
    let connection = harness.connection();
    let steps = json!([
        {"probeAt": 9500.0, "startedAt": 9000.0, "result": "29", "resultAt": 9500.5},
        {"probeAt": 9600.0, "startedAt": 9000.0, "dueAt": 9550.0, "result": "29", "resultAt": 9600.5},
    ]);
    connection
        .execute(
            "INSERT INTO gp_cooling(account,gateway,started,recovered,data) VALUES (?,?,?,?,?)",
            rusqlite::params![
                "A",
                "unified-142",
                9000.0,
                Option::<f64>::None,
                json!({"steps": steps}).to_string()
            ],
        )
        .expect("insert cycle with a step lacking dueAt");
    let summary = harness.view(json!({"accountId": "A"}))["pairs"][0]["coolingSummary"].clone();
    let visits = summary["visits"].as_array().expect("visits");
    assert_eq!(visits.len(), 1);
    assert_num(&visits[0]["probeAt"], 9600.0);
    assert_num(&visits[0]["afterDueSeconds"], 50.0);
    // 周期列表：最后一步缺少 dueAt 的周期被跳过，其余周期照常返回。
    let lone = json!([{"probeAt": null, "startedAt": 9000.0, "resultAt": null, "result": null}]);
    connection
        .execute(
            "INSERT INTO gp_cooling(account,gateway,started,recovered,data) VALUES (?,?,?,?,?)",
            rusqlite::params![
                "B",
                "unified-142",
                9000.0,
                Option::<f64>::None,
                json!({"steps": lone}).to_string()
            ],
        )
        .expect("insert cycle for B");
    let cycles = harness.stats(json!({"view": "cooling"}))["recoveries"].clone();
    let accounts: Vec<&str> = cycles
        .as_array()
        .expect("recoveries")
        .iter()
        .filter_map(|cycle| cycle["accountId"].as_str())
        .collect();
    assert!(!accounts.contains(&"B"));
    assert!(accounts.contains(&"A"));
}

/// 一次判定为 21 的业务访问：开始、过了 `seconds` 秒后带 21 判定结束，并归还租约。
fn served_21(harness: &Harness, pair: &Value, seconds: f64) -> f64 {
    let (lease, access) = harness.business(pair);
    harness.advance(seconds);
    harness.report(
        "businessResult",
        &lease,
        &access,
        true,
        json!({"status": 200, "stateReturned": false, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": lease}));
    harness.now()
}

fn record_of(harness: &Harness, generation: &Value) -> Value {
    harness.stats(json!({}))["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|row| &row["id"] == generation)
        .expect("window record")
        .clone()
}

#[test]
fn window_seconds_stop_at_the_last_confirmed_21_instead_of_growing_while_idle() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let first = record_of(&harness, &pair["generation"])["windowStartAt"].clone();
    let confirmed = served_21(&harness, &pair, 10.0);
    // 窗口用满之后没有任何使用：时长止于最后一次确认可用，页面不再按查询时刻加时间。
    harness.advance(5000.0);
    let row = record_of(&harness, &pair["generation"]);
    assert_num(&row["lastConfirmedAt"], confirmed);
    assert_num(&row["windowEndAt"], confirmed);
    assert_num(
        &row["windowSeconds"],
        confirmed - first.as_f64().expect("start"),
    );
    harness.advance(5000.0);
    assert_num(
        &record_of(&harness, &pair["generation"])["windowSeconds"],
        confirmed - first.as_f64().expect("start"),
    );
    // 之后又被使用且仍为 21：从新的确认时刻继续算。
    let later = served_21(&harness, &pair, 20.0);
    let row = record_of(&harness, &pair["generation"]);
    assert_num(
        &row["windowSeconds"],
        later - first.as_f64().expect("start"),
    );
    // 最近 windowSeconds 内有使用：仍算到查询时刻。
    harness.advance(30.0);
    let row = record_of(&harness, &pair["generation"]);
    assert_num(
        &row["windowSeconds"],
        harness.now() - first.as_f64().expect("start"),
    );
    assert_eq!(row["windowEndAt"], Value::Null);
    // 有在途请求：算到查询时刻，即使很久没有 21 判定。
    harness.advance(1000.0);
    harness.business(&pair);
    harness.advance(1000.0);
    let row = record_of(&harness, &pair["generation"]);
    assert_num(
        &row["windowSeconds"],
        harness.now() - first.as_f64().expect("start"),
    );
}

#[test]
fn a_window_ended_by_29_stops_at_the_end_of_the_last_21() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let first = record_of(&harness, &pair["generation"])["windowStartAt"]
        .as_f64()
        .expect("start");
    let confirmed = served_21(&harness, &pair, 10.0);
    harness.advance(600.0);
    let (lease, access) = harness.business(&pair);
    harness.advance(2.0);
    harness.report(
        "businessResult",
        &lease,
        &access,
        true,
        json!({"status": 200, "stateReturned": true, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": lease}));
    let row = record_of(&harness, &pair["generation"]);
    assert_eq!(row["reason"], json!("29"));
    assert!(row["retiredAt"].as_f64().expect("retired") > confirmed + 600.0);
    assert_num(&row["lastConfirmedAt"], confirmed);
    assert_num(&row["windowSeconds"], confirmed - first);
}

#[test]
fn a_standby_window_is_counted_from_its_first_business_access_and_a_dropped_one_has_no_window() {
    let harness = Harness::history();
    let first = harness.probe("21", json!({}));
    harness.advance(181.0);
    // 窗口用满：只准备备用票，当前窗口继续服务。
    let spare = harness.probe("21", json!({}));
    let row = record_of(&harness, &spare["generation"]);
    assert_eq!(row["standby"], json!(true));
    assert_eq!(row["state"], json!("standby"));
    assert_num(&row["standbyAt"], harness.now());
    assert_eq!(row["activatedAt"], Value::Null);
    assert_num(&row["windowSeconds"], 0.0);
    // 备用票不是窗口，不缺失计时。
    assert_eq!(
        harness.stats(json!({}))["summary"]["missing"]["windowSeconds"],
        json!(0)
    );
    let obtained = harness.now();
    // 超过 50 分钟仍未启用：丢弃，结束时间记为应当到期的时刻，原因为备用过期。
    harness.advance(3001.0);
    harness.rt("plan", json!({"demand": true}));
    let row = record_of(&harness, &spare["generation"]);
    assert_eq!(row["reason"], json!("standby_expired"));
    assert_num(&row["retiredAt"], obtained + 3000.0);
    assert_num(&row["windowSeconds"], 0.0);
    // 没有服务过的备用票不算遍历：不进入本批已处理的网关。
    let batch = harness.stats(json!({"view": "summary"}))["batches"][0].clone();
    assert!(
        batch["processed"]
            .get(spare["gateway"].as_str().expect("gateway"))
            .is_none()
    );
    // 静置起点是拿到它的时间。
    let cycle = harness.stats(json!({"view": "cooling"}))["recoveries"][0].clone();
    let step = cycle["steps"][0].clone();
    assert_eq!(step["reason"], json!("standby_expired"));
    assert_num(&step["scheduledStartAt"], obtained);
    // 新的备用票转正：窗口从它的第一次业务访问算起，备用期间不计入。
    harness.advance(2.0);
    let next = harness.probe("21", json!({}));
    assert_eq!(next["gateway"], spare["gateway"]);
    // 转正要求配套材料仍有效（历史用例的探测不带材料，这里补上）。
    harness
        .connection()
        .execute(
            "INSERT INTO materials VALUES ('A',?,?,?)",
            rusqlite::params![
                next["generation"].as_str(),
                harness.now() + 50_000.0,
                json!({"state": "s", "cookie": "c", "expires": harness.now() + 50_000.0})
                    .to_string()
            ],
        )
        .expect("insert material");
    harness.retire_pair(&first);
    let promoted = harness.view(json!({}))["pairs"]
        .as_array()
        .expect("pairs")
        .iter()
        .find(|pair| pair["generation"] == next["generation"])
        .expect("promoted pair")
        .clone();
    assert_eq!(promoted["state"], json!("ready"));
    harness.advance(500.0);
    let row = record_of(&harness, &next["generation"]);
    assert_num(&row["windowSeconds"], 0.0);
    assert_eq!(row["windowStartAt"], Value::Null);
    let started = harness.now();
    let confirmed = served_21(&harness, &promoted, 12.0);
    let row = record_of(&harness, &next["generation"]);
    assert_num(&row["windowStartAt"], started);
    assert_num(&row["windowSeconds"], confirmed - started);
    assert!(row["activatedAt"].as_f64().is_some());
}

#[test]
fn cooling_steps_record_reason_tier_start_tier_and_the_failure_kind() {
    let harness = Harness::history();
    only_142(&harness);
    harness.probe("29", json!({}));
    harness.advance(2.0);
    harness.probe("29", json!({}));
    harness.advance(3.0);
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.advance(1.0);
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 429, "paired": true, "gatewayMatches": true,
               "failed": true, "failure": "http_status"}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    let cycle = harness.stats(json!({"view": "cooling"}))["recoveries"][0].clone();
    let steps = cycle["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 3);
    let field =
        |name: &str| -> Vec<Value> { steps.iter().map(|step| step[name].clone()).collect() };
    assert_eq!(
        field("reason"),
        vec![json!("29"), json!("29"), json!("probe_failed")]
    );
    // 档位：初始 1 秒，复测仍为 29 时翻倍（乘性）；失败的静置不改档。
    assert_eq!(
        field("tierSeconds")
            .iter()
            .map(|v| v.as_f64())
            .collect::<Vec<_>>(),
        vec![Some(1.0), Some(2.0), Some(2.0)]
    );
    assert_eq!(
        field("plannedSeconds")
            .iter()
            .map(|v| v.as_f64())
            .collect::<Vec<_>>(),
        vec![Some(1.0), Some(2.0), Some(300.0)]
    );
    assert_eq!(field("startTierSeconds")[0].as_f64(), Some(1.0));
    // 每一步的结果是这一轮静置结束后复测的结果：第二轮之后的复测失败，状态码与失败类别单独记录。
    assert_eq!(
        field("result"),
        vec![json!("29"), json!("failed"), Value::Null]
    );
    assert_eq!(field("status")[1], json!(429));
    assert_eq!(field("failure")[1], json!("http_status"));
    assert_eq!(field("failure")[0], Value::Null);
}

#[test]
fn startup_marks_orphaned_business_accesses_interrupted_and_leaves_live_leases_alone() {
    let mut harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let (live_lease, live) = harness.business(&pair);
    let now = harness.now();
    let connection = harness.connection();
    for (id, lease) in [("orphan", "gone"), ("expired", "stale")] {
        connection
            .execute(
                "INSERT INTO gp_accesses(id,lease,generation,kind,observed,started,outcome) VALUES (?,?,?,'business',?,?,'headers')",
                rusqlite::params![id, lease, pair["generation"].as_str(), now, now],
            )
            .expect("insert access");
    }
    connection
        .execute(
            "INSERT INTO leases VALUES ('stale',?,?,?,'business',?,1,'{}')",
            rusqlite::params![
                "A",
                pair["gateway"].as_str(),
                pair["generation"].as_str(),
                now - 1.0
            ],
        )
        .expect("insert expired lease");
    drop(connection);
    harness.reopen();
    let connection = harness.connection();
    let outcome = |id: &str| -> (Option<String>, Option<f64>) {
        connection
            .query_row(
                "SELECT outcome,ended FROM gp_accesses WHERE id=?",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("access row")
    };
    // 没有结束时间且不属于在途租约：标为中断，结束时间保持未知。
    assert_eq!(outcome("orphan"), (Some("interrupted".to_owned()), None));
    assert_eq!(outcome("expired"), (Some("interrupted".to_owned()), None));
    // 仍有效的在途租约不动。
    let live_id = live["id"].as_str().expect("access id");
    assert_eq!(outcome(live_id), (None, None));
    assert_eq!(harness.count("leases"), 2);
    assert!(live_lease.is_string());
}

#[test]
fn summary_lists_the_last_24_hours_of_window_seconds_per_account_and_gateway() {
    let harness = Harness::history();
    let pair = harness.probe("21", json!({}));
    let first = record_of(&harness, &pair["generation"])["windowStartAt"]
        .as_f64()
        .expect("start");
    let confirmed = served_21(&harness, &pair, 10.0);
    // 不受账号与网关筛选影响。
    let summary =
        harness.stats(json!({"view": "summary", "accountId": "B", "gateway": "unified-143"}));
    let recent = summary["recentWindows"].as_array().expect("recent");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["accountId"], json!("A"));
    assert_eq!(recent[0]["gateway"], pair["gateway"]);
    assert_eq!(recent[0]["windows"], json!(1));
    assert_num(&recent[0]["windowSeconds"], confirmed - first);
    // 只算最近 24 小时：窗口时长早已停在 24 小时之前。
    harness.advance(100_000.0);
    let summary = harness.stats(json!({"view": "summary"}));
    assert_eq!(summary["recentWindows"], json!([]));
}
