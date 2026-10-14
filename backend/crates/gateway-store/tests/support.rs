/// 读取集成测试基础设施地址（PostgreSQL / Redis）。
///
/// 本地未配置时返回 `None`，测试跳过；CI 中缺失则直接失败——
/// 静默跳过产生的绿灯与真实回归不可区分。
pub(crate) fn test_env(name: &str) -> Option<String> {
    let value = std::env::var(name).ok();
    assert!(
        value.is_some() || !running_in_ci(),
        "{name} must be set in CI: store integration tests are not allowed to skip silently"
    );
    value
}

/// 按 is-ci 约定判定 CI：`CI` 存在且非 falsey（`0`/`false`/空）。
/// 开发者用 `CI=false` 显式声明「非 CI」时不应把跳过变成 panic。
fn running_in_ci() -> bool {
    std::env::var("CI")
        .ok()
        .is_some_and(|value| !matches!(value.as_str(), "" | "0" | "false"))
}

/// 进程内网关实例互斥：`StoreBundle` 启动时取得 PostgreSQL 网关实例锁（`pg_try_advisory_lock(1129337426, 1)`，排他），
/// 锁随 Bundle 持有至释放。同一测试进程内并行的 Bundle 会互相争用该锁，因此取得 Bundle 的测试必须持有本守卫直到测试结束。
static GATEWAY_INSTANCE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn gateway_instance_guard() -> tokio::sync::MutexGuard<'static, ()> {
    GATEWAY_INSTANCE_LOCK.lock().await
}
