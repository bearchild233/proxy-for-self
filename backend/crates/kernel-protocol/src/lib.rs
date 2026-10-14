//! 内核与插件共享的 0.2 wire contract；不包含调度或授权实现。
mod capabilities;
mod frame;
mod hooks;
mod message;

pub use capabilities::*;
pub use frame::*;
pub use hooks::*;
pub use message::*;
