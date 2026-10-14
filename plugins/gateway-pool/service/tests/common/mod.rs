//! 集成测试共用的辅助：临时目录、可控时钟、直接读写数据库的小工具。

#![allow(dead_code)]

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use gateway_pool::{DB_FILE, Failure, ManualClock, Object, Store, dispatch};
use rusqlite::{Connection, params};
use serde_json::Value;

pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(label: &str) -> Self {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let index = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!(
            "gateway-pool-{label}-{}-{index}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Harness {
    pub dir: TestDir,
    pub clock: ManualClock,
    pub store: Store,
    /// 历史用例里访问标识的递增序号（对应 Python 测试的 `self.sequence`）。
    pub sequence: std::sync::atomic::AtomicU64,
}

impl Harness {
    pub fn new(label: &str, now: f64) -> Self {
        let dir = TestDir::new(label);
        let clock = ManualClock::new(now);
        let store = Store::open(dir.path(), clock.clock()).expect("open store");
        Self {
            dir,
            clock,
            store,
            sequence: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn now(&self) -> f64 {
        (self.clock.clock())()
    }

    pub fn advance(&self, seconds: f64) {
        self.clock.advance(seconds);
    }

    /// 运行动作；默认账号 A，`data` 中的 `accountId` 可以覆盖（与 Python 测试的 `call` 相同）。
    pub fn try_rt(&self, action: &str, data: Value) -> Result<Value, Failure> {
        let mut message = object_of(serde_json::json!({"action": action, "accountId": "A"}));
        for (key, value) in object_of(data) {
            message.insert(key, value);
        }
        self.store.runtime(&Value::Object(message))
    }

    pub fn rt(&self, action: &str, data: Value) -> Value {
        self.try_rt(action, data)
            .unwrap_or_else(|error| panic!("{action} failed: {error}"))
    }

    pub fn rt_for(&self, account: &str, action: &str, data: Value) -> Value {
        self.rt(
            action,
            merge(serde_json::json!({"accountId": account}), data),
        )
    }

    /// 期望 `ValueError`：返回错误消息。
    pub fn rt_message(&self, action: &str, data: Value) -> String {
        message(self.try_rt(action, data))
    }

    /// 不带账号的原始运行消息（例如 `active`）。
    pub fn rt_raw(&self, message: Value) -> Value {
        self.store
            .runtime(&message)
            .unwrap_or_else(|error| panic!("runtime failed: {error}"))
    }

    /// 页面读取（含账号与网关过滤）。
    pub fn view(&self, query: Value) -> Value {
        self.call_ok("read", query)
    }

    pub fn pair_of(&self, account: &str, gateway: &str) -> Value {
        self.view(serde_json::json!({}))["pairs"]
            .as_array()
            .and_then(|pairs| {
                pairs.iter().find(|pair| {
                    pair["accountId"] == serde_json::json!(account)
                        && pair["gateway"] == serde_json::json!(gateway)
                })
            })
            .cloned()
            .unwrap_or_else(|| panic!("pair {account}/{gateway} not found"))
    }

    /// 账号 A 在 unified-142 上的组合。
    pub fn pair(&self) -> Value {
        self.pair_of("A", "unified-142")
    }

    pub fn queue_order(&self) -> Vec<String> {
        self.view(serde_json::json!({}))["accountQueue"]["accounts"]
            .as_array()
            .expect("accounts")
            .iter()
            .map(|account| account["accountId"].as_str().expect("id").to_owned())
            .collect()
    }

    pub fn queue_revision(&self) -> i64 {
        self.view(serde_json::json!({}))["accountQueue"]["revision"]
            .as_i64()
            .expect("revision")
    }

    /// 表行数。
    pub fn count(&self, table: &str) -> i64 {
        self.connection()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .expect("count rows")
    }

    /// 打一次完整的探测（对应 `test_gateway_pool.py` 的 `probe`）：返回 (组合, 结果)。
    pub fn probe_simple(&self, state_returned: bool, extra: Value) -> (Value, Value) {
        let claim = self.rt("claim", serde_json::json!({"demand": true}));
        assert_eq!(claim["claimed"], serde_json::json!(true));
        let lease = claim["leaseId"].clone();
        self.rt("sent", serde_json::json!({"leaseId": lease}));
        let result = self.rt(
            "probeResult",
            merge(
                serde_json::json!({"leaseId": lease, "status": 200, "stateReturned": state_returned,
                                   "paired": true, "gatewayMatches": true}),
                extra,
            ),
        );
        self.rt("finish", serde_json::json!({"leaseId": lease}));
        (claim["pair"].clone(), result)
    }

    // ---- 历史用例的访问计时流程（对应 `GatewayPoolHistoryTests`） ----

    pub fn history() -> Self {
        let harness = Self::new("history", 10_000.0);
        harness.call_ok(
            "configure",
            serde_json::json!({"gateways": ["unified-142", "unified-143"], "initialRestSeconds": 1, "maximumRestSeconds": 8}),
        );
        harness
    }

    pub fn send(&self, lease: &Value, kind: &str) -> Value {
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let access = serde_json::json!({"id": sequence.to_string(), "startedAt": self.now(),
                                        "elapsedMs": 0, "ended": false, "outcome": "headers"});
        self.rt(
            "sent",
            serde_json::json!({"leaseId": lease, "timingVersion": 2, "accessId": access["id"], "purpose": kind}),
        );
        access
    }

    pub fn report(
        &self,
        action: &str,
        lease: &Value,
        access: &Value,
        end: bool,
        extra: Value,
    ) -> Value {
        let started = access["startedAt"].as_f64().expect("startedAt");
        let mut item = object_of(access.clone());
        item.insert(
            "elapsedMs".into(),
            serde_json::json!((self.now() - started) * 1000.0),
        );
        item.insert("ended".into(), serde_json::json!(end));
        item.insert(
            "outcome".into(),
            serde_json::json!(if end { "completed" } else { "headers" }),
        );
        self.rt(
            action,
            merge(serde_json::json!({"leaseId": lease, "access": item}), extra),
        )
    }

    /// 完整探测：bootstrap 与 replay 两次访问后上报结果并归还租约；返回被探测的组合。
    pub fn probe(&self, verdict: &str, extra: Value) -> Value {
        let claim = self.rt("claim", merge(serde_json::json!({"demand": true}), extra));
        assert_eq!(claim["claimed"], serde_json::json!(true));
        let lease = claim["leaseId"].clone();
        let start = self.now();
        for phase in ["bootstrap", "replay"] {
            let access = self.send(&lease, phase);
            self.advance(0.1);
            self.report(
                "probeAttempt",
                &lease,
                &access,
                true,
                serde_json::json!({"phase": phase, "retry": 0, "status": 200,
                                   "stateReturned": phase == "bootstrap" || verdict == "29"}),
            );
        }
        self.rt(
            "probeResult",
            serde_json::json!({"leaseId": lease, "status": 200, "stateReturned": verdict == "29",
                               "paired": true, "gatewayMatches": true, "preparationStartedAt": start,
                               "durationMs": (self.now() - start) * 1000.0}),
        );
        self.rt("finish", serde_json::json!({"leaseId": lease}));
        claim["pair"].clone()
    }

    pub fn business(&self, pair: &Value) -> (Value, Value) {
        let lease = self.rt(
            "reserve",
            serde_json::json!({"gateway": pair["gateway"], "generation": pair["generation"],
                               "deadline": self.now() + 20000.0}),
        )["leaseId"]
            .clone();
        let access = self.send(&lease, "business");
        (lease, access)
    }

    pub fn finish_access(&self, lease: &Value, access: &Value, delay: f64) {
        self.advance(delay);
        self.report("finish", lease, access, true, serde_json::json!({}));
    }

    /// 通过预约 + invalidate + finish 退役窗口（对应 Python 的 `retire`）。
    pub fn retire_pair(&self, pair: &Value) {
        let lease = self.rt(
            "reserve",
            serde_json::json!({"gateway": pair["gateway"], "generation": pair["generation"],
                               "deadline": self.now() + 100.0}),
        )["leaseId"]
            .clone();
        self.rt("invalidate", serde_json::json!({"leaseId": lease}));
        self.rt("finish", serde_json::json!({"leaseId": lease}));
    }

    pub fn stats(&self, query: Value) -> Value {
        self.call_ok("history.read", query)
    }

    /// 关闭并重新打开同一数据目录，验证数据在重启后保留。
    pub fn reopen(&mut self) {
        self.store = Store::open(self.dir.path(), self.clock.clock()).expect("reopen store");
    }

    pub fn call(&self, method: &str, data: Value) -> Result<Value, Failure> {
        let object = object_of(data);
        dispatch(&self.store, method, &object).map(|(value, _)| value)
    }

    pub fn call_ok(&self, method: &str, data: Value) -> Value {
        self.call(method, data)
            .unwrap_or_else(|error| panic!("{method} failed: {error}"))
    }

    pub fn connection(&self) -> Connection {
        Connection::open(self.dir.path().join(DB_FILE)).expect("open database directly")
    }

    /// 直接写入一条组合事实（运行状态机在后续步骤迁入，这里只构造测试所需的初始状态）。
    pub fn seed_pair(&self, account: &str, gateway: &str, data: Value) {
        self.connection()
            .execute(
                "INSERT OR REPLACE INTO pairs VALUES (?,?,?)",
                params![account, gateway, data.to_string()],
            )
            .expect("seed pair");
    }

    /// 读取当前页面用的组合列表（经 `read` 管理方法）。
    pub fn pairs(&self) -> Vec<Value> {
        self.call_ok("read", serde_json::json!({}))["pairs"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }
}

pub fn object_of(data: Value) -> Object {
    match data {
        Value::Object(object) => object,
        other => panic!("expected JSON object, got {other}"),
    }
}

/// 把两个 JSON 对象合并（后者覆盖前者）。
pub fn merge(base: Value, extra: Value) -> Value {
    let mut result = object_of(base);
    for (key, value) in object_of(extra) {
        result.insert(key, value);
    }
    Value::Object(result)
}

pub fn message(result: Result<Value, Failure>) -> String {
    match result {
        Err(Failure::Invalid(message)) => message,
        Err(other) => panic!("expected invalid parameters, got {other}"),
        Ok(value) => panic!("expected invalid parameters, got {value}"),
    }
}

/// 数值近似相等（对应 Python 的 `assertAlmostEqual`，也用于不区分整数与浮点的比较）。
pub fn assert_num(value: &Value, expected: f64) {
    let actual = value
        .as_f64()
        .unwrap_or_else(|| panic!("expected a number, got {value}"));
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}
