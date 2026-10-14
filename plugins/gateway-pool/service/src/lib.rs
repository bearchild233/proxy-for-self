//! 网关池 Rust 服务插件（P1）：SQLite 存储、设置校验、管理接口、运行状态机与请求路径钩子。
//! 表结构、数值语义、统计投影与运行状态机与 Python 实现保持一致（`Store::runtime` 接受与
//! `GatewayPool.runtime` 相同的 JSON 消息）；请求路径钩子（`route.select`、`route.prepare`、
//! `response.head`、`request.finish`）把旧网关里的编排接到运行状态机上，对外访问全部经内核能力。

pub mod admin;
mod caps;
mod checkpoint;
mod cookie;
mod error;
mod history;
mod history_query;
mod ids;
mod num;
mod policy;
mod pool_service;
mod queue;
mod routing;
mod runtime;
mod settings;
mod store;
mod util;

pub use admin::{GatewayPoolService, dispatch};
pub use caps::{CAPABILITIES, HOOKS};
pub use error::Failure;
pub use ids::{IdSource, random_ids, sequential_ids};
pub use store::{Clock, DB_FILE, ManualClock, Ownership, Store, system_clock};
pub use util::Object;
