//! 服务内部错误。`Invalid` 对应 Python 的 ValueError，映射为 `invalid_params`，消息保持原文。

use std::fmt;

use plugin_sdk::protocol::{INTERNAL, INVALID_PARAMS, NOT_SUPPORTED, RpcError};

#[derive(Debug)]
pub enum Failure {
    /// 参数或状态不符合规则；消息与 Python 实现一致。
    Invalid(String),
    /// 未注册的管理方法。
    NotSupported(String),
    /// 数据库、序列化等内部故障。
    Internal(String),
}

impl Failure {
    pub fn invalid<T>(message: &str) -> Result<T, Self> {
        Err(Self::Invalid(message.to_owned()))
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) | Self::NotSupported(message) | Self::Internal(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl std::error::Error for Failure {}

impl From<rusqlite::Error> for Failure {
    fn from(error: rusqlite::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Internal(error.to_string())
    }
}

impl From<Failure> for RpcError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Invalid(message) => RpcError::new(INVALID_PARAMS, message),
            Failure::NotSupported(method) => RpcError::new(NOT_SUPPORTED, method),
            Failure::Internal(message) => RpcError::new(INTERNAL, message),
        }
    }
}
