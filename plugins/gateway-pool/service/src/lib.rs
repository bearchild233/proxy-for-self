//! 网关池 Rust 服务插件（P1 第 1 步）：SQLite 存储、设置校验与管理接口。
//! 表结构、数值语义与统计投影与 Python 实现保持一致；运行状态机、内核钩子与编排在后续步骤迁入。

pub mod admin;
mod error;
mod history;
mod history_query;
mod num;
mod policy;
mod queue;
mod settings;
mod store;
mod util;

pub use admin::{GatewayPoolService, dispatch};
pub use error::Failure;
pub use store::{Clock, DB_FILE, ManualClock, Ownership, Store, system_clock};
pub use util::Object;
