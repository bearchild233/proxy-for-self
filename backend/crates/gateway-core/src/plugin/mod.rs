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

/// 监控规则：`*` 表示所有不归任何插件所有的账号，`!分组` 表示带该分组的账号不监控（优先于其余规则），
/// 其余条目是具体分组。调用方负责先排除归其他插件所有的账号。
pub fn watch_matches<G: AsRef<str>>(rules: &[String], account_groups: &[G]) -> bool {
    let has = |group: &str| account_groups.iter().any(|own| own.as_ref() == group);
    if rules
        .iter()
        .filter_map(|rule| rule.strip_prefix('!'))
        .any(has)
    {
        return false;
    }
    rules.iter().any(|rule| rule == "*" || has(rule))
}

#[async_trait]
pub trait PluginHooks: Send + Sync {
    fn ownership(&self, group: &str) -> Ownership;
    /// 不归任何插件所有的账号由哪些插件监控；默认只按逐个分组的 `Watched` 汇总。
    fn watchers(&self, account_groups: &[String]) -> Vec<String> {
        let mut plugins = Vec::new();
        for group in account_groups {
            if let Ownership::Watched { plugins: found } = self.ownership(group) {
                plugins.extend(found);
            }
        }
        plugins.sort();
        plugins.dedup();
        plugins
    }
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
    /// 能力调用能否访问带这些分组的账号；默认按 `groups()` 求交集。
    fn allows(&self, account_groups: &[String]) -> bool {
        let groups = self.groups();
        account_groups.iter().any(|group| groups.contains(group))
    }
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
