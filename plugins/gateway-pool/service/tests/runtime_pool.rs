//! 运行状态机用例，移植自 `services/plugin_host/test_gateway_pool.py`（每个测试名与 Python 用例同名）。
//! 断言与 Python 保持相同；时钟为浮点（生产时钟恒为浮点），数值比较不区分整数与浮点。

mod common;

use common::{Harness, assert_num};
use gateway_pool::Failure;
use serde_json::{Value, json};

fn pool() -> Harness {
    let harness = Harness::new("pool", 10_000.0);
    harness.call_ok(
        "configure",
        json!({"gateways": ["unified-142", "unified-143"], "initialRestSeconds": 3600}),
    );
    harness
}

fn settings(harness: &Harness) -> Value {
    harness.view(json!({}))["settings"].clone()
}

fn dynamic_groups(harness: &Harness) {
    harness.call_ok(
        "configure",
        json!({"groupIds": ["grp_degraded"], "dynamicGroups": true,
               "normalGroupId": "grp_normal", "degradedGroupId": "grp_degraded"}),
    );
}

#[test]
fn model_ticket_shares_window_and_expires_with_generation() {
    let harness = pool();
    assert_eq!(settings(&harness)["probeModel"], json!("gpt-6.1-sol"));
    let (pair, _) = harness.probe_simple(false, json!({}));
    let generation = pair["generation"].clone();
    let lease = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": generation, "deadline": harness.now() + 60.0}),
    )["leaseId"]
        .clone();
    let ticket = json!({"state": "astra-state", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + 600.0});
    // 未实际发送的租约不能记录拿票。
    assert_eq!(
        harness.rt_message(
            "ticketResult",
            json!({"leaseId": lease, "model": "gpt-6-astra", "status": 200, "stateReturned": true, "material": ticket}),
        ),
        "拿票记录无效"
    );
    harness.rt("sent", json!({"leaseId": lease}));
    harness.rt(
        "ticketResult",
        json!({"leaseId": lease, "model": "gpt-6-astra", "status": 200, "stateReturned": true,
               "durationMs": 5, "material": ticket}),
    );
    assert_eq!(
        harness.rt(
            "ticket",
            json!({"generation": generation, "model": "gpt-6-astra"})
        ),
        ticket
    );
    assert_eq!(harness.pair()["ticketModels"], json!(["gpt-6-astra"]));
    assert!(!harness.view(json!({})).to_string().contains("astra-state"));
    assert_eq!(
        harness.rt(
            "ticket",
            json!({"generation": generation, "model": "gpt-6.1-sol"})
        ),
        Value::Null
    );
    let sample = harness.view(json!({}))["observations"][0].clone();
    assert_eq!(
        (
            &sample["source"],
            &sample["model"],
            &sample["classification"]
        ),
        (&json!("ticket"), &json!("gpt-6-astra"), &Value::Null)
    );
    // 拿票不做判定，窗口仍是同一组合；29 退役后该代次的票全部作废。
    assert_eq!(harness.pair()["state"], json!("ready"));
    harness.rt(
        "businessResult",
        json!({"leaseId": lease, "model": "gpt-6-astra", "status": 200, "stateReturned": true,
               "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": lease}));
    assert_eq!(harness.pair()["state"], json!("cooling"));
    assert_eq!(
        harness.rt(
            "ticket",
            json!({"generation": generation, "model": "gpt-6-astra"})
        ),
        Value::Null
    );
    assert_eq!(
        harness.rt_message(
            "ticket",
            json!({"generation": generation, "model": "GPT 6"})
        ),
        "模型标识无效"
    );
}

#[test]
fn candidates_follow_pool_catalog_and_cookie_expiry_rotates() {
    let harness = pool();
    harness.call_ok("configure", json!({"gateways": []}));
    let catalog = json!([{"gateway": "unified-150", "validForS": 100, "country": "CH"},
                         {"gateway": "unified-151", "validForS": 2000, "country": "US"}]);
    // 不在目录中的网关不是候选；Cookie 剩余不足一个窗口的排在后面。
    let claim = harness.rt("claim", json!({"demand": true, "catalog": catalog}));
    assert_eq!(claim["pair"]["gateway"], json!("unified-151"));
    let lease = claim["leaseId"].clone();
    harness.rt("sent", json!({"leaseId": lease}));
    let material =
        json!({"state": "s0", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + 600.0});
    harness.rt(
        "probeResult",
        json!({"leaseId": lease, "status": 200, "stateReturned": false, "paired": true,
               "gatewayMatches": true, "material": material}),
    );
    harness.rt("finish", json!({"leaseId": lease}));
    let pair = harness.pair_of("A", "unified-151");
    assert_eq!(pair["state"], json!("ready"));
    assert_num(&pair["cookieExpiresAt"], harness.now() + 600.0);
    assert_eq!(
        harness.view(json!({}))["catalogs"]["A"]["gateways"][0]["country"],
        json!("CH")
    );
    // 窗口未到，但 Cookie 即将到期：开始准备下一个；目录里已无此网关也继续服务。
    harness.advance(100.0);
    let narrowed = json!([{"gateway": "unified-150", "validForS": 3000}]);
    let plan = harness.rt("plan", json!({"demand": true, "catalog": narrowed}));
    assert_eq!(plan["rotationDue"], json!(false));
    assert_eq!(plan["ready"]["gateway"], json!("unified-151"));
    harness.advance(450.0);
    let plan = harness.rt("plan", json!({"demand": true, "catalog": narrowed}));
    assert_eq!(plan["rotationDue"], json!(true));
    assert_eq!(plan["probe"]["gateway"], json!("unified-150"));
    assert_eq!(
        harness.rt_message(
            "plan",
            json!({"demand": true, "catalog": [{"gateway": "evil", "validForS": 1}]})
        ),
        "网关目录无效"
    );
}

#[test]
fn windows_are_independent_per_account() {
    let harness = pool();
    harness.probe_simple(false, json!({}));
    assert_eq!(
        harness.rt_raw(json!({"action": "active"}))["accounts"],
        json!(["A"])
    );
    // A 的窗口未到期：B 属于另一个 Key 的选择，可以独立开自己的窗口。
    let claim = harness.rt_for("B", "claim", json!({"demand": true}));
    assert_eq!(claim["claimed"], json!(true));
    harness.rt_for("B", "sent", json!({"leaseId": claim["leaseId"]}));
    harness.rt_for(
        "B",
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "stateReturned": false, "paired": true, "gatewayMatches": true}),
    );
    harness.rt_for("B", "finish", json!({"leaseId": claim["leaseId"]}));
    let mut accounts: Vec<Value> = harness.rt_raw(json!({"action": "active"}))["accounts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    accounts.sort_by_key(Value::to_string);
    assert_eq!(accounts, vec![json!("A"), json!("B")]);
    assert_eq!(harness.pair()["state"], json!("ready"));
    assert_eq!(harness.pair_of("B", "unified-142")["state"], json!("ready"));
    let switch = harness.view(json!({"accountId": "B"}))["switches"][0].clone();
    assert_eq!(
        (&switch["fromAccount"], &switch["fromGateway"]),
        (&Value::Null, &Value::Null)
    );
}

#[test]
fn regroup_records_one_event_per_direction_and_result_closes_it() {
    let harness = pool();
    let regroup = |extra: Value| {
        harness.try_rt(
            "regroup",
            common::merge(json!({"model": "gpt-6.1-sol"}), extra),
        )
    };
    // 未开启：不产生记录。
    assert_eq!(
        regroup(json!({"direction": "degraded"})).expect("disabled"),
        json!({"move": null, "reason": "disabled"})
    );
    dynamic_groups(&harness);
    let first = regroup(json!({"direction": "degraded"})).expect("first")["move"].clone();
    assert_eq!(
        (&first["removeGroup"], &first["addGroup"]),
        (&json!("grp_normal"), &json!("grp_degraded"))
    );
    // 刚发出还没回报：不重复。
    assert_eq!(
        regroup(json!({"direction": "degraded"})).expect("again")["reason"],
        json!("recent")
    );
    harness.rt(
        "regroupResult",
        json!({"eventId": first["eventId"], "status": "moved"}),
    );
    harness.advance(100.0);
    assert_eq!(
        regroup(json!({"direction": "degraded"})).expect("moved")["reason"],
        json!("recent")
    );
    // 恢复方向独立，且方向相反。
    let back = regroup(json!({"direction": "recovered"})).expect("back")["move"].clone();
    assert_eq!(
        (&back["removeGroup"], &back["addGroup"]),
        (&json!("grp_degraded"), &json!("grp_normal"))
    );
    // 10 分钟后同方向可以再次移动（账号被手动移回后又降智）。
    harness.advance(601.0);
    assert!(!regroup(json!({"direction": "degraded"})).expect("later")["move"].is_null());
    let events = harness.view(json!({}))["groupEvents"].clone();
    let statuses: Vec<&Value> = events
        .as_array()
        .expect("events")
        .iter()
        .filter(|event| event["direction"] == json!("degraded"))
        .map(|event| &event["status"])
        .collect();
    assert_eq!(statuses, vec![&json!("pending"), &json!("moved")]);
    assert_eq!(
        common::message(regroup(json!({"direction": "sideways"}))),
        "分组方向无效"
    );
    assert_eq!(
        common::message(regroup(
            json!({"direction": "degraded", "model": "bad model!"})
        )),
        "模型无效"
    );
    assert_eq!(
        harness.rt_message(
            "regroupResult",
            json!({"eventId": "nope", "status": "moved"})
        ),
        "分组结果无效"
    );
    assert_eq!(
        harness.rt_message(
            "regroupResult",
            json!({"eventId": first["eventId"], "status": "done"})
        ),
        "分组结果无效"
    );
}

#[test]
fn recovery_check_needs_eight_minutes_of_full_power_and_pauses_after_failure() {
    let harness = pool();
    dynamic_groups(&harness);
    // 取一个 21 的窗口，并保存配套材料（Cookie 剩余时间足够长）。
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "stateReturned": false, "paired": true,
               "gatewayMatches": true,
               "material": {"state": "s0", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + 3000.0}}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    let check = || harness.rt("recoveryCheck", json!({}))["check"].clone();
    // 不到 8 分钟：不检测。
    harness.advance(479.0);
    assert_eq!(check(), Value::Null);
    harness.advance(2.0);
    let first = check();
    assert_eq!(first["gateway"], claim["pair"]["gateway"]);
    assert!(first["ageSeconds"].as_i64().expect("age") >= 480);
    // 同一代次只领取一次。
    assert_eq!(check(), Value::Null);
    // 检测失败：暂停 1 小时，期间不再领取。
    let paused = harness.rt("recoveryResult", json!({"outcome": "failed"}));
    assert_num(&paused["pausedUntil"], harness.now() + 3600.0);
    harness.advance(3599.0);
    assert_eq!(check(), Value::Null);
}

#[test]
fn recovery_check_ignores_generations_with_degraded_signals_or_expired_material() {
    let harness = pool();
    dynamic_groups(&harness);
    // 这一代出现过 29：不能作为恢复证据。
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "stateReturned": false, "paired": true,
               "gatewayMatches": true,
               "material": {"state": "s0", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + 3000.0}}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    let pair = claim["pair"].clone();
    let reserved = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 100.0}),
    );
    harness.rt("sent", json!({"leaseId": reserved["leaseId"]}));
    harness.rt(
        "businessResult",
        json!({"leaseId": reserved["leaseId"], "status": 200, "stateReturned": true, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": reserved["leaseId"]}));
    harness.advance(600.0);
    assert_eq!(harness.rt("recoveryCheck", json!({}))["check"], Value::Null);
    // 未开启动态分组：始终不领取。
    harness.call_ok("configure", json!({"dynamicGroups": false}));
    assert_eq!(harness.rt("recoveryCheck", json!({}))["check"], Value::Null);
    assert_eq!(
        harness.rt_message("recoveryResult", json!({"outcome": "maybe"})),
        "恢复检测结果无效"
    );
}

#[test]
fn interrupted_recovery_claim_expires_after_five_minutes_across_restart() {
    let mut harness = pool();
    dynamic_groups(&harness);
    let (pair, _) = harness.probe_simple(
        false,
        json!({"material": {
            "state": "s0", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + 20_000.0
        }}),
    );
    harness.advance(480.0);
    let claim = || json!({"action": "recoveryCheck", "accountId": "A"});
    assert_eq!(
        harness.rt_raw(claim())["check"]["generation"],
        pair["generation"]
    );
    harness.reopen(); // 领取后进程中断，没有 recoveryResult。
    harness.advance(299.0);
    assert_eq!(harness.rt_raw(claim())["check"], Value::Null);
    harness.advance(1.0);
    assert_eq!(harness.rt_raw(claim())["check"], Value::Null);
    harness.advance(0.001);
    let other = gateway_pool::Store::open(harness.dir.path(), harness.clock.clock()).unwrap();
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| harness.rt_raw(claim()));
        let b = scope.spawn(|| other.runtime(&claim()).unwrap());
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(
        results
            .iter()
            .filter(|r| r["check"]["generation"] == pair["generation"])
            .count(),
        1
    );
    assert_eq!(harness.rt_raw(claim())["check"], Value::Null);
    // 旧记录缺领取时间时视为过期，即使仍保留 claimed_generation。
    harness
        .connection()
        .execute(
            "UPDATE recovery_checks SET claimed_at=NULL WHERE account='A'",
            [],
        )
        .unwrap();
    assert_eq!(
        harness.rt_raw(claim())["check"]["generation"],
        pair["generation"]
    );
    // 已回报的同代不重领；失败暂停与成功/无法检测的既有规则保持不变。
    for outcome in ["failed", "unavailable", "recovered"] {
        harness.rt("recoveryResult", json!({"outcome": outcome}));
        harness.advance(3601.0);
        assert_eq!(harness.rt_raw(claim())["check"], Value::Null);
    }
}

#[test]
fn window_used_up_prepares_one_standby_while_the_current_window_keeps_serving() {
    let harness = pool();
    let (_, result) = harness.probe_simple(false, json!({}));
    assert_eq!(result["classification"], json!("21"));
    // 窗口还没用满：不准备备用票。
    assert_eq!(
        harness.rt("plan", json!({"demand": true}))["probe"],
        Value::Null
    );
    harness.advance(181.0);
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["rotationDue"], json!(true));
    assert_eq!(plan["standbyProbe"], json!(true));
    assert_eq!(plan["ready"]["gateway"], json!("unified-142"));
    assert_eq!(plan["probe"]["gateway"], json!("unified-143"));
    let (standby_pair, result) = harness.probe_simple(false, json!({}));
    assert_eq!(result["classification"], json!("21"));
    assert_eq!(standby_pair["gateway"], json!("unified-143"));
    // 当前窗口继续服务；备用票不接业务请求，也没有切换记录。
    assert_eq!(harness.pair()["state"], json!("ready"));
    let standby = harness.pair_of("A", "unified-143");
    assert_eq!(standby["state"], json!("standby"));
    assert_num(&standby["standbyAt"], harness.now());
    assert_num(&standby["standbyExpiresAt"], harness.now() + 3000.0);
    assert_eq!(
        harness.rt(
            "reserve",
            json!({"gateway": "unified-143", "generation": standby["generation"], "deadline": harness.now() + 5.0}),
        )["reserved"],
        json!(false)
    );
    // 只有第一个窗口发布时的那一条切换记录，备用票到达不产生切换。
    assert_eq!(
        harness.view(json!({}))["switches"].as_array().map(Vec::len),
        Some(1)
    );
    // 同一账号同时最多一张备用票：不再找票。
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["probe"], Value::Null);
    assert_eq!(plan["standby"]["gateway"], json!("unified-143"));
    assert_eq!(
        harness.rt("claim", json!({"demand": true}))["claimed"],
        json!(false)
    );
}

#[test]
fn standby_takes_over_at_once_on_a_29_and_the_window_counts_from_its_first_business_access() {
    let harness = pool();
    harness.call_ok(
        "configure",
        json!({"gateways": ["unified-142", "unified-143", "unified-144"]}),
    );
    let (first, _) = harness.probe_simple(false, json!({}));
    harness.advance(181.0);
    let (standby, _) = harness.probe_simple(false, with_material(&harness));
    harness.advance(60.0);
    let lease = reserve(&harness, &first);
    harness.rt("sent", json!({"leaseId": lease}));
    harness.rt(
        "businessResult",
        json!({"leaseId": lease, "status": 200, "stateReturned": true, "paired": true, "gatewayMatches": true}),
    );
    // 29 的同一次回报里备用票已经接手，不再找票；旧窗口等在途请求结束后静置。
    assert_eq!(harness.pair()["state"], json!("draining"));
    assert_eq!(harness.pair()["retiredReason"], json!("29"));
    let promoted = harness.pair_of("A", "unified-143");
    assert_eq!(promoted["state"], json!("ready"));
    assert_eq!(promoted["generation"], standby["generation"]);
    assert_eq!(promoted["fromStandby"], json!(true));
    assert_num(&promoted["activatedAt"], harness.now());
    assert_eq!(promoted["windowStartedAt"], Value::Null);
    harness.advance(10.0);
    harness.rt("finish", json!({"leaseId": lease}));
    // 旧窗口从最后一次访问结束起静置，起点档 1 小时（初始静置）。
    assert_num(&harness.pair()["restUntil"], harness.now() + 3600.0);
    let switch = harness.view(json!({}))["switches"][0].clone();
    assert_eq!(
        (
            &switch["fromGateway"],
            &switch["toGateway"],
            &switch["cause"]
        ),
        (
            &json!("unified-142"),
            &json!("unified-143"),
            &json!("switch")
        )
    );
    // 备用期间不计入窗口：转正后还没有业务访问，窗口不会到期。
    harness.advance(1000.0);
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["rotationDue"], json!(false));
    assert_eq!(plan["probe"], Value::Null);
    // 第一次业务访问开始计时；之后的备用票倒计时也从这里算起。
    let lease = reserve(&harness, &promoted);
    harness.rt("sent", json!({"leaseId": lease}));
    let started = harness.now();
    assert_num(
        &harness.pair_of("A", "unified-143")["windowStartedAt"],
        started,
    );
    harness.rt("finish", json!({"leaseId": lease}));
    harness.advance(179.0);
    assert_eq!(
        harness.rt("plan", json!({"demand": true}))["rotationDue"],
        json!(false)
    );
    harness.advance(2.0);
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["rotationDue"], json!(true));
    assert_eq!(plan["standbyProbe"], json!(true));
}

#[test]
fn unused_standby_is_dropped_after_fifty_minutes_and_rests_from_when_it_was_obtained() {
    let harness = pool();
    harness.call_ok(
        "configure",
        json!({"gateways": ["unified-142", "unified-143", "unified-144"]}),
    );
    harness.probe_simple(false, json!({}));
    harness.advance(181.0);
    harness.probe_simple(false, json!({}));
    let obtained = harness.now();
    harness.advance(2999.0);
    // 还在存活期内：不丢弃，也不再准备新的。
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["standby"]["state"], json!("standby"));
    assert_eq!(plan["probe"], Value::Null);
    harness.advance(2.0);
    // 超过 50 分钟：丢弃并静置，起点是拿到它的时间；窗口仍超过时长且有需求，准备新的备用票。
    let plan = harness.rt("plan", json!({"demand": true}));
    let dropped = harness.pair_of("A", "unified-143");
    assert_eq!(dropped["state"], json!("cooling"));
    assert_eq!(dropped["retiredReason"], json!("standby_expired"));
    assert_num(&dropped["restUntil"], obtained + 3600.0);
    assert_eq!(plan["standbyProbe"], json!(true));
    assert_eq!(plan["probe"]["gateway"], json!("unified-144"));
    // 没有需求时（懒模式）不准备新的。
    assert_eq!(
        harness.rt("plan", json!({"demand": false}))["probe"],
        Value::Null
    );
}

#[test]
fn cookie_ending_swaps_in_the_standby_and_without_one_probes_a_replacement_directly() {
    let harness = pool();
    harness.call_ok(
        "configure",
        json!({"gateways": ["unified-142", "unified-143", "unified-144"]}),
    );
    let material = |seconds: f64| json!({"material": {"state": "s0", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + seconds}});
    let (first, _) = harness.probe_simple(false, material(400.0));
    harness.advance(181.0);
    let (standby, _) = harness.probe_simple(false, material(3000.0));
    // 第一个窗口的 Cookie 在 340 秒时进入到期前的准备期：备用票立即接手，不再找票。
    harness.advance(160.0);
    let lease = reserve(&harness, &first);
    harness.rt("sent", json!({"leaseId": lease}));
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["ready"]["generation"], standby["generation"]);
    assert_eq!(plan["probe"], Value::Null);
    assert_eq!(plan["rotationDue"], json!(false));
    let old = harness.pair();
    assert_eq!(old["state"], json!("draining"));
    assert_eq!(old["retiredReason"], json!("cookie_expiring"));
    let switch = harness.view(json!({}))["switches"][0].clone();
    assert_eq!(switch["cause"], json!("cookie_expiring"));
    harness.rt("finish", json!({"leaseId": lease}));
    assert_eq!(harness.pair()["state"], json!("cooling"));
    // 没有备用票时 Cookie 即将到期：直接找替代（不是备用票），通过后旧窗口退役。
    harness.advance(2780.0);
    let plan = harness.rt("plan", json!({"demand": true}));
    assert_eq!(plan["standbyProbe"], json!(false));
    assert_eq!(plan["probe"]["gateway"], json!("unified-144"));
    let (replacement, _) = harness.probe_simple(false, material(3000.0));
    assert_eq!(replacement["gateway"], json!("unified-144"));
    let retired = harness.pair_of("A", "unified-143");
    assert_eq!(retired["state"], json!("cooling"));
    assert_eq!(retired["retiredReason"], json!("cookie_expiring"));
    assert_eq!(harness.pair_of("A", "unified-144")["state"], json!("ready"));
}

#[test]
fn account_leaving_the_schedule_drops_the_standby_and_rests_it() {
    let harness = pool();
    harness.probe_simple(false, json!({}));
    harness.advance(181.0);
    harness.probe_simple(false, json!({}));
    let obtained = harness.now();
    harness.advance(100.0);
    harness.rt("quota", json!({"exhausted": true}));
    let ready = harness.pair();
    let standby = harness.pair_of("A", "unified-143");
    assert_eq!(ready["state"], json!("cooling"));
    assert_eq!(standby["state"], json!("cooling"));
    assert_eq!(standby["retiredReason"], json!("account_left"));
    assert_num(&standby["restUntil"], obtained + 3600.0);
}

#[test]
fn removing_the_serving_gateway_from_the_scope_promotes_an_in_scope_standby() {
    let harness = pool();
    harness.probe_simple(false, json!({}));
    harness.advance(181.0);
    harness.probe_simple(false, with_material(&harness));
    harness.call_ok("configure", json!({"gateways": ["unified-143"]}));
    assert_eq!(harness.pair()["retiredReason"], json!("configuration"));
    assert_eq!(harness.pair_of("A", "unified-143")["state"], json!("ready"));
}

#[test]
fn additive_backoff_adds_a_step_per_29_retest_and_lowers_after_two_recoveries_on_one_tier() {
    let harness = pool();
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    // 从未静置过的组合先 29：从起始档（初始静置 1h）开始。
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 3600.0);
    // 复测仍 29：每次加一个步长（1h）。
    harness.advance(3600.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 7200.0);
    harness.advance(7200.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 10800.0);
    harness.advance(10800.0);
    let (pair, _) = harness.probe_simple(false, json!({}));
    // 在 3h 档恢复：以后从该档开始。
    assert_num(&harness.pair()["lastRecoveredRestSeconds"], 10800.0);
    assert_num(&harness.pair()["learnedRestSeconds"], 10800.0);
    // 业务中途 29 从起始档开始，不升档。
    business_29(&harness, &pair);
    assert_num(&harness.pair()["restSeconds"], 10800.0);
    assert_num(&harness.pair()["tierSeconds"], 10800.0);
    // 连续两次在同一档恢复：下次起始档降一档（减一个步长）。
    harness.advance(10800.0);
    let (pair, _) = harness.probe_simple(false, json!({}));
    assert_num(&harness.pair()["recoveryStreak"], 2.0);
    assert_num(&harness.pair()["learnedRestSeconds"], 7200.0);
    business_29(&harness, &pair);
    assert_num(&harness.pair()["restSeconds"], 7200.0);
    // 在较低的档恢复：不清空，记为新的恢复档。
    harness.advance(7200.0);
    harness.probe_simple(false, json!({}));
    assert_num(&harness.pair()["lastRecoveredRestSeconds"], 7200.0);
    assert_num(&harness.pair()["recoveryStreak"], 1.0);
    assert_num(&harness.pair()["learnedRestSeconds"], 7200.0);
}

#[test]
fn multiplicative_backoff_doubles_and_halves_within_the_limits() {
    let harness = pool();
    harness.call_ok(
        "configure",
        json!({"gateways": ["unified-142"], "backoffMode": "multiplicative", "initialRestSeconds": 3600, "maximumRestSeconds": 14400}),
    );
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 3600.0);
    harness.advance(3600.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 7200.0);
    harness.advance(7200.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 14400.0);
    // 上限：不超过最长静置。
    harness.advance(14400.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 14400.0);
    harness.advance(14400.0);
    let (pair, _) = harness.probe_simple(false, json!({}));
    business_29(&harness, &pair);
    harness.advance(14400.0);
    let (pair, _) = harness.probe_simple(false, json!({}));
    // 同一档连续两次恢复：减半，不低于初始静置。
    assert_num(&harness.pair()["learnedRestSeconds"], 7200.0);
    business_29(&harness, &pair);
    assert_num(&harness.pair()["restSeconds"], 7200.0);
    harness.advance(7200.0);
    harness.probe_simple(false, json!({}));
}

#[test]
fn detection_failure_rests_300_seconds_without_touching_the_tier_or_the_start_tier() {
    let harness = pool();
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    harness.probe_simple(true, json!({}));
    harness.advance(3600.0);
    let before = harness.pair();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.advance(1.0);
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 429, "paired": true, "gatewayMatches": true, "failed": true, "failure": "http_status"}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    let pair = harness.pair();
    assert_eq!(pair["retiredReason"], json!("probe_failed"));
    assert_num(&pair["restSeconds"], 300.0);
    assert_num(&pair["restUntil"], harness.now() + 300.0);
    assert_eq!(pair["tierSeconds"], before["tierSeconds"]);
    assert_eq!(pair["learnedRestSeconds"], before["learnedRestSeconds"]);
    // 重试仍是 29：在原来的档上升一档，失败不算一档。
    harness.advance(300.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restSeconds"], 7200.0);
    assert_num(&harness.pair()["tierSeconds"], 7200.0);
}

#[test]
fn business_21_is_not_retired_and_29_retires() {
    let harness = pool();
    let (pair, _) = harness.probe_simple(false, json!({}));
    for (returned, verdict) in [(false, "21"), (true, "29")] {
        let lease = harness.rt(
            "reserve",
            json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 100.0}),
        )["leaseId"]
            .clone();
        harness.rt("sent", json!({"leaseId": lease}));
        assert_eq!(
            harness.rt(
                "businessResult",
                json!({"leaseId": lease, "status": 200, "stateReturned": returned, "paired": true, "gatewayMatches": true}),
            )["classification"],
            json!(verdict)
        );
        harness.rt("finish", json!({"leaseId": lease}));
    }
    assert_eq!(harness.pair()["state"], json!("cooling"));
}

#[test]
fn replacement_after_29_keeps_previous_gateway_and_window() {
    let harness = pool();
    let (pair, _) = harness.probe_simple(false, json!({}));
    let lease = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 100.0}),
    )["leaseId"]
        .clone();
    harness.rt("sent", json!({"leaseId": lease}));
    harness.rt(
        "businessResult",
        json!({"leaseId": lease, "status": 200, "stateReturned": true, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": lease}));
    harness.advance(10.0);
    harness.probe_simple(false, json!({}));
    let switch = harness.view(json!({}))["switches"][0].clone();
    assert_eq!(switch["fromGateway"], json!("unified-142"));
    assert_eq!(switch["toGateway"], json!("unified-143"));
    assert_num(&switch["previousWindowSeconds"], 10.0);
}

#[test]
fn cooldown_restarts_at_each_probe_and_records_precontact_idle() {
    let harness = pool();
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    harness.probe_simple(true, json!({}));
    harness.advance(3600.0);
    harness.probe_simple(true, json!({}));
    assert_num(&harness.pair()["restUntil"], harness.now() + 7200.0);
    assert_num(
        &harness.view(json!({}))["observations"][0]["observedIdleSeconds"],
        3600.0,
    );
    harness.advance(7200.0);
    harness.probe_simple(false, json!({}));
    let pair = harness.pair();
    assert_num(&pair["lastRecoveredRestSeconds"], 7200.0);
    // 第一次恢复就记下恢复档，下一次从该档开始。
    assert_num(&pair["learnedRestSeconds"], 7200.0);
}

#[test]
fn lazy_has_no_idle_preparation_continuous_does() {
    let harness = pool();
    harness.probe_simple(false, json!({}));
    harness.advance(181.0);
    assert_eq!(
        harness.rt("plan", json!({"demand": false}))["probe"],
        Value::Null
    );
    harness.call_ok("configure", json!({"mode": "continuous"}));
    assert!(!harness.rt("plan", json!({"demand": false}))["probe"].is_null());
}

#[test]
fn restart_keeps_cooldown_configuration_and_samples() {
    let mut harness = pool();
    harness.call_ok(
        "configure",
        json!({"initialRestSeconds": 1800, "poolToken": "fake-pool-secret"}),
    );
    harness.probe_simple(true, json!({}));
    harness.reopen();
    assert_num(&harness.pair()["restUntil"], harness.now() + 1800.0);
    let visible = harness.view(json!({}));
    assert!(!visible.to_string().contains("fake-pool-secret"));
    assert_eq!(visible["settings"]["poolTokenConfigured"], json!(true));
    assert_eq!(visible["observations"].as_array().map(Vec::len), Some(1));
}

#[test]
fn simultaneous_ab_claims_only_prepare_once() {
    let harness = pool();
    let other = gateway_pool::Store::open(harness.dir.path(), harness.clock.clock())
        .expect("second process");
    let claim = json!({"action": "claim", "accountId": "A", "demand": true});
    let results: Vec<Value> = std::thread::scope(|scope| {
        let first = scope.spawn(|| harness.store.runtime(&claim).expect("first claim"));
        let second = scope.spawn(|| other.runtime(&claim).expect("second claim"));
        vec![first.join().expect("first"), second.join().expect("second")]
    });
    let claimed = results
        .iter()
        .filter(|result| result["claimed"] == json!(true))
        .count();
    assert_eq!(claimed, 1);
}

#[test]
fn accounts_cool_independently_on_same_gateway() {
    let harness = pool();
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    harness.probe_simple(true, json!({}));
    assert_eq!(
        harness.rt("claim", json!({"demand": true}))["claimed"],
        json!(false)
    );
    assert_eq!(
        harness.rt_for("B", "claim", json!({"demand": true}))["claimed"],
        json!(true)
    );
}

#[test]
fn unknown_errors_and_gateway_change_never_classify_21() {
    let harness = pool();
    for (status, matched) in [(401, true), (429, true), (200, false)] {
        let account = format!("{status}{}", if matched { "True" } else { "False" });
        let claim = harness.rt_for(&account, "claim", json!({"demand": true}));
        let result = harness.rt_for(
            &account,
            "probeResult",
            json!({"leaseId": claim["leaseId"], "status": status, "paired": true,
                   "gatewayMatches": matched, "stateReturned": false}),
        );
        assert_eq!(result["classification"], Value::Null);
    }
}

#[test]
fn no_probe_contact_does_not_restart_idle_and_crash_is_recoverable() {
    let harness = pool();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.advance(61.0);
    harness.rt("plan", json!({"demand": true}));
    let pair = harness.pair();
    assert_eq!(pair["state"], json!("unknown"));
    assert!(pair.get("lastContactAt").is_none());
    assert_eq!(
        harness.rt("finish", json!({"leaseId": claim["leaseId"]}))["accepted"],
        json!(false)
    );
}

#[test]
fn window_and_observed_idle_start_at_actual_send_not_claim() {
    let harness = pool();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.advance(5.0);
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    let first = harness.now();
    harness.advance(3.0);
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "stateReturned": false, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    assert_num(&harness.pair()["firstContactAt"], first);
}

#[test]
fn switch_rejects_unsent_reservation_but_keeps_sent_request() {
    let harness = pool();
    let (pair, _) = harness.probe_simple(false, json!({}));
    let pending = reserve(&harness, &pair);
    harness.advance(181.0);
    // 备用票到达时当前窗口照常服务，未发送的预约仍可发送。
    harness.probe_simple(false, with_material(&harness));
    harness.retire_pair(&pair);
    // 备用票接手、旧窗口退役之后，旧预约不能再发送到退役组合。
    assert_eq!(
        harness.rt("sent", json!({"leaseId": pending}))["accepted"],
        json!(false)
    );
    harness.rt("finish", json!({"leaseId": pending}));
    assert_num(&harness.pair()["lastContactAt"], 10_000.0);
    assert_eq!(harness.pair_of("A", "unified-143")["state"], json!("ready"));
}

#[test]
fn disabled_plugin_still_accepts_inflight_completion() {
    let harness = pool();
    let (pair, _) = harness.probe_simple(false, json!({}));
    let lease = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 500.0}),
    )["leaseId"]
        .clone();
    harness.rt("sent", json!({"leaseId": lease}));
    harness.store.set_enabled(false);
    harness.advance(10.0);
    assert_eq!(
        harness.rt("finish", json!({"leaseId": lease}))["accepted"],
        json!(true)
    );
    assert_eq!(harness.pair()["inFlight"], json!(0));
    assert_eq!(harness.rt("config", json!({}))["enabled"], json!(false));
    // 其余运行动作在停用期间被拒绝（Python 的 PermissionError）。
    match harness.try_rt("claim", json!({"demand": true})) {
        Err(Failure::Forbidden(message)) => assert_eq!(message, "网关池插件未启用"),
        other => panic!("expected forbidden, got {other:?}"),
    }
}

#[test]
fn private_material_shared_across_restart_never_visible_in_stats() {
    let mut harness = pool();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    let material = json!({"state": "fake-state-secret", "cookie": "fake-cookie-secret", "expires": harness.now() + 600.0});
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "stateReturned": false, "paired": true,
               "gatewayMatches": true, "material": material}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    harness.reopen();
    let generation = claim["pair"]["generation"].clone();
    assert_eq!(
        harness.rt("material", json!({"generation": generation})),
        material
    );
    let visible = harness.view(json!({})).to_string();
    assert!(!visible.contains("fake-state-secret"));
    assert!(!visible.contains("fake-cookie-secret"));
    harness.advance(601.0);
    assert_eq!(
        harness.rt("material", json!({"generation": generation})),
        Value::Null
    );
}

#[test]
fn probe_interval_plan_has_no_probe_without_demand() {
    // `test_probe_interval_stays_integer_and_default_is_50ms` 的 plan 部分；设置部分在 settings.rs。
    let harness = pool();
    assert_eq!(
        harness.rt("plan", json!({"demand": false}))["probe"],
        Value::Null
    );
}

#[test]
fn error_attempts_remain_unclassified_and_failure_rests_300_seconds() {
    let harness = pool();
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    harness.probe_simple(true, json!({}));
    harness.advance(3600.0);
    let claim = harness.rt("claim", json!({"demand": true}));
    for retry in [0, 1] {
        harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
        harness.advance(1.0);
        harness.rt(
            "probeAttempt",
            json!({"leaseId": claim["leaseId"], "phase": "replay", "retry": retry, "status": 402,
                   "stateReturned": false, "durationMs": 42}),
        );
    }
    let result = harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 402, "paired": true, "gatewayMatches": true, "failed": true}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    assert_eq!(result["classification"], Value::Null);
    let pair = harness.pair();
    assert_eq!(pair["retiredReason"], json!("probe_failed"));
    assert_num(&pair["restSeconds"], 300.0);
    assert_num(&pair["restUntil"], harness.now() + 300.0);
    let observations = harness.view(json!({}))["observations"].clone();
    let attempts: Vec<&Value> = observations
        .as_array()
        .expect("observations")
        .iter()
        .filter(|sample| sample["source"] == json!("probe-attempt"))
        .collect();
    assert_eq!(
        attempts
            .iter()
            .map(|sample| sample["retry"].clone())
            .collect::<Vec<_>>(),
        vec![json!(1), json!(0)]
    );
    assert!(
        attempts
            .iter()
            .all(|sample| sample["status"] == json!(402) && sample["classification"].is_null())
    );
}

#[test]
fn bootstrap_returning_state_is_material_acquisition_not_29() {
    let harness = pool();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.rt(
        "probeAttempt",
        json!({"leaseId": claim["leaseId"], "phase": "bootstrap", "retry": 0, "status": 200, "stateReturned": true}),
    );
    let sample = harness.view(json!({}))["observations"][0].clone();
    assert_eq!(sample["classification"], Value::Null);
    assert_eq!(sample["stateReturned"], json!(true));
    assert_eq!(harness.pair()["state"], json!("preparing"));
}

#[test]
fn probe_finishes_after_disable_without_leaving_preparing_state() {
    let harness = pool();
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.store.set_enabled(false);
    harness.advance(10.0);
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    let pair = harness.pair();
    // 没有得到判定：与检测失败相同，固定 300 秒。
    assert_eq!(pair["state"], json!("cooling"));
    assert_eq!(pair["inFlight"], json!(0));
    assert_eq!(pair["retiredReason"], json!("probe_interrupted"));
    assert_num(&pair["restUntil"], harness.now() + 300.0);
    harness.store.set_enabled(true);
    harness.advance(300.0);
    assert!(!harness.rt("plan", json!({"demand": true}))["probe"].is_null());
}

#[test]
fn removed_gateway_cannot_publish_ready_or_send_reserved_request() {
    let harness = pool();
    let (pair, _) = harness.probe_simple(false, json!({}));
    let lease = harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 500.0}),
    )["leaseId"]
        .clone();
    harness.call_ok("configure", json!({"gateways": ["unified-143"]}));
    assert_eq!(
        harness.rt("sent", json!({"leaseId": lease}))["accepted"],
        json!(false)
    );
    harness.rt("finish", json!({"leaseId": lease}));
    let claim = harness.rt("claim", json!({"demand": true}));
    harness.rt("sent", json!({"leaseId": claim["leaseId"]}));
    harness.call_ok("configure", json!({"gateways": ["unified-142"]}));
    harness.rt(
        "probeResult",
        json!({"leaseId": claim["leaseId"], "status": 200, "stateReturned": false, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": claim["leaseId"]}));
    assert_eq!(
        harness.pair_of("A", "unified-143")["state"],
        json!("cooling")
    );
}

#[test]
fn switch_records_follow_account_and_gateway_filters() {
    let harness = pool();
    let (pair, _) = harness.probe_simple(false, json!({}));
    harness.advance(181.0);
    harness.probe_simple(false, with_material(&harness));
    harness.retire_pair(&pair);
    assert_eq!(
        harness.view(json!({"accountId": "B"}))["switches"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(
        harness.view(json!({"gateway": "unified-143"}))["switches"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
}

/// 带配套材料的探测参数：备用票转正前要求材料仍有效。
fn with_material(harness: &Harness) -> Value {
    json!({"material": {"state": "s0", "cookie": "__oailb=x; __cflb=y", "expires": harness.now() + 50_000.0}})
}

fn reserve(harness: &Harness, pair: &Value) -> Value {
    harness.rt(
        "reserve",
        json!({"gateway": pair["gateway"], "generation": pair["generation"], "deadline": harness.now() + 500.0}),
    )["leaseId"]
        .clone()
}

/// 服务中的窗口遇到一次 29 并归还租约。
fn business_29(harness: &Harness, pair: &Value) {
    let lease = reserve(harness, pair);
    harness.rt("sent", json!({"leaseId": lease}));
    harness.rt(
        "businessResult",
        json!({"leaseId": lease, "status": 200, "stateReturned": true, "paired": true, "gatewayMatches": true}),
    );
    harness.rt("finish", json!({"leaseId": lease}));
}
