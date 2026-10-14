//! 内核插件运行时：socket、注册表和能力适配，独立于请求执行路径。
mod capability;
mod connection;
mod registry;
mod runtime;
mod state;
mod transport;

pub use capability::LogCapabilities;
pub use kernel_protocol::{KernelInfo, KernelSlot};
pub use registry::{PluginRegistration, PluginRegistry, PluginRuntimeConfig};
pub use runtime::PluginRuntime;
pub use state::{ConnectionState, PluginSnapshot, RuntimeSnapshot};
