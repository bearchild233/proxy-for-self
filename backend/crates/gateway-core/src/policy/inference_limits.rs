//! 实例入口容量；数据库覆盖启动默认值，零表示该维度不限制。
#[derive(Debug, Clone, Copy, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct InferenceLimits {
    #[serde(alias = "max_requests")]
    pub max_requests: u16,
    #[serde(alias = "max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(alias = "max_in_flight_body_bytes")]
    pub max_in_flight_body_bytes: u32,
}
impl InferenceLimits {
    #[must_use]
    pub fn is_valid(self) -> bool {
        u32::try_from(self.max_body_bytes).is_ok()
            && (self.max_in_flight_body_bytes == 0
                || (self.max_body_bytes > 0
                    && self.max_body_bytes <= self.max_in_flight_body_bytes as usize))
    }
    #[must_use]
    pub const fn body_limit(self) -> usize {
        if self.max_body_bytes == 0 {
            usize::MAX
        } else {
            self.max_body_bytes
        }
    }
}
