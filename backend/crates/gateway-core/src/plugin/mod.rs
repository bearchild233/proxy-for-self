//! Provider 无关的插件端口；这里只定义合同，不进入现有推理路径。
use async_trait::async_trait;
use serde_json::Value;

pub use kernel_protocol::{
    AdminCallParams, RequestFinishParams, ResponseHeadParams, ResponseHeadResult,
    RoutePrepareParams, RoutePrepareResult, RpcError,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    Owned { plugin: String, available: bool },
    NotOwned,
}

#[async_trait]
pub trait PluginHooks: Send + Sync {
    fn ownership(&self, group: &str) -> Ownership;
    async fn route_prepare(
        &self,
        plugin: &str,
        params: RoutePrepareParams,
    ) -> Result<RoutePrepareResult, RpcError>;
    async fn response_head(
        &self,
        plugin: &str,
        params: ResponseHeadParams,
    ) -> Result<ResponseHeadResult, RpcError>;
    async fn request_finish(&self, plugin: &str, params: RequestFinishParams);
}

#[async_trait]
pub trait PluginCapabilities: Send + Sync {
    /// 身份由连接确定；运行时先授权，端口只负责按方法实现能力。
    async fn call(&self, plugin: &str, method: &str, params: Value) -> Result<Value, RpcError>;
}
