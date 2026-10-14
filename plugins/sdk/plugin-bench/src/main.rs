use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use plugin_sdk::{protocol::*, testing::MockKernel};
use tokio::{process::Command, task::JoinSet};

fn head() -> ResponseHeadParams {
    ResponseHeadParams {
        request_id: "bench".into(),
        lease_id: "lease".into(),
        attempt: 1,
        status: Some(200),
        error_kind: None,
        state_returned: false,
        turn_state: None,
        set_cookies: vec![],
        duration_ms: 0,
        replay_safe: true,
    }
}

async fn measure(
    kernel: &MockKernel,
    count: usize,
    timeout: Duration,
) -> Result<Vec<Duration>, RpcError> {
    let params = head();
    let mut samples = Vec::with_capacity(count);
    for _ in 0..count {
        let started = Instant::now();
        let result: ResponseHeadResult = kernel.call("response.head", &params, timeout).await?;
        assert_eq!(result, ResponseHeadResult::Accept);
        samples.push(started.elapsed());
    }
    Ok(samples)
}

fn report(label: &str, samples: &mut [Duration]) {
    samples.sort_unstable();
    let millis = |value: Duration| value.as_secs_f64() * 1000.0;
    println!(
        "{label}: n={} p50={:.6} ms p99={:.6} ms max={:.6} ms",
        samples.len(),
        millis(samples[samples.len().div_ceil(2) - 1]),
        millis(samples[(samples.len() * 99).div_ceil(100) - 1]),
        millis(*samples.last().expect("samples"))
    );
}

fn rss(pid: u32) -> std::io::Result<String> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))?
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .map(str::to_owned)
        .ok_or_else(|| std::io::Error::other("VmRSS missing"))
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // TODO(design): 预热次数和测量调用预算未指定；必须传参，报告记录实验取值。
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        return Err(
            "usage: plugin-bench <example-echo> <warmup-count> <call-timeout-ms> <shutdown-ms>"
                .into(),
        );
    }
    let warmup: usize = args[2].parse()?;
    let timeout = Duration::from_millis(args[3].parse()?);
    let shutdown = Duration::from_millis(args[4].parse()?);
    let directory =
        TempDir(std::env::temp_dir().join(format!("cpr-plugin-bench-{}", std::process::id())));
    std::fs::create_dir(&directory.0)?;
    let socket = directory.0.join("plugin.sock");
    let mut child = Command::new(&args[1])
        .env("CPR_PLUGIN_SOCKET", &socket)
        .env("CPR_PLUGIN_DATA", directory.0.join("data"))
        .env("CPR_PLUGIN_SHUTDOWN_MS", &args[4])
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().ok_or("missing child pid")?;
    // 无固定 sleep：启动仅等待路径出现，并受显式调用预算约束。
    tokio::time::timeout(timeout, async {
        while !socket.exists() {
            if let Some(status) = child.try_wait()? {
                return Err(std::io::Error::other(format!("plugin exited: {status}")));
            }
            tokio::task::yield_now().await;
        }
        Ok::<(), std::io::Error>(())
    })
    .await??;
    let kernel = Arc::new(
        MockKernel::connect(
            &socket,
            HelloParams {
                kernel_api: KernelApi::default(),
                kernel: KernelInfo {
                    version: "bench".into(),
                    slot: None,
                },
                role: ConnectionRole::Kernel,
            },
            timeout,
        )
        .await?,
    );
    measure(&kernel, warmup, timeout).await?;
    let before = rss(pid)?;
    let mut sequential = measure(&kernel, 10_000, timeout).await?;
    let mut tasks = JoinSet::new();
    for _ in 0..8 {
        let kernel = kernel.clone();
        tasks.spawn(async move { measure(&kernel, 2_000, timeout).await });
    }
    let mut concurrent = Vec::with_capacity(16_000);
    while let Some(result) = tasks.join_next().await {
        concurrent.extend(result??);
    }
    let after = rss(pid)?;
    println!(
        "example-echo; Unix socket; 8 concurrent callers on one connection; nearest-rank percentiles"
    );
    println!(
        "warmup={warmup}; call-timeout-ms={}; shutdown-ms={}",
        args[3], args[4]
    );
    report("sequential", &mut sequential);
    report("concurrent (8 x 2000)", &mut concurrent);
    println!("after warmup: {before}\nafter measurement: {after}");
    kernel.notify("shutdown", &EmptyParams {}).await?;
    let status = tokio::time::timeout(shutdown, child.wait()).await??;
    if !status.success() {
        return Err(format!("plugin exited: {status}").into());
    }
    Ok(())
}
