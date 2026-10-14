use std::{future::Future, path::PathBuf, time::Duration};

use crate::{Context, RpcResult, protocol::*};

/// 关停预算没有默认值，必须由插件描述提供。
#[derive(Clone)]
pub struct Descriptor {
    pub plugin: PluginInfo,
    pub kernel_api: PluginApi,
    pub hooks: Vec<String>,
    pub capabilities: Vec<String>,
    pub owned_groups: Vec<String>,
    pub watched_groups: Vec<String>,
    pub shutdown_timeout: Duration,
}

impl Descriptor {
    pub fn hello(&self) -> HelloResult {
        HelloResult {
            plugin: self.plugin.clone(),
            sdk: SdkInfo {
                version: env!("CARGO_PKG_VERSION").into(),
            },
            kernel_api: self.kernel_api,
            hooks: self.hooks.clone(),
            capabilities: self.capabilities.clone(),
            owned_groups: self.owned_groups.clone(),
            watched_groups: self.watched_groups.clone(),
        }
    }
}

#[derive(Clone)]
pub struct Settings {
    pub socket: PathBuf,
    pub data: PathBuf,
}

impl Settings {
    pub fn from_env() -> std::io::Result<Self> {
        let required = |name| {
            std::env::var_os(name)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, name))
        };
        Ok(Self {
            socket: required("CPR_PLUGIN_SOCKET")?,
            data: required("CPR_PLUGIN_DATA")?,
        })
    }
}

pub trait Plugin: Send + Sync + 'static {
    fn health(&self, _ctx: Context) -> impl Future<Output = RpcResult<HealthResult>> + Send {
        async {
            Ok(HealthResult {
                ready: true,
                detail: None,
            })
        }
    }

    fn route_prepare(
        &self,
        _ctx: Context,
        _params: RoutePrepareParams,
    ) -> impl Future<Output = RpcResult<RoutePrepareResult>> + Send {
        async { Err(RpcError::new(NOT_SUPPORTED, "route.prepare")) }
    }

    fn response_head(
        &self,
        _ctx: Context,
        _params: ResponseHeadParams,
    ) -> impl Future<Output = RpcResult<ResponseHeadResult>> + Send {
        async { Err(RpcError::new(NOT_SUPPORTED, "response.head")) }
    }

    fn response_observe(
        &self,
        _ctx: Context,
        _params: ResponseObserveParams,
    ) -> impl Future<Output = RpcResult<ResponseObserveResult>> + Send {
        async { Ok(ResponseObserveResult::Accept) }
    }

    fn request_finish(
        &self,
        _ctx: Context,
        _params: RequestFinishParams,
    ) -> impl Future<Output = RpcResult<()>> + Send {
        async { Err(RpcError::new(NOT_SUPPORTED, "request.finish")) }
    }

    fn admin_call(
        &self,
        _ctx: Context,
        _params: AdminCallParams,
    ) -> impl Future<Output = RpcResult<serde_json::Value>> + Send {
        async { Err(RpcError::new(NOT_SUPPORTED, "admin.call")) }
    }
}
