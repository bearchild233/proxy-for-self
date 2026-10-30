use std::time::Duration;

use account_warmup::{Store, WarmupService};
use plugin_sdk::{
    Descriptor, Plugin, Server, Settings,
    protocol::{KERNEL_API, PluginApi, PluginInfo},
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let shutdown_ms: u64 = std::env::var("CPR_PLUGIN_SHUTDOWN_MS")?.parse()?;
    let settings = Settings::from_env()?;
    let store =
        Store::open(&settings.data).map_err(|error| std::io::Error::other(error.message))?;
    let service = WarmupService::new(store);
    let descriptor = Descriptor {
        plugin: PluginInfo {
            id: "account-warmup".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        kernel_api: PluginApi {
            major: KERNEL_API.0,
            min_minor: 8,
        },
        hooks: vec!["admin.call".into()],
        capabilities: vec![
            "accounts.warmup.read".into(),
            "accounts.warmup.run".into(),
            "log".into(),
        ],
        owned_groups: vec![],
        watched_groups: vec![],
        shutdown_timeout: Duration::from_millis(shutdown_ms),
    };
    let server = Server::bind(settings, descriptor, service.clone())?;
    let stopping = service.clone();
    let budget = Duration::from_millis(shutdown_ms.min(30000));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    // 先停认领并让后台请求收尾，再通知 SDK 关闭连接；后台能力调用不属于 SDK inFlight。
    let result = server
        .run_until(async move {
            tokio::select! { _ = terminate.recv() => {}, _ = tokio::signal::ctrl_c() => {} }
            stopping.stop_background();
            if stopping.join_background(budget).await.is_err() {
                eprintln!("account warmup background did not drain before shutdown deadline");
            }
        })
        .await;
    let drained = service.join_background(budget).await;
    result?;
    drained.map_err(|error| std::io::Error::other(error.message))?;
    Ok(())
}
