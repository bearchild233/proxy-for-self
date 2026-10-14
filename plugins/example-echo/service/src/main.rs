use std::time::Duration;

use plugin_sdk::{Context, Descriptor, Plugin, RpcResult, Server, Settings, protocol::*};

struct Echo;

impl Plugin for Echo {
    async fn route_select(
        &self,
        _ctx: Context,
        params: RouteSelectParams,
    ) -> RpcResult<RouteSelectResult> {
        Ok(RouteSelectResult {
            preferred: params.accounts,
            live: vec![],
            blocked: vec![],
            pinned_decision: params.pinned.map(|_| PinnedDecision::Stay),
        })
    }
    async fn route_prepare(
        &self,
        _ctx: Context,
        _params: RoutePrepareParams,
    ) -> RpcResult<RoutePrepareResult> {
        Ok(RoutePrepareResult::Fail {
            reason: Reason {
                code: "not_supported".into(),
                message: "not_supported".into(),
            },
        })
    }

    async fn response_head(
        &self,
        _ctx: Context,
        _params: ResponseHeadParams,
    ) -> RpcResult<ResponseHeadResult> {
        Ok(ResponseHeadResult::Accept)
    }

    async fn admin_call(
        &self,
        _ctx: Context,
        params: AdminCallParams,
    ) -> RpcResult<serde_json::Value> {
        Ok(params.params)
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TODO(design): 示例插件的发布关停预算尚未指定，调用方必须显式提供。
    let shutdown_ms: u64 = std::env::var("CPR_PLUGIN_SHUTDOWN_MS")?.parse()?;
    let descriptor = Descriptor {
        plugin: PluginInfo {
            id: "example-echo".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        kernel_api: PluginApi {
            major: KERNEL_API.0,
            // 管理连接（hello.role = admin）自 0.5 起存在；清单 kernelApi.minMinor 与此保持一致。
            min_minor: 5,
        },
        hooks: vec![
            "route.select".into(),
            "route.prepare".into(),
            "response.head".into(),
            "admin.call".into(),
        ],
        capabilities: vec!["log".into()],
        owned_groups: vec![],
        watched_groups: vec![],
        shutdown_timeout: Duration::from_millis(shutdown_ms),
    };
    Server::bind(Settings::from_env()?, descriptor, Echo)?
        .run()
        .await?;
    Ok(())
}
