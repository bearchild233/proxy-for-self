//! 管理员的养号准入政策，与上游真实额度事实分开保存。
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaProtection {
    enabled: bool,
    used_percent_limit: u8,
}

impl Default for AccountQuotaProtection {
    fn default() -> Self {
        Self::DISABLED
    }
}

impl AccountQuotaProtection {
    pub const DISABLED: Self = Self {
        enabled: false,
        used_percent_limit: 90,
    };
    pub fn new(enabled: bool, used_percent_limit: u8) -> Option<Self> {
        (1..=99).contains(&used_percent_limit).then_some(Self {
            enabled,
            used_percent_limit,
        })
    }

    pub const fn enabled(self) -> bool {
        self.enabled
    }
    pub const fn used_percent_limit(self) -> u8 {
        self.used_percent_limit
    }

    /// 仅使用 Provider 确认仍有效的账号级窗口；未知不伪造为耗尽。
    pub fn reached(self, used_percent: Option<f64>) -> bool {
        self.enabled
            && used_percent
                .is_some_and(|used| used.is_finite() && used >= f64::from(self.used_percent_limit))
    }
}

impl<'de> Deserialize<'de> for AccountQuotaProtection {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Wire {
            enabled: bool,
            used_percent_limit: u8,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.enabled, wire.used_percent_limit).ok_or_else(|| {
            serde::de::Error::custom("usedPercentLimit must be an integer from 1 to 99")
        })
    }
}
