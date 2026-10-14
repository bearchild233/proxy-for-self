//! 后台打票：对应旧路径的 `start_prepare`、`prepare`、`prepare_material`、`probe_headers`。
//! 探测经内核 `upstream.probe`（额度头同步与 429 冷却由内核完成，这里不重复处理）；
//! 池服务的 Cookie 经 `http.fetch`。

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use plugin_sdk::protocol::{AccountSummary, UpstreamProbeParams};
use serde_json::{Value, json};

use super::{
    AccessTiming, Material, PREPARE_TIMEOUT, PROBE_ATTEMPT_MS, Pair, Pool, PoolFailure,
    PoolSettings,
};
use crate::{
    caps::Caps,
    cookie::{cookie_expiry, cookie_gateway, merge_cookie, routing_cookie},
    pool_service,
};

/// 同一 Key 内的退路：别的降智账号，或降智组之外的普通账号。
#[derive(Clone, Default)]
pub(super) struct Fallback {
    pub alternatives: Vec<String>,
    pub ordinary: bool,
}

/// 一次探测观察到的响应头事实。
pub(super) struct Observation {
    pub status: u16,
    pub state: Option<String>,
    pub state_returned: bool,
    pub cookies: Vec<String>,
    pub retry_after: Option<Duration>,
}

impl Pool {
    /// 经内核 `upstream.probe` 发一次 `hi`，只看响应头。没有响应（连接失败、超时、内核拒绝）返回 None。
    pub(super) async fn send_probe(
        &self,
        caps: &Caps,
        account: &str,
        model: &str,
        state: Option<&str>,
        cookie: Option<&str>,
    ) -> Option<Observation> {
        if self.is_stopped() {
            return None;
        }
        let result = caps
            .probe(UpstreamProbeParams {
                account_id: account.to_owned(),
                model: model.to_owned(),
                turn_state: state.map(str::to_owned),
                cookie: cookie.map(str::to_owned),
                include_account_cookies: false,
                timeout_ms: PROBE_ATTEMPT_MS,
            })
            .await
            .ok()?;
        Some(Observation {
            status: result.status?,
            state: result.turn_state,
            state_returned: result.state_returned,
            cookies: result.set_cookies,
            retry_after: result.retry_after_ms.map(Duration::from_millis),
        })
    }

    /// 为账号领取候选并在后台打票；已有 4 个打票任务在跑时不启动。
    /// `until` 是请求截止时间（Unix 秒）：旧路径随请求取消而停止，钩子里拿不到取消信号，改以截止时间为界。
    // 011 已确认：客户端断开后仍以请求截止时间为界。
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn start_prepare(
        self: &Arc<Self>,
        caps: &Caps,
        settings: &Arc<PoolSettings>,
        account: &str,
        demand: bool,
        until: Option<f64>,
        fallback: Fallback,
        catalog: Value,
    ) -> Result<(), PoolFailure> {
        if self.is_stopped() {
            return Ok(());
        }
        let Ok(permit) = Arc::clone(&self.probes).try_acquire_owned() else {
            return Ok(());
        };
        let claim = self
            .runtime(json!({"action": "claim", "accountId": account, "demand": demand, "catalog": catalog,
                "alternatives": fallback.alternatives, "ordinary": fallback.ordinary}))
            .await?;
        if claim["claimed"] != true {
            return Ok(());
        }
        let (pool, caps, settings, account) = (
            Arc::clone(self),
            caps.clone(),
            Arc::clone(settings),
            account.to_owned(),
        );
        tokio::spawn(async move {
            let _permit = permit;
            let (mut claim, mut settings) = (claim, settings);
            let queue_turn = claim["queueTurn"].clone();
            loop {
                if pool
                    .prepare(&caps, &account, claim, &settings, until)
                    .await
                    .is_err()
                {
                    // 结果没能回报给运行状态机；探测租约由租约超时回收兜底。
                    caps.warn("网关池打票结果回报失败", json!({"accountId": account}))
                        .await;
                    break;
                }
                if pool.past(until) || pool.is_stopped() {
                    break;
                }
                // 候选失败后立即尝试下一组合，不等待下一次周期。
                tokio::time::sleep(Duration::from_millis(settings.probe_interval_ms)).await;
                let Ok(next_settings) = pool.settings().await else {
                    break;
                };
                if !demand && next_settings.mode != "continuous" {
                    break;
                }
                settings = next_settings;
                let Ok(Some(row)) = pool.load_account(&caps, &account).await else {
                    break;
                };
                if pool.eligible(&caps, &row).await.is_err() {
                    break;
                }
                let Some(identity) = settings.member_id(
                    &row.id,
                    row.upstream_account_id.as_deref(),
                    row.user_id.as_deref(),
                ) else {
                    break;
                };
                let catalog = pool
                    .catalog_value(&caps, &settings, &account, &identity)
                    .await;
                let Ok(next) = pool
                    .runtime(json!({"action": "claim", "accountId": account, "demand": demand, "catalog": catalog,
                        "queueTurn": queue_turn, "alternatives": fallback.alternatives, "ordinary": fallback.ordinary}))
                    .await
                else {
                    break;
                };
                if next["claimed"] != true {
                    break;
                }
                claim = next;
            }
        });
        Ok(())
    }

    /// 请求截止时间（Unix 秒）是否已过。
    fn past(&self, until: Option<f64>) -> bool {
        until.is_some_and(|until| self.now() >= until)
    }

    async fn until(&self, until: Option<f64>) {
        match until {
            Some(until) => {
                tokio::time::sleep(Duration::from_secs_f64((until - self.now()).max(0.0))).await;
            }
            None => std::future::pending().await,
        }
    }

    /// 执行一次打票并向运行状态机回报结果、归还探测租约。
    async fn prepare(
        self: &Arc<Self>,
        caps: &Caps,
        account: &str,
        claim: Value,
        settings: &PoolSettings,
        until: Option<f64>,
    ) -> Result<(), ()> {
        let lease = claim["leaseId"].as_str().ok_or(())?.to_owned();
        let pair = Pair::from_value(&claim["pair"]).ok_or(())?;
        let started = Instant::now();
        let preparation_started_at = self.now();
        let result = tokio::select! {
            biased;
            _ = self.until(until) => None,
            result = tokio::time::timeout(PREPARE_TIMEOUT, self.prepare_material(caps, account, &pair, &lease, settings)) => result.ok().and_then(Result::ok),
        };
        let observation = result
            .as_ref()
            .map(|(material, status, returned)| {
                json!({"status": status, "stateReturned": returned, "paired": material.is_some(), "gatewayMatches": material.is_some()})
            })
            .unwrap_or_else(|| {
                json!({"status": null, "stateReturned": false, "paired": false, "gatewayMatches": false, "failed": true})
            });
        let private_material = result
            .as_ref()
            .and_then(|(material, status, returned)| {
                (*status == 200 && !returned).then_some(material.as_ref()).flatten()
            })
            .map(|material| json!({"state": material.state, "cookie": material.cookie, "expires": material.expires}));
        if let Some((Some(material), 200, false)) = result {
            let now = self.now();
            let mut materials = self.materials.lock().expect("materials lock");
            materials.retain(|_, material| material.expires > now);
            Self::insert_bounded(
                &mut materials,
                format!("{account}:{}", pair.generation),
                material,
            );
        }
        let report = self
            .runtime(json!({"action": "probeResult", "accountId": account, "leaseId": lease,
                "durationMs": started.elapsed().as_millis() as u64, "preparationStartedAt": preparation_started_at,
                "status": observation["status"], "stateReturned": observation["stateReturned"],
                "paired": observation["paired"], "gatewayMatches": observation["gatewayMatches"],
                "failed": observation["failed"], "material": private_material}))
            .await;
        let finish = self
            .runtime(json!({"action": "finish", "accountId": account, "leaseId": lease}))
            .await;
        report.map_err(|_| ())?;
        finish.map_err(|_| ())?;
        Ok(())
    }

    /// 取池 Cookie、bootstrap、重放：成功时给出窗口配套材料。返回 (材料, 状态码, 重放是否又返回 state)。
    async fn prepare_material(
        &self,
        caps: &Caps,
        account: &str,
        pair: &Pair,
        lease: &str,
        settings: &PoolSettings,
    ) -> Result<(Option<Material>, u16, bool), ()> {
        let row = self
            .load_account(caps, account)
            .await
            .ok()
            .flatten()
            .ok_or(())?;
        self.eligible(caps, &row).await.map_err(|_| ())?;
        if self.is_stopped() {
            return Err(());
        }
        let identity = settings
            .member_id(
                &row.id,
                row.upstream_account_id.as_deref(),
                row.user_id.as_deref(),
            )
            .ok_or(())?;
        let value = pool_service::cookie(caps, settings, account, &identity, &pair.gateway).await?;
        let cookie = routing_cookie(&value).ok_or(())?;
        // 池服务给出的剩余有效期与 Cookie 自身 exp 取较早者。
        let pool_deadline = value["valid_for_s"]
            .as_f64()
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
            .map(|seconds| self.now() + seconds);
        if cookie_gateway(&cookie).as_deref() != Some(pair.gateway.as_str()) {
            return Err(());
        }
        let first = self
            .probe_headers(
                caps,
                &row,
                pair,
                lease,
                &settings.probe_model,
                None,
                &cookie,
            )
            .await?;
        if first.status != 200 {
            return Ok((None, first.status, false));
        }
        let state = first
            .state
            .filter(|state| !state.is_empty() && state.len() <= 16384)
            .ok_or(())?;
        let paired_cookie = merge_cookie(&cookie, &first.cookies).ok_or(())?;
        if cookie_gateway(&paired_cookie).as_deref() != Some(pair.gateway.as_str()) {
            return Ok((None, 200, false));
        }
        let second = self
            .probe_headers(
                caps,
                &row,
                pair,
                lease,
                &settings.probe_model,
                Some(&state),
                &paired_cookie,
            )
            .await?;
        let resulting = merge_cookie(&paired_cookie, &second.cookies).ok_or(())?;
        if cookie_gateway(&resulting).as_deref() != Some(pair.gateway.as_str()) {
            return Ok((None, second.status, second.state_returned));
        }
        let expires = cookie_expiry(&resulting, self.now()).ok_or(())?;
        let expires = pool_deadline.map_or(expires, |deadline| expires.min(deadline));
        Ok((
            Some(Material {
                pair: pair.clone(),
                state,
                cookie: resulting,
                expires,
            }),
            second.status,
            second.state_returned,
        ))
    }

    /// 一次探测阶段（bootstrap 或重放），最多两次尝试；每次尝试都登记访问并回报 `probeAttempt`。
    /// 额度头同步由内核在 `upstream.probe` 中完成；Retry-After 超过 5 秒时不抢跑，交给冷却后的下一次探测。
    #[allow(clippy::too_many_arguments)]
    async fn probe_headers(
        &self,
        caps: &Caps,
        row: &AccountSummary,
        pair: &Pair,
        lease: &str,
        model: &str,
        state: Option<&str>,
        cookie: &str,
    ) -> Result<Observation, ()> {
        let phase = if state.is_some() {
            "replay"
        } else {
            "bootstrap"
        };
        for attempt in 0..=1_u32 {
            let current = self
                .load_account(caps, &row.id)
                .await
                .ok()
                .flatten()
                .ok_or(())?;
            self.eligible(caps, &current).await.map_err(|_| ())?;
            let access_id = self.new_id();
            let sent = self
                .runtime(json!({"action": "sent", "accountId": row.id, "leaseId": lease, "timingVersion": 2,
                    "accessId": access_id, "purpose": phase}))
                .await
                .map_err(|_| ())?;
            if sent["accepted"] != true {
                return Err(());
            }
            let started = Instant::now();
            let mut timing = AccessTiming::new(access_id, self.now());
            let observed = self
                .send_probe(caps, &row.id, model, state, Some(cookie))
                .await;
            timing.end(if observed.is_some() {
                "headers"
            } else {
                "failed"
            });
            // 每次访问独立留痕；bootstrap 行仅是材料获取，不参与 21/29 判定。
            self.runtime(json!({"action": "probeAttempt", "accountId": row.id, "leaseId": lease, "phase": phase,
                "retry": attempt, "status": observed.as_ref().map(|o| o.status),
                "stateReturned": observed.as_ref().is_some_and(|o| o.state_returned),
                "durationMs": started.elapsed().as_millis() as u64, "gateway": pair.gateway,
                "access": timing.value()}))
                .await
                .map_err(|_| ())?;
            if let Some(observation) = observed {
                if observation.status == 200 || attempt == 1 {
                    return Ok(observation);
                }
                if observation
                    .retry_after
                    .is_some_and(|delay| delay > Duration::from_secs(5))
                {
                    return Ok(observation);
                }
                tokio::time::sleep(observation.retry_after.unwrap_or(Duration::from_millis(50)))
                    .await;
            } else if attempt == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Err(())
    }
}
