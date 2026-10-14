//! Unix socket 插件运行时。授权与业务策略由内核和插件各自负责。
mod client;
mod plugin;
mod runtime;
#[cfg(feature = "testing")]
pub mod testing;
mod transport;

pub use client::{Context, Kernel};
pub use kernel_protocol as protocol;
pub use plugin::{Descriptor, Plugin, Settings};
pub use runtime::Server;
pub type RpcResult<T> = Result<T, protocol::RpcError>;
