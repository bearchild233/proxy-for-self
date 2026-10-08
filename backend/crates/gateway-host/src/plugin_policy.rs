//! 可选插件策略进程。仅传递脱敏数据，独立版本切换不影响主网关。
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use async_trait::async_trait;
use gateway_admin::ports::backup::BackupPolicyPort;
use gateway_core::task::WorkerTaskError;
use serde_json::Value;
use tokio::{io::AsyncWriteExt, process::Command};

struct ProcessPolicy {
    plugin: &'static str,
    runner: PathBuf,
    root: PathBuf,
}

/// 非插件部署未配置时沿用既有策略；插件部署必须同时设置两个路径。
#[must_use]
pub fn from_environment() -> Option<Arc<dyn BackupPolicyPort>> {
    let runner = std::env::var_os("CPR_PLUGIN_POLICY_RUNNER")?;
    let root = std::env::var_os("CPR_PLUGIN_ROOT")?;
    Some(Arc::new(ProcessPolicy {
        plugin: "backup",
        runner: runner.into(),
        root: root.into(),
    }))
}

#[async_trait]
impl BackupPolicyPort for ProcessPolicy {
    async fn decide(&self, input: Value) -> Result<Value, WorkerTaskError> {
        self.run(input).await
    }
}

#[must_use]
pub fn pricing_from_environment()
-> Option<Arc<dyn gateway_admin::ports::pricing::PricingSyncPolicy>> {
    Some(Arc::new(ProcessPolicy {
        plugin: "pricing",
        runner: std::env::var_os("CPR_PLUGIN_POLICY_RUNNER")?.into(),
        root: std::env::var_os("CPR_PLUGIN_ROOT")?.into(),
    }))
}

#[async_trait]
impl gateway_admin::ports::pricing::PricingSyncPolicy for ProcessPolicy {
    async fn decide(&self, input: Value) -> Result<Value, WorkerTaskError> {
        self.run(input).await
    }
}

impl ProcessPolicy {
    async fn run(&self, input: Value) -> Result<Value, WorkerTaskError> {
        let failure = || WorkerTaskError::safe("plugin policy unavailable");
        let mut child = Command::new("/usr/bin/python3")
            .arg("-I")
            .arg(&self.runner)
            .arg("--root")
            .arg(&self.root)
            .arg("--plugin")
            .arg(self.plugin)
            .env_clear()
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| failure())?;
        let mut stdin = child.stdin.take().ok_or_else(failure)?;
        stdin
            .write_all(input.to_string().as_bytes())
            .await
            .map_err(|_| failure())?;
        drop(stdin);
        let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
            .await
            .map_err(|_| failure())?
            .map_err(|_| failure())?;
        if !output.status.success() || output.stdout.len() > 131_072 {
            return Err(failure());
        }
        serde_json::from_slice(&output.stdout).map_err(|_| failure())
    }
}
