//! 热更新接管只持久化事实，不序列化进程句柄或单调时钟。
use super::{AccessTiming, LeaseState, Pool};
use crate::error::Failure;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

impl Pool {
    pub(super) async fn persist_lease(&self, id: &str, lease: &LeaseState) -> Result<(), Failure> {
        let data = json!({"account": lease.account, "model": lease.model,
            "gateway": lease.gateway, "generation": lease.generation, "state": lease.state,
            "cookie": lease.cookie, "expires": lease.expires,
            "access": lease.timing.as_ref().map(AccessTiming::value)});
        let (store, id) = (self.store.clone(), id.to_owned());
        tokio::task::spawn_blocking(move || store.save_hook_lease(&id, &data))
            .await
            .map_err(|e| Failure::Internal(e.to_string()))?
    }

    pub(super) async fn restore_lease(self: &Arc<Self>, id: &str) -> Result<(), Failure> {
        if self.leases.lock().expect("leases lock").contains_key(id) {
            return Ok(());
        }
        let (store, key) = (self.store.clone(), id.to_owned());
        let data = tokio::task::spawn_blocking(move || store.load_hook_lease(&key))
            .await
            .map_err(|e| Failure::Internal(e.to_string()))??;
        let Some(data) = data else {
            return Ok(());
        };
        let text = |key: &str| -> Result<String, Failure> {
            data[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| Failure::Internal("invalid hook lease".into()))
        };
        let timing = if data["access"].is_null() {
            None
        } else {
            let access = &data["access"];
            let at = access["startedAt"]
                .as_f64()
                .ok_or_else(|| Failure::Internal("invalid access".into()))?;
            let ended = if access["ended"] == true {
                let outcome = match access["outcome"].as_str() {
                    Some("rejected") => "rejected",
                    Some("failed") => "failed",
                    Some("completed") => "completed",
                    Some("cancelled") => "cancelled",
                    _ => "interrupted",
                };
                Some((
                    Duration::from_millis(access["elapsedMs"].as_u64().unwrap_or_default()),
                    outcome,
                ))
            } else {
                None
            };
            // 跨进程无法延续 Instant：用已保存 UTC 起点补齐已过时间，接管后继续用单调时钟。
            Some(AccessTiming {
                id: access["id"].as_str().unwrap_or_default().to_owned(),
                at,
                started: Instant::now(),
                elapsed_before: Duration::from_secs_f64((self.now() - at).max(0.0)),
                ended,
            })
        };
        let lease = LeaseState {
            account: text("account")?,
            model: text("model")?,
            gateway: text("gateway")?,
            generation: text("generation")?,
            state: text("state")?,
            cookie: text("cookie")?,
            expires: data["expires"].as_f64().unwrap_or_default(),
            timing,
        };
        self.leases
            .lock()
            .expect("leases lock")
            .entry(id.to_owned())
            .or_insert(lease);
        Ok(())
    }

    pub(super) async fn persist_current(&self, id: &str) -> Result<(), Failure> {
        let data: Option<Value> = self
            .leases
            .lock()
            .expect("leases lock")
            .get(id)
            .map(|lease| {
                json!({"account": lease.account, "model": lease.model, "gateway": lease.gateway,
                "generation": lease.generation, "state": lease.state, "cookie": lease.cookie,
                "expires": lease.expires, "access": lease.timing.as_ref().map(AccessTiming::value)})
            });
        if let Some(data) = data {
            let (store, id) = (self.store.clone(), id.to_owned());
            tokio::task::spawn_blocking(move || store.save_hook_lease(&id, &data))
                .await
                .map_err(|e| Failure::Internal(e.to_string()))??;
        }
        Ok(())
    }
}
