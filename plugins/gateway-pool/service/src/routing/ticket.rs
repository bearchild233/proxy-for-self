//! 拿票：窗口由打票模型验证，其他模型共用该窗口，只需各自的 state（对应旧路径的 `use_model`、
//! `shared_ticket`、`fetch_ticket`）。先读本进程缓存，再读共享存储（A/B 与重启共用），都没有时
//! 用该模型发一次 `hi`，拿到响应头即结束，不做第二轮判定。
//!
//! 内核的钩子预算只有 2 秒，而发一次探测最长要 15 秒：拿票在后台任务里进行，本次钩子等不到时返回
//! `wait`，窗口预约保留在请求状态里，下一次 `route.prepare` 继续。

use std::{sync::Arc, time::Instant};

use serde_json::json;

use super::{
    AccessTiming, HOOK_BUDGET, Held, Material, Pair, Pool, PoolFailure, PoolSettings, Ticket,
};
use crate::{
    caps::Caps,
    cookie::{cookie_expiry, cookie_gateway, merge_cookie},
    error::Failure,
};

/// 拿票的进展。
pub(super) enum TicketStage {
    /// 票已就绪（`held.ticket` 为空时用窗口自身的 state）。
    Ready,
    /// 票还在获取，稍后再来。
    Pending,
    Failed(PoolFailure),
}

/// 模型标识：`[a-z0-9.-]{1,64}`。
fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 64
        && model
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
}

impl Pool {
    pub(super) async fn ticket_stage(
        self: &Arc<Self>,
        caps: &Caps,
        settings: &PoolSettings,
        account: &str,
        model: &str,
        held: &mut Held,
        started: Instant,
    ) -> TicketStage {
        if model == settings.probe_model || held.ticket.is_some() {
            return TicketStage::Ready;
        }
        if !valid_model(model) {
            return TicketStage::Failed(PoolFailure::new(
                "ticket_unavailable",
                "请求的模型标识无效，无法在该窗口内取得票",
            ));
        }
        let key = format!("{account}:{}:{model}", held.material.pair.generation);
        let cached = self
            .tickets
            .lock()
            .expect("tickets lock")
            .get(&key)
            .filter(|ticket| ticket.expires > self.now())
            .cloned();
        if let Some(ticket) = cached {
            held.ticket = Some(ticket);
            return TicketStage::Ready;
        }
        match self
            .shared_ticket(account, &held.material.pair, model)
            .await
        {
            Ok(Some(ticket)) => {
                self.cache_ticket(key, ticket.clone());
                held.ticket = Some(ticket);
                return TicketStage::Ready;
            }
            Ok(None) => {}
            Err(_) => return TicketStage::Failed(PoolFailure::control_plane()),
        }
        let mut task = match held.task.take() {
            Some(task) => task,
            None => {
                // 同一组合同模型的拿票只做一次；其他请求等待它写入缓存。
                if !self
                    .ticket_inflight
                    .lock()
                    .expect("ticket inflight lock")
                    .insert(key.clone())
                {
                    return TicketStage::Pending;
                }
                let (pool, caps) = (Arc::clone(self), caps.clone());
                let (account, lease) = (account.to_owned(), held.lease.clone());
                let (material, model) = (held.material.clone(), model.to_owned());
                tokio::spawn(async move {
                    let result = pool
                        .fetch_ticket(&caps, &account, &lease, &material, &model)
                        .await;
                    if let Ok(ticket) = &result {
                        pool.cache_ticket(key.clone(), ticket.clone());
                    }
                    pool.ticket_inflight
                        .lock()
                        .expect("ticket inflight lock")
                        .remove(&key);
                    result
                })
            }
        };
        match tokio::time::timeout(HOOK_BUDGET.saturating_sub(started.elapsed()), &mut task).await {
            Ok(Ok(Ok(ticket))) => {
                held.ticket = Some(ticket);
                TicketStage::Ready
            }
            Ok(_) => TicketStage::Failed(PoolFailure::new(
                "ticket_unavailable",
                "无法在该窗口内取得所请求模型的票",
            )),
            Err(_) => {
                held.task = Some(task);
                TicketStage::Pending
            }
        }
    }

    fn cache_ticket(&self, key: String, ticket: Ticket) {
        let now = self.now();
        let mut tickets = self.tickets.lock().expect("tickets lock");
        tickets.retain(|_, ticket| ticket.expires > now);
        Self::insert_bounded(&mut tickets, key, ticket);
    }

    async fn shared_ticket(
        &self,
        account: &str,
        pair: &Pair,
        model: &str,
    ) -> Result<Option<Ticket>, Failure> {
        let shared = self
            .runtime(json!({"action": "ticket", "accountId": account, "generation": pair.generation, "model": model}))
            .await?;
        Ok(shared["state"]
            .as_str()
            .zip(shared["cookie"].as_str())
            .and_then(|(state, cookie)| {
                let expires = shared["expires"].as_f64()?;
                (expires > self.now()
                    && cookie_gateway(cookie).as_deref() == Some(pair.gateway.as_str()))
                .then(|| Ticket {
                    state: state.to_owned(),
                    cookie: cookie.to_owned(),
                    expires,
                })
            }))
    }

    /// 用该模型发一次 `hi` 只拿 state。拿票是真实上游访问，记在本业务租约上；组合已退役时不再发送。
    async fn fetch_ticket(
        &self,
        caps: &Caps,
        account: &str,
        lease: &str,
        base: &Material,
        model: &str,
    ) -> Result<Ticket, ()> {
        let access_id = self.new_id();
        let sent = self
            .runtime(json!({"action": "sent", "accountId": account, "leaseId": lease, "purpose": "ticket",
                "timingVersion": 2, "accessId": access_id}))
            .await
            .map_err(|_| ())?;
        if sent["accepted"] != true {
            return Err(());
        }
        let started = Instant::now();
        let mut timing = AccessTiming::new(access_id, self.now());
        let observed = self
            .send_probe(caps, account, model, None, Some(&base.cookie))
            .await;
        timing.end(if observed.is_some() {
            "headers"
        } else {
            "failed"
        });
        let ticket = observed
            .as_ref()
            .filter(|observation| observation.status == 200)
            .and_then(|observation| {
                let state = observation
                    .state
                    .clone()
                    .filter(|state| !state.is_empty() && state.len() <= 16384)?;
                let cookie = merge_cookie(&base.cookie, &observation.cookies)?;
                (cookie_gateway(&cookie).as_deref() == Some(base.pair.gateway.as_str()))
                    .then_some(())?;
                let expires = cookie_expiry(&cookie, self.now())?.min(base.expires);
                Some(Ticket {
                    state,
                    cookie,
                    expires,
                })
            });
        let material = ticket
            .as_ref()
            .map(|ticket| json!({"state": ticket.state, "cookie": ticket.cookie, "expires": ticket.expires}));
        self.runtime(json!({"action": "ticketResult", "accountId": account, "leaseId": lease, "model": model,
            "status": observed.as_ref().map(|o| o.status),
            "stateReturned": observed.as_ref().is_some_and(|o| o.state_returned),
            "durationMs": started.elapsed().as_millis() as u64, "material": material, "access": timing.value()}))
            .await
            .map_err(|_| ())?;
        ticket.ok_or(())
    }
}
