use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountOverride {
    pub account_id: String,
    pub enabled: bool,
    pub daily_time: Option<String>,
    pub model: Option<String>,
    pub reset_delay_seconds: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub enabled: bool,
    pub timezone: String,
    pub daily_enabled: bool,
    pub daily_time: String,
    pub reset_enabled: bool,
    pub reset_delay_seconds: u32,
    pub jitter_min_seconds: u32,
    pub jitter_max_seconds: u32,
    pub target_mode: String,
    pub account_ids: Vec<String>,
    pub default_model: String,
    pub account_overrides: Vec<AccountOverride>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            timezone: "Asia/Shanghai".into(),
            daily_enabled: false,
            daily_time: "08:00".into(),
            reset_enabled: true,
            reset_delay_seconds: 30,
            jitter_min_seconds: 5,
            jitter_max_seconds: 15,
            target_mode: "all".into(),
            account_ids: vec![],
            default_model: "gpt-6.1-sol".into(),
            account_overrides: vec![],
        }
    }
}

impl Settings {
    pub fn account_override(&self, id: &str) -> Option<&AccountOverride> {
        self.account_overrides
            .iter()
            .find(|item| item.account_id == id)
    }

    pub fn model(&self, id: &str) -> &str {
        self.account_override(id)
            .and_then(|item| item.model.as_deref())
            .unwrap_or(&self.default_model)
    }

    pub fn automatic(&self, id: &str) -> bool {
        self.enabled
            && (self.target_mode == "all" || self.account_ids.iter().any(|item| item == id))
            && self.account_override(id).is_none_or(|item| item.enabled)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortTerm {
    pub observed_at: Option<DateTime<Utc>>,
    pub window_seconds: u64,
    pub used_percent: Option<f64>,
    pub reset_at: Option<DateTime<Utc>>,
}

impl ShortTerm {
    pub fn observed_reset(&self) -> Option<DateTime<Utc>> {
        let observed = self.observed_at?;
        let reset = self.reset_at?;
        (self.window_seconds.abs_diff(18000) <= 900 && reset > observed).then_some(reset)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub authentication_kind: String,
    pub enabled: bool,
    pub status: String,
    pub supported: bool,
    pub reason_code: Option<String>,
    pub model_allowed: Option<bool>,
    pub models: Option<Vec<Model>>,
    pub short_term: Option<ShortTerm>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Directory {
    pub accounts: Vec<Account>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub outcome: String,
    pub reason_code: Option<String>,
    pub status: Option<u16>,
    pub duration_ms: u64,
    pub send_state: String,
    pub short_term: Option<ShortTerm>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskItem {
    pub id: String,
    pub account_id: String,
    pub source: String,
    pub model: String,
    pub status: String,
    pub scheduled_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub reason_code: Option<String>,
    pub http_status: Option<u16>,
    pub reset_at: Option<DateTime<Utc>>,
    pub timer_confirmed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    pub id: String,
    pub source: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub total: u32,
    pub accepted: u32,
    pub failed: u32,
    pub skipped: u32,
    pub unknown: u32,
    pub pending: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadParams {
    pub account_ids: Option<Vec<String>>,
    pub include_models: Option<bool>,
    pub model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigureParams {
    pub expected_revision: u64,
    pub settings: Settings,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunParams {
    pub submission_id: String,
    pub scope: String,
    pub account_ids: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryParams {
    pub limit: Option<u32>,
    pub run_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CancelParams {
    pub run_id: String,
}
