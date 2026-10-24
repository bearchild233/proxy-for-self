//! 管理员的养号准入政策，与上游真实额度事实分开保存。
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountQuotaProtection {
    enabled: bool,
    used_percent_limit: u8,
    short_term_used_percent_limit: u8,
    weekly_used_percent_limit: u8,
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
        short_term_used_percent_limit: 90,
        weekly_used_percent_limit: 90,
    };
    pub fn new(enabled: bool, used_percent_limit: u8) -> Option<Self> {
        (1..=99).contains(&used_percent_limit).then_some(Self {
            enabled,
            used_percent_limit,
            short_term_used_percent_limit: used_percent_limit,
            weekly_used_percent_limit: used_percent_limit,
        })
    }

    pub fn with_window_limits(mut self, short_term: u8, weekly: u8) -> Option<Self> {
        if !(1..=99).contains(&short_term) || !(1..=99).contains(&weekly) {
            return None;
        }
        self.short_term_used_percent_limit = short_term;
        self.weekly_used_percent_limit = weekly;
        Some(self)
    }

    pub const fn enabled(self) -> bool {
        self.enabled
    }
    pub const fn used_percent_limit(self) -> u8 {
        self.used_percent_limit
    }

    pub const fn short_term_used_percent_limit(self) -> u8 {
        self.short_term_used_percent_limit
    }

    pub const fn weekly_used_percent_limit(self) -> u8 {
        self.weekly_used_percent_limit
    }

    pub fn reached_usage(self, usage: AccountQuotaProtectionUsage) -> bool {
        self.enabled
            && (usage
                .short_term
                .is_some_and(|used| used >= self.short_term_used_percent_limit)
                || usage
                    .weekly
                    .is_some_and(|used| used >= self.weekly_used_percent_limit)
                || usage
                    .other
                    .is_some_and(|used| used >= self.used_percent_limit))
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
            #[serde(default, deserialize_with = "present_limit")]
            short_term_used_percent_limit: Option<u8>,
            #[serde(default, deserialize_with = "present_limit")]
            weekly_used_percent_limit: Option<u8>,
        }
        let wire = Wire::deserialize(deserializer)?;
        let legacy = Self::new(wire.enabled, wire.used_percent_limit).ok_or_else(|| {
            serde::de::Error::custom("usedPercentLimit must be an integer from 1 to 99")
        })?;
        match (
            wire.short_term_used_percent_limit,
            wire.weekly_used_percent_limit,
        ) {
            (None, None) => Ok(legacy),
            (Some(short_term), Some(weekly)) => legacy
                .with_window_limits(short_term, weekly)
                .ok_or_else(|| {
                    serde::de::Error::custom("window limits must be integers from 1 to 99")
                }),
            _ => Err(serde::de::Error::custom(
                "shortTermUsedPercentLimit and weeklyUsedPercentLimit must be supplied together",
            )),
        }
    }
}

fn present_limit<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u8>, D::Error> {
    u8::deserialize(deserializer).map(Some)
}

/// Provider 按实际周期分类的有效使用率；其它窗口保留原有统一阈值语义。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AccountQuotaProtectionUsage {
    short_term: Option<u8>,
    weekly: Option<u8>,
    other: Option<u8>,
}

impl AccountQuotaProtectionUsage {
    pub const UNKNOWN: Self = Self {
        short_term: None,
        weekly: None,
        other: None,
    };

    pub fn new(short_term: Option<f64>, weekly: Option<f64>, other: Option<f64>) -> Self {
        // 阈值为整数，向下取整防止89.999%被排序rank的四舍五入提前判为90%。
        fn normalize(used: Option<f64>) -> Option<u8> {
            used.filter(|used| used.is_finite() && *used >= 0.0)
                .map(|used| used.min(100.0).floor() as u8)
        }
        Self {
            short_term: normalize(short_term),
            weekly: normalize(weekly),
            other: normalize(other),
        }
    }

    pub fn short_term_used_percent(self) -> Option<f64> {
        self.short_term.map(f64::from)
    }
    pub fn weekly_used_percent(self) -> Option<f64> {
        self.weekly.map(f64::from)
    }
    pub fn other_used_percent(self) -> Option<f64> {
        self.other.map(f64::from)
    }
    pub fn max_used_percent(self) -> Option<f64> {
        [self.short_term, self.weekly, self.other]
            .into_iter()
            .flatten()
            .max()
            .map(f64::from)
    }
}
