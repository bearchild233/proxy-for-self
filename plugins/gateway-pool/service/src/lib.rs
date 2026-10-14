//! 网关池 Rust 服务插件：SQLite 存储、设置校验、管理接口、运行状态机与请求路径钩子。
//! `Store::runtime` 接受 JSON 动作消息并返回 JSON（状态机本体，含备用票与静置退避）；
//! 请求路径钩子（`route.select`、`route.prepare`、`response.head`、`request.finish`）把编排接到
//! 运行状态机上，对外访问全部经内核能力。

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
mod standby;
mod store;
mod util;

pub use admin::{GatewayPoolService, dispatch};
pub use caps::{CAPABILITIES, HOOKS};
pub use error::Failure;
pub use ids::{IdSource, random_ids, sequential_ids};
pub use store::{Clock, DB_FILE, ManualClock, Ownership, Store, system_clock};
pub use util::Object;
