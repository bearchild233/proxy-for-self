//! 管理接口：`admin.call` 的方法分派。方法名与页面能力的对应关系见 008 任务说明第 3 部分。

use std::sync::Arc;

use plugin_sdk::{
    Context, Plugin, RpcResult,
    protocol::{AdminCallParams, FORBIDDEN, INTERNAL, INVALID_PARAMS, RpcError},
};
use serde_json::Value;

use crate::{
    error::Failure,
    store::{Ownership, Store},
    util::Object,
};

/// 同步分派，供 `admin.call` 与测试共用。`configure` 同时返回新的归属，由调用方决定如何广播。
pub fn dispatch(
    store: &Store,
    method: &str,
    data: &Object,
) -> Result<(Value, Option<Ownership>), Failure> {
    match method {
        "read" => Ok((store.read_admin(data)?, None)),
        "configure" => {
            let (settings, ownership) = store.configure(data)?;
            Ok((Value::Object(settings), Some(ownership)))
        }
        "queue.update" => Ok((store.update_queue(data)?, None)),
        "history.read" => Ok((store.statistics(data)?, None)),
        other => Err(Failure::NotSupported(other.to_owned())),
    }
}

pub struct GatewayPoolService {
    store: Arc<Store>,
}

impl GatewayPoolService {
    pub fn new(store: Store) -> Self {
        Self {
            store: Arc::new(store),
        }
    }
}

impl Plugin for GatewayPoolService {
    async fn admin_call(&self, ctx: Context, params: AdminCallParams) -> RpcResult<Value> {
        // 与原实现一致：只有管理员角色可以调用网关池管理方法。
        if params.actor.role != "admin" {
            return Err(RpcError::new(FORBIDDEN, "没有网关池管理权限"));
        }
        let Value::Object(data) = params.params else {
            return Err(RpcError::new(INVALID_PARAMS, "参数必须为对象"));
        };
        let store = self.store.clone();
        let method = params.method;
        // 数据库操作是同步的，放到阻塞线程池，避免占用插件的异步运行时。
        let (value, ownership) =
            tokio::task::spawn_blocking(move || dispatch(&store, &method, &data))
                .await
                .map_err(|error| RpcError::new(INTERNAL, error.to_string()))??;
        if let Some(ownership) = ownership {
            // 归属先写入内核侧状态再广播；广播失败时下一次握手会带上最新归属，不回滚已保存的设置。
            if let Err(error) = ctx
                .kernel()
                .set_ownership(ownership.owned_groups, ownership.watched_groups)
                .await
            {
                eprintln!("gateway pool ownership broadcast failed: {}", error.message);
            }
        }
        Ok(value)
    }
}
