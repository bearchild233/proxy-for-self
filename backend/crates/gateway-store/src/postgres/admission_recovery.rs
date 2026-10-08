//! Redis 丢失后从 `model_requests` 恢复客户端准入热状态。

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use gateway_core::{
    engine::{
        ModelRequestId,
        admission::{
            ClientAdmissionError, ClientAdmissionRecovery as CoreAdmissionRecovery,
            ClientAdmissionRecoveryPort, RecentAdmissionFact, RunningAdmissionFact,
        },
    },
    policy::ClientApiKeyId,
};
use sqlx::{PgConnection, PgPool};

use crate::{StoreResult, postgres_unavailable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRecentRequest {
    pub model_request_id: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRunningRequest {
    pub model_request_id: String,
    pub deadline_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientAdmissionRecovery {
    pub client_api_key_ref: String,
    pub recent_requests: Vec<ClientAdmissionRecentRequest>,
    pub running_requests: Vec<ClientAdmissionRunningRequest>,
}

#[async_trait]
pub trait ClientAdmissionRecoveryRepository: Send + Sync {
    async fn load_client_admission_recovery(
        &self,
        window_started_at: DateTime<Utc>,
    ) -> StoreResult<Vec<ClientAdmissionRecovery>>;
}

#[derive(Clone)]
pub struct PgClientAdmissionRecoveryRepository {
    pool: PgPool,
}

impl PgClientAdmissionRecoveryRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// A/B 独立槽锁 + 共享存活锁；旧单实例仍持有排他锁时拒绝加入。
    pub async fn acquire_slot(&self, slot: &str) -> StoreResult<PgSlotGuard> {
        let slot_key = match slot {
            "a" => 10_i32,
            "b" => 11_i32,
            _ => return Err(postgres_unavailable("invalid deployment slot")),
        };
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| postgres_unavailable("connect slot lock"))?
            .detach();
        for key in [1_i32, slot_key] {
            let acquired: bool = sqlx::query_scalar("select pg_try_advisory_lock(1129337427, $1)")
                .bind(key)
                .fetch_one(&mut connection)
                .await
                .map_err(|_| postgres_unavailable("acquire slot startup lock"))?;
            if !acquired {
                return Err(postgres_unavailable("slot occupied or startup in progress"));
            }
        }
        let cold: bool = sqlx::query_scalar("select pg_try_advisory_lock(1129337426, 1)")
            .fetch_one(&mut connection)
            .await
            .map_err(|_| postgres_unavailable("check live gateway slots"))?;
        if !cold {
            let joined: bool =
                sqlx::query_scalar("select pg_try_advisory_lock_shared(1129337426, 1)")
                    .fetch_one(&mut connection)
                    .await
                    .map_err(|_| postgres_unavailable("join live gateway slot"))?;
            if !joined {
                return Err(postgres_unavailable(
                    "legacy gateway or cold startup is exclusive",
                ));
            }
        } else {
            recover_interrupted_requests(&mut connection).await?;
        }
        Ok(PgSlotGuard {
            connection,
            cold,
            initialized: false,
        })
    }

    /// 单副本网关启动屏障：连接由 Bundle 持有至退出，重复启动不能清理在途请求。
    pub async fn recover_after_restart(&self) -> StoreResult<PgConnection> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| postgres_unavailable("connect gateway instance lock"))?
            .detach();
        let acquired: bool = sqlx::query_scalar("select pg_try_advisory_lock(1129337426, 1)")
            .fetch_one(&mut connection)
            .await
            .map_err(|_| postgres_unavailable("acquire gateway instance lock"))?;
        if !acquired {
            return Err(postgres_unavailable("another gateway instance is running"));
        }
        recover_interrupted_requests(&mut connection).await?;
        Ok(connection)
    }
}

/// 连接持有至进程退出；只有无其他存活槽的启动者可进行全局恢复。
pub struct PgSlotGuard {
    connection: PgConnection,
    cold: bool,
    initialized: bool,
}

impl PgSlotGuard {
    #[must_use]
    pub const fn needs_recovery(&self) -> bool {
        self.cold
    }

    /// 冷启动完成 Redis/Core 恢复后降为共享锁，再开放另一槽的启动屏障。
    pub async fn complete_startup(&mut self) -> StoreResult<()> {
        if self.initialized {
            return Ok(());
        }
        if self.cold {
            sqlx::query("select pg_advisory_lock_shared(1129337426, 1)")
                .execute(&mut self.connection)
                .await
                .map_err(|_| postgres_unavailable("hold shared slot lifetime"))?;
            sqlx::query("select pg_advisory_unlock(1129337426, 1)")
                .execute(&mut self.connection)
                .await
                .map_err(|_| postgres_unavailable("finish cold slot recovery"))?;
        }
        sqlx::query("select pg_advisory_unlock(1129337427, 1)")
            .execute(&mut self.connection)
            .await
            .map_err(|_| postgres_unavailable("release slot startup barrier"))?;
        self.initialized = true;
        Ok(())
    }

    pub async fn close(self) -> StoreResult<()> {
        use sqlx::Connection;
        self.connection
            .close()
            .await
            .map_err(|_| postgres_unavailable("close slot lock"))
    }
}

async fn recover_interrupted_requests(connection: &mut PgConnection) -> StoreResult<()> {
    let result = sqlx::query(
        "update model_requests set outcome = 'incomplete',
                 error_kind = 'process_interrupted',
                 error_message = 'gateway restarted before request completion',
                 image_generation_succeeded = case
                   when image_generation_requested then false else null end,
                 completed_at = now()
             where outcome = 'running'",
    )
    .execute(connection)
    .await
    .map_err(|_| postgres_unavailable("recover interrupted gateway requests"))?;
    tracing::info!(requests = result.rows_affected(), "网关启动时回收中断请求");
    Ok(())
}

#[async_trait]
impl ClientAdmissionRecoveryRepository for PgClientAdmissionRecoveryRepository {
    async fn load_client_admission_recovery(
        &self,
        window_started_at: DateTime<Utc>,
    ) -> StoreResult<Vec<ClientAdmissionRecovery>> {
        let rows = sqlx::query_as::<_, (String, String, DateTime<Utc>, DateTime<Utc>, String)>(
            "select client_api_key_ref, id, started_at, deadline_at, outcome
             from model_requests
             where started_at >= $1 or outcome = 'running'
             order by client_api_key_ref, started_at, id",
        )
        .bind(window_started_at)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| postgres_unavailable("load client admission recovery"))?;
        let mut recoveries = BTreeMap::<String, ClientAdmissionRecovery>::new();
        for (client_api_key_ref, model_request_id, started_at, deadline_at, outcome) in rows {
            let recovery = recoveries
                .entry(client_api_key_ref.clone())
                .or_insert_with(|| ClientAdmissionRecovery {
                    client_api_key_ref,
                    recent_requests: Vec::new(),
                    running_requests: Vec::new(),
                });
            if started_at >= window_started_at {
                recovery.recent_requests.push(ClientAdmissionRecentRequest {
                    model_request_id: model_request_id.clone(),
                    started_at,
                });
            }
            if outcome == "running" {
                recovery
                    .running_requests
                    .push(ClientAdmissionRunningRequest {
                        model_request_id,
                        deadline_at,
                    });
            }
        }
        Ok(recoveries.into_values().collect())
    }
}

impl ClientAdmissionRecoveryPort for PgClientAdmissionRecoveryRepository {
    fn load_recovery(
        &self,
        since: std::time::SystemTime,
    ) -> futures::future::BoxFuture<'_, Result<Vec<CoreAdmissionRecovery>, ClientAdmissionError>>
    {
        Box::pin(async move {
            self.load_client_admission_recovery(DateTime::<Utc>::from(since))
                .await
                .map_err(|_| ClientAdmissionError)?
                .into_iter()
                .map(|recovery| {
                    let client_api_key_id = ClientApiKeyId::new(recovery.client_api_key_ref)
                        .map_err(|_| ClientAdmissionError)?;
                    let recent_requests = recovery
                        .recent_requests
                        .into_iter()
                        .map(|request| {
                            Ok(RecentAdmissionFact {
                                model_request_id: ModelRequestId::new(request.model_request_id)
                                    .map_err(|_| ClientAdmissionError)?,
                                started_at: request.started_at.into(),
                            })
                        })
                        .collect::<Result<Vec<_>, ClientAdmissionError>>()?;
                    let running_requests = recovery
                        .running_requests
                        .into_iter()
                        .map(|request| {
                            Ok(RunningAdmissionFact {
                                model_request_id: ModelRequestId::new(request.model_request_id)
                                    .map_err(|_| ClientAdmissionError)?,
                                expires_at: request.deadline_at.into(),
                            })
                        })
                        .collect::<Result<Vec<_>, ClientAdmissionError>>()?;
                    Ok(CoreAdmissionRecovery {
                        client_api_key_id,
                        recent_requests,
                        running_requests,
                    })
                })
                .collect()
        })
    }
}
