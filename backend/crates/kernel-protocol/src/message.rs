use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const NOT_SUPPORTED: &str = "not_supported";
pub const INVALID_PARAMS: &str = "invalid_params";
pub const UNAVAILABLE: &str = "unavailable";
pub const TIMEOUT: &str = "timeout";
pub const FORBIDDEN: &str = "forbidden";
pub const INTERNAL: &str = "internal";
pub const KERNEL_API: (u32, u32) = (0, 5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Message {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Response {
        id: u64,
        #[serde(flatten)]
        payload: ResponsePayload,
    },
    Notify {
        method: String,
        params: Value,
    },
}

// 外部标签保留 result: null，同时区分缺失 result 的错误响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponsePayload {
    Result(Value),
    Error(RpcError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelApi {
    pub major: u32,
    pub minor: u32,
}

impl Default for KernelApi {
    fn default() -> Self {
        Self {
            major: KERNEL_API.0,
            minor: KERNEL_API.1,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginApi {
    pub major: u32,
    pub min_minor: u32,
}

pub fn is_compatible(kernel: KernelApi, plugin: PluginApi) -> bool {
    kernel.major == plugin.major
        && plugin.min_minor <= kernel.minor
        && (kernel.major != 0 || plugin.min_minor >= 4)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KernelSlot {
    A,
    B,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelInfo {
    pub version: String,
    pub slot: Option<KernelSlot>,
}

/// 连接角色（协议 0.5）。内核连接承担钩子与能力；管理连接只处理 `health` 与 `admin.call`。
/// 缺省为 `kernel`，保证不带该字段的握手按内核连接处理。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionRole {
    #[default]
    Kernel,
    Admin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloParams {
    pub kernel_api: KernelApi,
    pub kernel: KernelInfo,
    /// 协议 0.5 新增，可选；缺省按 `kernel` 处理。
    #[serde(default)]
    pub role: ConnectionRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkInfo {
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HelloResult {
    pub plugin: PluginInfo,
    pub sdk: SdkInfo,
    pub kernel_api: PluginApi,
    pub hooks: Vec<String>,
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub owned_groups: Vec<String>,
    #[serde(default)]
    pub watched_groups: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnershipChangedParams {
    #[serde(default)]
    pub owned_groups: Vec<String>,
    #[serde(default)]
    pub watched_groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmptyParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResult {
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrainResult {
    pub in_flight: u32,
}
