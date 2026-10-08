//! 手动同步的外部价目来源；网络和外部文档解析由 Host 实现。

use crate::model::{AdminError, pricing::PricingSyncPreview};
use async_trait::async_trait;

#[async_trait]
pub trait PricingSource: Send + Sync {
    async fn fetch(&self) -> Result<PricingSyncPreview, AdminError>;
}

/// 插件决定同步时机并保存执行状态；网关只执行经过验证的价目写入。
#[async_trait]
pub trait PricingSyncPolicy: Send + Sync {
    async fn decide(
        &self,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, gateway_core::task::WorkerTaskError>;
}
