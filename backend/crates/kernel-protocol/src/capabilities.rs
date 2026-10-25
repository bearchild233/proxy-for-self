use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 维护能力与普通路由归属授权相互独立，只授予 account-warmup。
pub const ACCOUNT_WARMUP_PLUGIN: &str = "account-warmup";
pub const ACCOUNT_WARMUP_READ: &str = "accounts.warmup.read";
pub const ACCOUNT_WARMUP_RUN: &str = "accounts.warmup.run";

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountWarmupReadParams {
    pub ids: Option<Vec<String>>,
    pub model: Option<String>,
    #[serde(default)]
    pub include_models: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountWarmupRunParams {
    pub account_id: String,
    pub model: String,
    pub timeout_ms: u64,
    pub attempt_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountWarmupShortTerm {
    pub observed_at: Option<String>,
    pub window_seconds: u64,
    pub used_percent: Option<f64>,
    pub reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountWarmupModel {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountWarmupAccount {
    pub id: String,
    pub provider: String,
    pub authentication_kind: String,
    pub enabled: bool,
    pub status: String,
    pub supported: bool,
    pub reason_code: Option<String>,
    pub model_allowed: Option<bool>,
    pub models: Option<Vec<AccountWarmupModel>>,
    pub short_term: Option<AccountWarmupShortTerm>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountWarmupReadResult {
    pub accounts: Vec<AccountWarmupAccount>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountWarmupOutcome {
    Accepted,
    Skipped,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountWarmupSendState {
    NotSent,
    Sent,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountWarmupRunResult {
    pub outcome: AccountWarmupOutcome,
    pub reason_code: Option<String>,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub send_state: AccountWarmupSendState,
    pub short_term: Option<AccountWarmupShortTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProbeParams {
    pub account_id: String,
    pub model: String,
    pub turn_state: Option<String>,
    pub cookie: Option<String>,
    #[serde(default)]
    pub include_account_cookies: bool,
    pub timeout_ms: u64,
    /// 同一次打票的 bootstrap 与重放共用的上游请求 ID；省略时内核为每次探测生成新的。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamProbeResult {
    pub status: Option<u16>,
    pub error_kind: Option<String>,
    pub state_returned: bool,
    pub turn_state: Option<String>,
    pub set_cookies: Vec<String>,
    pub rate_limit_headers: Vec<(String, String)>,
    pub retry_after_ms: Option<u64>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum Egress {
    Direct,
    Account { account_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpFetchParams {
    pub url: String,
    pub method: HttpMethod,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub egress: Egress,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpFetchResult {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsReadParams {
    pub ids: Option<Vec<String>>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSummary {
    pub id: String,
    pub enabled: bool,
    pub status: String,
    pub authentication_kind: String,
    pub groups: Vec<String>,
    pub upstream_account_id: Option<String>,
    pub user_id: Option<String>,
    pub model_allowed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountsReadResult {
    pub accounts: Vec<AccountSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupsMoveParams {
    pub account_id: String,
    pub remove_group: Option<String>,
    pub add_group: String,
    pub reason: crate::Reason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveOutcome {
    Moved,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupsMoveResult {
    pub outcome: MoveOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaReadParams {
    pub account_id: String,
    #[serde(default)]
    pub refresh_if_missing: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaReadResult {
    pub snapshot: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogParams {
    pub level: String,
    pub message: String,
    pub fields: Value,
}
