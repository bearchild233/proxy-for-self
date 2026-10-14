use std::{
    future::Future,
    io,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    sync::{Arc, Mutex},
};

use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Notify, watch},
    task::JoinSet,
};

use crate::{
    Context, Descriptor, Kernel, Plugin, RpcResult, Settings,
    protocol::*,
    transport::{Peer, internal, writer},
};

#[derive(Default)]
struct Lifecycle {
    draining: bool,
    stopping: bool,
    in_flight: u32,
    // 管理连接的调用在 in_flight 中，但不计入 drain 返回的在途数（它们不持有租约）。
    // TODO(design): 规格中“管理连接不计入 drain 后的在途统计以外的特殊处理”语义不明确，此处按“不计入 drain 在途数、仍计入关停等待”实现，需设计方确认。
    excluded: u32,
}

struct State {
    lifecycle: Mutex<Lifecycle>,
    stop: watch::Sender<bool>,
    idle: Notify,
}

impl State {
    fn shutdown(&self) {
        self.lifecycle.lock().expect("lifecycle lock").stopping = true;
        self.stop.send_replace(true);
    }

    fn admit(self: &Arc<Self>, excluded: bool) -> Option<CallGuard> {
        let mut life = self.lifecycle.lock().expect("lifecycle lock");
        if life.stopping {
            return None;
        }
        life.in_flight = life.in_flight.checked_add(1)?;
        if excluded {
            life.excluded += 1;
        }
        Some(CallGuard {
            state: self.clone(),
            excluded,
        })
    }

    async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self.lifecycle.lock().expect("lifecycle lock").in_flight == 0 {
                return;
            }
            notified.await;
        }
    }
}

struct CallGuard {
    state: Arc<State>,
    excluded: bool,
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        {
            let mut life = self.state.lifecycle.lock().expect("lifecycle lock");
            life.in_flight -= 1;
            if self.excluded {
                life.excluded -= 1;
            }
        }
        self.state.idle.notify_one();
    }
}

pub struct Server<P: Plugin> {
    listener: UnixListener,
    plugin: Arc<P>,
    settings: Settings,
    descriptor: Descriptor,
    kernel: Kernel,
}

impl<P: Plugin> Server<P> {
    pub fn bind(settings: Settings, descriptor: Descriptor, plugin: P) -> io::Result<Self> {
        // 只清理 socket；配置错指普通文件或符号链接时不能删除用户数据。
        match std::fs::symlink_metadata(&settings.socket) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                std::fs::remove_file(&settings.socket)?
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "socket path is not a socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir_all(&settings.data)?;
        std::fs::set_permissions(&settings.data, std::fs::Permissions::from_mode(0o700))?;
        let listener = UnixListener::bind(&settings.socket)?;
        std::fs::set_permissions(&settings.socket, std::fs::Permissions::from_mode(0o660))?;
        let kernel = Kernel {
            ownership: Arc::new(tokio::sync::Mutex::new(OwnershipChangedParams {
                owned_groups: descriptor.owned_groups.clone(),
                watched_groups: descriptor.watched_groups.clone(),
            })),
            ..Kernel::default()
        };
        Ok(Self {
            listener,
            plugin: Arc::new(plugin),
            settings,
            descriptor,
            kernel,
        })
    }

    pub fn kernel(&self) -> Kernel {
        self.kernel.clone()
    }

    pub async fn run(self) -> io::Result<()> {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        self.run_until(async move {
            term.recv().await;
        })
        .await
    }

    /// 嵌入和测试可注入关停信号；生产 run() 使用 SIGTERM。
    pub async fn run_until(self, shutdown: impl Future<Output = ()> + Send) -> io::Result<()> {
        let (stop, mut stopping) = watch::channel(false);
        let state = Arc::new(State {
            lifecycle: Mutex::new(Lifecycle::default()),
            stop,
            idle: Notify::new(),
        });
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                biased;
                _ = stopping.changed() => break Ok(()),
                _ = &mut shutdown => { state.shutdown(); break Ok(()); }
                Some(_) = connections.join_next(), if !connections.is_empty() => {},
                accepted = self.listener.accept() => {
                    let (stream, _) = match accepted { Ok(pair) => pair, Err(error) => break Err(error) };
                    connections.spawn(connection(stream, self.plugin.clone(), self.descriptor.clone(),
                        self.kernel.clone(), Arc::new(self.settings.data.clone()), state.clone()));
                }
            }
        };
        state.shutdown();
        drop(self.listener);
        // 保留读循环接收能力响应，直到已准入调用（包括通知）完成并写回响应。
        let drained =
            tokio::time::timeout(self.descriptor.shutdown_timeout, state.wait_idle()).await;
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        std::fs::remove_file(&self.settings.socket)?;
        result?;
        drained.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "plugin shutdown timeout"))
    }
}

fn params<T: DeserializeOwned>(value: Value) -> RpcResult<T> {
    serde_json::from_value(value).map_err(|error| RpcError::new(INVALID_PARAMS, error.to_string()))
}

async fn dispatch<P: Plugin>(
    plugin: &P,
    ctx: Context,
    method: &str,
    value: Value,
) -> RpcResult<Value> {
    match method {
        "health" => {
            let _: EmptyParams = params(value)?;
            serde_json::to_value(plugin.health(ctx).await?).map_err(internal)
        }
        "route.select" => {
            serde_json::to_value(plugin.route_select(ctx, params(value)?).await?).map_err(internal)
        }
        "route.prepare" => {
            serde_json::to_value(plugin.route_prepare(ctx, params(value)?).await?).map_err(internal)
        }
        "response.observe" => {
            serde_json::to_value(plugin.response_observe(ctx, params(value)?).await?)
                .map_err(internal)
        }
        "response.head" => {
            serde_json::to_value(plugin.response_head(ctx, params(value)?).await?).map_err(internal)
        }
        "admin.call" => plugin.admin_call(ctx, params(value)?).await,
        _ => Err(RpcError::new(NOT_SUPPORTED, method)),
    }
}

async fn connection<P: Plugin>(
    mut stream: UnixStream,
    plugin: Arc<P>,
    descriptor: Descriptor,
    kernel: Kernel,
    data: Arc<std::path::PathBuf>,
    state: Arc<State>,
) {
    let Ok(Message::Request {
        id,
        method,
        params: value,
    }) = read_frame(&mut stream).await
    else {
        return;
    };
    if id == 0 || method != "hello" {
        return;
    }
    let Ok(hello) = params::<HelloParams>(value) else {
        return;
    };
    if !is_compatible(hello.kernel_api, descriptor.kernel_api) {
        return;
    }
    let groups = kernel.ownership.lock().await;
    let mut hello_result = descriptor.hello();
    hello_result.owned_groups = groups.owned_groups.clone();
    hello_result.watched_groups = groups.watched_groups.clone();
    let response = Message::Response {
        id,
        payload: ResponsePayload::Result(
            serde_json::to_value(hello_result).expect("hello serialization"),
        ),
    };
    if write_frame(&mut stream, &response).await.is_err() {
        return;
    }
    let role = hello.role;
    let (mut reader, write_half) = stream.into_split();
    let (peer, rx) = Peer::new(1);
    // 只有内核连接参与能力调用、后台句柄选路与归属广播；管理连接不登记。
    if role == ConnectionRole::Kernel {
        kernel.register(&peer);
    }
    drop(groups);
    // guard 保证 task 被外层 abort 时也撤销连接可用性与挂起请求。
    let _close = ClosePeer(peer.clone());
    let ctx = Context {
        peer: peer.clone(),
        data,
        kernel,
        role,
    };
    let mut calls = JoinSet::new();
    let mut write_loop = Box::pin(writer(write_half, rx));
    let mut last_id = id;
    'read: loop {
        // read_exact 不支持取消；回收已完成 hook 时必须保留同一个读帧 future。
        let read = read_frame::<_, Message>(&mut reader);
        tokio::pin!(read);
        let message = loop {
            tokio::select! {
                _ = &mut write_loop => break 'read,
                Some(_) = calls.join_next(), if !calls.is_empty() => {},
                message = &mut read => break match message { Ok(message) => message, Err(_) => break 'read },
            }
        };
        match message {
            Message::Response { id, payload } => peer.resolve(id, payload),
            Message::Request {
                id,
                method,
                params: value,
            } => {
                if id <= last_id || method == "hello" {
                    break;
                }
                last_id = id;
                // 准入与 drain 顺序在读循环内决定，不能由 hook task 的调度顺序决定。
                let Some(guard) = state.admit(role == ConnectionRole::Admin) else {
                    continue;
                };
                // 管理连接只处理 health 与 admin.call；其他钩子与能力返回 forbidden。
                let forbidden = role == ConnectionRole::Admin
                    && !matches!(method.as_str(), "health" | "admin.call");
                let immediate = if forbidden {
                    Some(Err(RpcError::new(FORBIDDEN, method.clone())))
                } else if method == "drain" {
                    Some(params::<EmptyParams>(value.clone()).and_then(|_| {
                        let mut life = state.lifecycle.lock().expect("lifecycle lock");
                        life.draining = true;
                        // drain 只统计持有租约的在途调用；本次 drain 自身与管理连接调用不计入。
                        serde_json::to_value(DrainResult {
                            in_flight: life.in_flight - life.excluded - 1,
                        })
                        .map_err(internal)
                    }))
                } else if method == "route.prepare"
                    && state.lifecycle.lock().expect("lifecycle lock").draining
                {
                    Some(
                        serde_json::to_value(RoutePrepareResult::Fail {
                            reason: Reason {
                                code: "plugin_draining".into(),
                                message: "plugin_draining".into(),
                            },
                        })
                        .map_err(internal),
                    )
                } else {
                    None
                };
                let plugin = plugin.clone();
                let ctx = ctx.clone();
                calls.spawn(async move {
                    let _guard = guard;
                    let result = match immediate {
                        Some(result) => result,
                        None => dispatch(&*plugin, ctx.clone(), &method, value).await,
                    };
                    let _ = ctx.peer.respond(id, result).await;
                });
            }
            Message::Notify {
                method,
                params: value,
            } => {
                // 管理连接不接受通知类钩子（含 shutdown 与 request.finish），直接忽略。
                // TODO(design): 管理连接发来通知类钩子时忽略还是断开连接；规格只写明“只处理 health 与 admin.call”。
                if role == ConnectionRole::Admin {
                    continue;
                }
                if method == "shutdown" {
                    if params::<EmptyParams>(value).is_err() {
                        break;
                    }
                    state.shutdown();
                } else if method == "request.finish" {
                    let Some(guard) = state.admit(false) else {
                        continue;
                    };
                    let Ok(value) = params(value) else {
                        break;
                    };
                    let plugin = plugin.clone();
                    let ctx = ctx.clone();
                    calls.spawn(async move {
                        let _guard = guard;
                        let _ = plugin.request_finish(ctx, value).await;
                    });
                } else {
                    break;
                }
            }
        }
    }
    peer.close();
    // 断线后不再驱动 writer，立即释放队列中的写完成 sender，唤醒等待写回的 hook。
    drop(write_loop);
    // 断线不取消已进入插件的调用，保证其有机会完成本地收尾。
    while calls.join_next().await.is_some() {}
}

struct ClosePeer(Arc<Peer>);
impl Drop for ClosePeer {
    fn drop(&mut self) {
        self.0.close();
    }
}
