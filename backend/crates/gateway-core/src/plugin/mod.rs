//! Provider 无关的插件钩子与能力端口。
use async_trait::async_trait;
use serde_json::Value;

pub use kernel_protocol::*;
mod capabilities;
pub use capabilities::*;

/// 同一请求跨账号及 Provider attempt 共享发送预算，插件不能重置内核上限。
#[derive(Debug, Clone, Default)]
pub struct PluginAttemptBudget(std::sync::Arc<std::sync::atomic::AtomicU32>);
impl PluginAttemptBudget {
    pub fn claim(&self) -> Option<u32> {
        use std::sync::atomic::Ordering;
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < 4).then_some(used + 1)
            })
            .ok()
            .map(|used| used + 1)
    }
    pub fn available(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed) < 4
    }
    pub fn next_attempt(&self) -> u32 {
        self.0.load(std::sync::atomic::Ordering::Relaxed) + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ownership {
    Owned { plugin: String, available: bool },
    Watched { plugins: Vec<String> },
    NotOwned,
}

#[async_trait]
pub trait PluginHooks: Send + Sync {
    fn ownership(&self, group: &str) -> Ownership;
    async fn route_select(
        &self,
        plugin: &str,
        params: RouteSelectParams,
    ) -> Result<RouteSelectResult, RpcError>;
    async fn response_observe(
        &self,
        plugin: &str,
        params: ResponseObserveParams,
    ) -> Result<ResponseObserveResult, RpcError>;
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
    async fn call(
        &self,
        plugin: &dyn PluginAccess,
        method: &str,
        params: Value,
    ) -> Result<Value, RpcError>;
}

/// 每次读取都取当前注册表与归属，排队中的调用不能继续使用已撤销的分组。
pub trait PluginAccess: Send + Sync {
    fn id(&self) -> &str;
    fn groups(&self) -> Vec<String>;
    fn fetch_hosts(&self) -> Vec<String>;
}

#[async_trait]
pub trait PluginAccounts: Send + Sync {
    async fn read(
        &self,
        ids: Option<&[String]>,
        model: Option<&str>,
    ) -> Result<Vec<AccountSummary>, RpcError>;
}

#[async_trait]
pub trait PluginUpstream: Send + Sync {
    async fn quota(&self, params: QuotaReadParams) -> Result<QuotaReadResult, RpcError>;
    async fn probe(
        &self,
        access: &dyn PluginAccess,
        accounts: &dyn PluginAccounts,
        params: UpstreamProbeParams,
    ) -> Result<UpstreamProbeResult, RpcError>;
    async fn fetch(&self, params: HttpFetchParams) -> Result<HttpFetchResult, RpcError>;
}
