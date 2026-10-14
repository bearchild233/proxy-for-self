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
            // 管理连接（hello.role = admin）自 0.5 起存在。
            min_minor: 5,
        },
        hooks: HOOKS.map(str::to_owned).to_vec(),
        // 对外访问全部经内核能力；清单里的 `service` 段在 012 打包时再加。
        capabilities: CAPABILITIES.map(str::to_owned).to_vec(),
        owned_groups: ownership.owned_groups,
        // 观察分组（动态分组的正常分组）要等 011 的降智观察接入后再上报；此处不上报，避免内核对这些账号调用 `response.observe`。
        watched_groups: Vec::new(),
        shutdown_timeout: Duration::from_millis(shutdown_ms),
    };
    Server::bind(settings, descriptor, GatewayPoolService::new(store))?
        .run()
        .await?;
    Ok(())
}
