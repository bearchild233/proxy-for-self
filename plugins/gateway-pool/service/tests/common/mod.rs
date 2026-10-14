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
}

impl Harness {
    pub fn new(label: &str, now: f64) -> Self {
        let dir = TestDir::new(label);
        let clock = ManualClock::new(now);
        let store = Store::open(dir.path(), clock.clock()).expect("open store");
        Self { dir, clock, store }
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
