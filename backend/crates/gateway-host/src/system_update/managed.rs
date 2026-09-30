//! 私有 Unix socket 的固定协议；不传 shell 命令、路径、凭据或任意下载地址。
use super::{OperationError, SystemUpdateConfig, conflict, internal};
use gateway_admin::model::system::{
    SystemOperationKind, SystemOperationState, SystemOperationStatus, SystemUpdateStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

fn client(config: &SystemUpdateConfig) -> Result<reqwest::Client, OperationError> {
    let socket = config
        .managed_socket
        .as_deref()
        .ok_or_else(|| conflict("managed updater is not configured"))?;
    #[cfg(unix)]
    {
        reqwest::Client::builder()
            .unix_socket(socket)
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| internal("cannot create updater client"))
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        Err(conflict("managed updater requires a Unix host"))
    }
}

async fn response(
    response: Result<reqwest::Response, reqwest::Error>,
) -> Result<Value, OperationError> {
    let mut response =
        response.map_err(|_| conflict("独立更新服务暂不可用，网关未执行进程替换"))?;
    if !response.status().is_success() {
        return Err(conflict(
            "更新服务拒绝操作：可能已有任务运行或需要维护者检查",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| internal("failed to read updater response"))?
    {
        if bytes.len() + chunk.len() > 16384 {
            return Err(internal("updater response exceeds limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| internal("invalid updater response"))
}

pub(super) async fn submit(
    config: &SystemUpdateConfig,
    action: &str,
    target: Option<&str>,
) -> Result<String, OperationError> {
    let value = response(
        client(config)?
            .post("http://localhost/operation")
            .json(&json!({"action":action, "target_version":target}))
            .send()
            .await,
    )
    .await?;
    value["operation_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| internal("updater did not confirm operation ID"))
}

#[derive(Deserialize)]
struct Status {
    previous_version: Option<String>,
    current_version: Option<String>,
    operation: Operation,
}

#[derive(Deserialize)]
struct Operation {
    operation_id: Option<String>,
    kind: Option<String>,
    status: String,
    target_version: Option<String>,
    message: Option<String>,
    error: Option<String>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(super) async fn status(
    config: &SystemUpdateConfig,
) -> Result<SystemUpdateStatus, OperationError> {
    let value = response(client(config)?.get("http://localhost/status").send().await).await?;
    let status: Status =
        serde_json::from_value(value).map_err(|_| internal("invalid managed updater status"))?;
    let operation = status.operation;
    Ok(SystemUpdateStatus {
        previous_version: status.previous_version,
        current_version: status.current_version,
        need_restart: false,
        operation: SystemOperationState {
            operation_id: operation.operation_id,
            kind: match operation.kind.as_deref() {
                None => None,
                Some("update") => Some(SystemOperationKind::Update),
                Some("rollback") => Some(SystemOperationKind::Rollback),
                Some("restart") => Some(SystemOperationKind::Restart),
                _ => return Err(internal("invalid updater operation kind")),
            },
            status: match operation.status.as_str() {
                "idle" => SystemOperationStatus::Idle,
                "running" => SystemOperationStatus::Running,
                "succeeded" => SystemOperationStatus::Succeeded,
                "failed" => SystemOperationStatus::Failed,
                _ => return Err(internal("invalid updater operation status")),
            },
            target_version: operation.target_version,
            message: operation.message,
            error: operation.error,
            started_at: operation.started_at,
            finished_at: operation.finished_at,
        },
    })
}

pub(super) async fn plugins(
    config: &SystemUpdateConfig,
) -> Result<Vec<gateway_admin::model::system::PluginStatus>, OperationError> {
    let value = response(client(config)?.get("http://localhost/plugins").send().await).await?;
    serde_json::from_value(value).map_err(|_| internal("invalid plugin status"))
}

pub(super) async fn plugin_action(
    config: &SystemUpdateConfig,
    id: &str,
    action: gateway_admin::model::system::PluginAction,
) -> Result<String, OperationError> {
    let value = response(
        client(config)?
            .post("http://localhost/plugins/action")
            .json(&json!({"id": id, "action": action}))
            .send()
            .await,
    )
    .await?;
    value["operation_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| internal("plugin operation not confirmed"))
}
