use std::time::Duration;

use gateway_pool::{CAPABILITIES, GatewayPoolService, HOOKS, Store, system_clock};
use plugin_sdk::{Descriptor, Server, Settings, protocol::*};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 关停预算由调用方经环境变量提供，与示例插件的约定一致。
    let shutdown_ms: u64 = std::env::var("CPR_PLUGIN_SHUTDOWN_MS")?.parse()?;
    let settings = Settings::from_env()?;
    let store = Store::open(&settings.data, system_clock())?;
    // 启动时按已保存设置声明归属，之后设置变化由管理方法广播。
    let ownership = store.ownership()?;
    let descriptor = Descriptor {
        plugin: PluginInfo {
            id: "gateway-pool".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        kernel_api: PluginApi {
            major: KERNEL_API.0,
            // 资格检查与 SSE 观察依赖协议 0.6。
            min_minor: 6,
        },
        hooks: HOOKS.map(str::to_owned).to_vec(),
        // 对外访问全部经内核能力，与清单里的 service.capabilities 一致。
        capabilities: CAPABILITIES.map(str::to_owned).to_vec(),
        owned_groups: ownership.owned_groups,
        watched_groups: ownership.watched_groups,
        shutdown_timeout: Duration::from_millis(shutdown_ms),
    };
    Server::bind(settings, descriptor, GatewayPoolService::new(store))?
        .run()
        .await?;
    Ok(())
}
