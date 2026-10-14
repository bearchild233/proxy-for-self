use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePrepareParams {
    pub request_id: String,
    pub attempt: u32,
    pub model: String,
    pub deadline_unix_ms: u64,
    pub replay_safe: bool,
    pub accounts: Vec<String>,
    pub ordinary_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Upstream {
    pub cookie: Option<String>,
    pub turn_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum RoutePrepareResult {
    Use {
        account_id: String,
        lease_id: String,
        upstream: Upstream,
    },
    Wait {
        retry_after_ms: u64,
        reason: Reason,
    },
    Fallback {
        reason: Reason,
    },
    Fail {
        reason: Reason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseHeadParams {
    pub request_id: String,
    pub lease_id: String,
    pub attempt: u32,
    pub status: Option<u16>,
    pub error_kind: Option<String>,
    pub state_returned: bool,
    pub turn_state: Option<String>,
    pub set_cookies: Vec<String>,
    pub duration_ms: u64,
    pub replay_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum ResponseHeadResult {
    Accept,
    Retry { delay_ms: u64 },
    Switch,
    Fail { reason: Reason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestOutcome {
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestFinishParams {
    pub request_id: String,
    pub lease_id: String,
    pub outcome: RequestOutcome,
    pub duration_ms: u64,
    pub usage: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminCallParams {
    pub method: String,
    pub params: Value,
    pub actor: Actor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseObserveParams {
    pub request_id: String,
    pub account_id: String,
    pub attempt: u32,
    pub model: String,
    pub request_had_turn_state: bool,
    pub status: Option<u16>,
    pub error_kind: Option<String>,
    pub state_returned: bool,
    pub turn_state: Option<String>,
    pub set_cookies: Vec<String>,
    pub replay_safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase")]
pub enum ResponseObserveResult {
    Accept,
    SwitchAccount { reason: Reason },
}
