//! 每秒执行恢复扫描与持续模式找票；活跃登记沿用旧路径 30 分钟寿命。
use super::{Pool, PoolFailure, observe::cookie_pairs, probe::Fallback};
use crate::caps::Caps;
use plugin_sdk::{Kernel, protocol::UpstreamProbeParams};
use serde_json::json;
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

impl Pool {
    pub(crate) fn start_background(self: &Arc<Self>, kernel: Kernel) {
        let mut task = self.background.lock().expect("background lock");
        if task.is_some() || self.is_stopped() {
            return;
        }
        let weak = Arc::downgrade(self);
        *task = Some(tokio::spawn(async move {
            let caps = Caps::new(kernel);
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let Some(pool) = weak.upgrade() else {
                    break;
                };
                if pool.is_stopped() {
                    break;
                }
                let _ = pool.background_tick(&caps).await;
            }
        }));
    }

    pub(crate) fn stop_background(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(task) = self.background.lock().expect("background lock").take() {
            task.abort();
        }
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    async fn background_tick(self: &Arc<Self>, caps: &Caps) -> Result<(), PoolFailure> {
        self.sweep();
        let settings = self.settings().await?;
        let now = self.now();
        let recent = |map: &std::sync::Mutex<std::collections::HashMap<String, f64>>| {
            let mut map = map.lock().expect("recent lock");
            map.retain(|_, at| now - *at < 1800.0);
            map.keys().cloned().collect::<Vec<_>>()
        };
        let mut active = recent(&self.active);
        let recoverable = recent(&self.recoverable);
        if settings.dynamic_groups_ready() {
            for id in recoverable {
                if self.is_stopped() {
                    return Ok(());
                }
                // 先占进程内名额，再领取持久化代次；无候选时立即释放，不把账号永久挂在 recovering。
                // recoveryCheck 在持久化状态机中回收超过 5 分钟未回报的领取，支持进程中断后重领。
                if !self
                    .recovering
                    .lock()
                    .expect("recovering lock")
                    .insert(id.clone())
                {
                    continue;
                }
                let reply = self
                    .runtime(json!({"action":"recoveryCheck", "accountId":id}))
                    .await;
                let generation = reply
                    .ok()
                    .and_then(|r| r["check"]["generation"].as_str().map(str::to_owned));
                let Some(generation) = generation else {
                    self.recovering.lock().expect("recovering lock").remove(&id);
                    continue;
                };
                let (pool, caps) = (self.clone(), caps.clone());
                tokio::spawn(async move {
                    let outcome = pool.recovery_check(&caps, &id, &generation).await;
                    let _ = pool
                        .runtime(
                            json!({"action":"recoveryResult","accountId":id,"outcome":outcome}),
                        )
                        .await;
                    pool.recovering.lock().expect("recovering lock").remove(&id);
                });
            }
        }
        if settings.mode != "continuous" || self.is_stopped() {
            return Ok(());
        }
        // 旧 background 还包含管理员显式配置池成员标识的账号；内核能力会按当前归属过滤。
        active.extend(settings.pool_account_ids.keys().cloned());
        active.sort();
        active.dedup();
        if active.is_empty() {
            return Ok(());
        }
        let rows = caps
            .accounts(&active, &settings.probe_model)
            .await
            .map_err(|_| PoolFailure::kernel())?;
        let mut candidates = Vec::new();
        for row in &rows {
            if !row.enabled
                || row.authentication_kind != "oauth"
                || row.model_allowed == Some(false)
            {
                continue;
            }
            let exhausted = if row.status == "quota_exhausted" {
                true
            } else {
                let q = caps
                    .quota(&row.id)
                    .await
                    .map_err(|_| PoolFailure::kernel())?;
                q.rank == Some(0) && q.exhausted
            };
            candidates.push(json!({"id": row.id,"exhausted":exhausted}));
        }
        let ordered = self
            .runtime(json!({"action":"active","candidates":candidates}))
            .await?;
        let ids: Vec<String> = ordered["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        for id in &ids {
            if self.is_stopped() {
                break;
            }
            let Some(row) = rows.iter().find(|row| &row.id == id) else {
                continue;
            };
            if self.eligible(caps, row).await.is_err() {
                continue;
            }
            let Some(identity) = settings.member_id(
                id,
                row.upstream_account_id.as_deref(),
                row.user_id.as_deref(),
            ) else {
                continue;
            };
            let catalog = self.catalog_value(caps, &settings, id, &identity).await;
            self.start_prepare(
                caps,
                &settings,
                id,
                false,
                None,
                Fallback {
                    alternatives: ids.iter().filter(|other| *other != id).cloned().collect(),
                    ordinary: false,
                },
                catalog,
            )
            .await?;
        }
        Ok(())
    }

    async fn state_probe(
        &self,
        caps: &Caps,
        id: &str,
        model: &str,
        state: Option<&str>,
        cookie: Option<String>,
    ) -> Option<plugin_sdk::protocol::UpstreamProbeResult> {
        if self.is_stopped() {
            return None;
        }
        // 旧 recovery probe_state 的单次预算为 20 秒，不携带账号 Cookie。
        caps.probe(UpstreamProbeParams {
            account_id: id.into(),
            model: model.into(),
            turn_state: state.map(str::to_owned),
            cookie,
            include_account_cookies: false,
            timeout_ms: 20_000,
            request_id: None,
        })
        .await
        .ok()
    }

    async fn recovery_check(
        self: &Arc<Self>,
        caps: &Caps,
        id: &str,
        generation: &str,
    ) -> &'static str {
        let Ok(Some(row)) = self.load_account(caps, id).await else {
            return "unavailable";
        };
        if self.eligible(caps, &row).await.is_err() {
            return "unavailable";
        }
        let Ok(settings) = self.settings().await else {
            return "unavailable";
        };
        let Ok(material) = self
            .runtime(json!({"action":"material","accountId":id,"generation":generation}))
            .await
        else {
            return "unavailable";
        };
        let (Some(state), Some(cookie)) = (material["state"].as_str(), material["cookie"].as_str())
        else {
            return "unavailable";
        };
        let model = &settings.probe_model;
        match self
            .state_probe(caps, id, model, Some(state), Some(cookie.into()))
            .await
        {
            Some(r) if r.status == Some(200) && !r.state_returned => {}
            Some(r) if r.status == Some(200) && r.state_returned => return "failed",
            _ => return "unavailable",
        }
        for _ in 0..2 {
            let Some(first) = self.state_probe(caps, id, model, None, None).await else {
                return "unavailable";
            };
            let Some(fresh) = first
                .turn_state
                .filter(|_| first.status == Some(200) && first.state_returned)
            else {
                return "unavailable";
            };
            match self
                .state_probe(
                    caps,
                    id,
                    model,
                    Some(&fresh),
                    cookie_pairs(&first.set_cookies),
                )
                .await
            {
                Some(r) if r.status == Some(200) && !r.state_returned => {}
                Some(r) if r.status == Some(200) && r.state_returned => return "failed",
                _ => return "unavailable",
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        self.report_regroup(caps, id, "recovered", model);
        "recovered"
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        self.stop_background();
    }
}
