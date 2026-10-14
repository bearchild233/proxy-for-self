use async_trait::async_trait;
use gateway_core::plugin::PluginCapabilities;
use kernel_protocol::{INVALID_PARAMS, LogParams, NOT_SUPPORTED, RpcError};
use serde_json::Value;

pub struct LogCapabilities;

#[async_trait]
impl PluginCapabilities for LogCapabilities {
    async fn call(&self, plugin: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        if method != "log" {
            return Err(RpcError::new(NOT_SUPPORTED, method));
        }
        let mut log: LogParams = serde_json::from_value(params)
            .map_err(|e| RpcError::new(INVALID_PARAMS, e.to_string()))?;
        // 按 UTF-8 边界截断，保证多字节消息不会导致 panic 或超过 4 KiB。
        let mut end = log.message.len().min(4096);
        while !log.message.is_char_boundary(end) {
            end -= 1;
        }
        log.message.truncate(end);
        match log.level.as_str() {
            "debug" => tracing::debug!(plugin_id = plugin, fields = %log.fields, "{}", log.message),
            "warn" => tracing::warn!(plugin_id = plugin, fields = %log.fields, "{}", log.message),
            "error" => tracing::error!(plugin_id = plugin, fields = %log.fields, "{}", log.message),
            _ => tracing::info!(plugin_id = plugin, fields = %log.fields, "{}", log.message),
        }
        Ok(Value::Null)
    }
}
