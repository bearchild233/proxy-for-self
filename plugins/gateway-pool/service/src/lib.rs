//! 网关池 Rust 服务插件（P1）：SQLite 存储、设置校验、管理接口与运行状态机。
//! 表结构、数值语义、统计投影与运行状态机与 Python 实现保持一致（`Store::runtime` 接受与
//! `GatewayPool.runtime` 相同的 JSON 消息）；内核钩子与编排在后续步骤接到运行状态机上。

pub mod admin;
mod error;
mod history;
mod history_query;
mod ids;
mod num;
mod policy;
mod queue;
mod runtime;
mod settings;
mod store;
mod util;

pub use admin::{GatewayPoolService, dispatch};
pub use error::Failure;
pub use ids::{IdSource, random_ids, sequential_ids};
pub use store::{Clock, DB_FILE, ManualClock, Ownership, Store, system_clock};
pub use util::Object;
