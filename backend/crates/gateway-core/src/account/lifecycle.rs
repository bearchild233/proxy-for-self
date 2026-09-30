//! 账号生命周期与临期调度事实。
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::AccountWeight;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AccountLifecycle {
    pub expiry_priority: bool,
    pub archived: bool,
    pub archive_reason: Option<String>,
    pub archived_at: Option<DateTime<Utc>>,
    pub subscription_expires_at: Option<DateTime<Utc>>,
    pub subscription_observed_at: Option<DateTime<Utc>>,
}

impl AccountLifecycle {
    pub const EMPTY: Self = Self {
        expiry_priority: false,
        archived: false,
        archive_reason: None,
        archived_at: None,
        subscription_expires_at: None,
        subscription_observed_at: None,
    };

    /// 套餐和订阅事实同时满足条件才提权；基础权重始终保留。
    #[must_use]
    pub fn effective_weight(
        &self,
        base: AccountWeight,
        is_plus: bool,
        now: DateTime<Utc>,
    ) -> AccountWeight {
        let fresh = self
            .subscription_observed_at
            .is_some_and(|at| at <= now && now - at <= Duration::hours(24));
        let expiring = self
            .subscription_expires_at
            .is_some_and(|at| at > now && at <= now + Duration::days(5));
        if self.expiry_priority && !self.archived && is_plus && fresh && expiring {
            AccountWeight::new(AccountWeight::MAX).expect("maximum account weight")
        } else {
            base
        }
    }
}
