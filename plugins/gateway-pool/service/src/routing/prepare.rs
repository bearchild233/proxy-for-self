//! `route.prepare`：旧路径 `route` 循环的一步。内核按 `wait` 的间隔重复调用，直到得到 `use` 或 `fail`。
//! 每次调用：资格检查 → `plan` → 启动后台打票或取材料 → 预约窗口 → 按请求模型取票 → 登记发送 → `use`。

use std::{sync::Arc, time::Instant};

use plugin_sdk::{RpcResult, protocol::*};
use serde_json::json;

use super::{
    AccessTiming, Attempt, Held, LeaseState, Material, Pair, Pool, PoolFailure, PoolSettings,
    RequestState, access::CatalogState, probe::Fallback, ticket::TicketStage,
};
use crate::{caps::Caps, cookie::cookie_gateway};

/// 比请求期限早一点放弃，这样才来得及给出具体原因，而不是被内核的外层超时先截断（与旧实现一致：1.5 秒）。
const GIVE_UP_BEFORE_DEADLINE: f64 = 1.5;

/// 一次 `route.prepare` 的结果。
enum Step {
    /// 稍后再来；说明保存在 `Attempt.waiting`。
    Wait,
    Use {
        lease_id: String,
        cookie: String,
        state: String,
    },
}

impl Pool {
    pub(crate) async fn prepare_route(
        self: &Arc<Self>,
        caps: &Caps,
        params: RoutePrepareParams,
    ) -> RpcResult<RoutePrepareResult> {
        let started = Instant::now();
        self.sweep();
        let deadline = params.deadline_unix_ms as f64 / 1000.0;
        let mut attempt = self.take_attempt(&params.request_id, &params.account_id, deadline);
        match self
            .prepare_step(caps, &params, deadline, &mut attempt, started)
            .await
        {
            Ok(Step::Wait) => {
                let retry_after_ms = self
                    .settings()
                    .await
                    .map_or(50, |settings| settings.probe_interval_ms);
                let reason = Reason {
                    code: "waiting".into(),
                    message: attempt.waiting.clone(),
                };
                self.put_attempt(&params.request_id, &params.account_id, attempt);
                Ok(RoutePrepareResult::Wait {
                    retry_after_ms,
                    reason,
                })
            }
            Ok(Step::Use {
                lease_id,
                cookie,
                state,
            }) => Ok(RoutePrepareResult::Use {
                account_id: params.account_id,
                lease_id,
                upstream: Upstream {
                    cookie: Some(cookie),
                    turn_state: Some(state),
                },
            }),
            Err(failure) => {
                // 已预约但没能交给内核的窗口立即归还。
                if let Some(held) = attempt.held.take() {
                    self.release(&params.account_id, &held.lease, None).await;
                }
                Ok(RoutePrepareResult::Fail {
                    reason: failure.reason(),
                })
            }
        }
    }

    fn take_attempt(&self, request_id: &str, account: &str, deadline: f64) -> Attempt {
        let mut requests = self.requests.lock().expect("requests lock");
        let state = requests
            .entry(request_id.to_owned())
            .or_insert_with(|| RequestState {
                expires: deadline,
                alternatives: Vec::new(),
                ordinary: false,
                attempts: Default::default(),
            });
        state.expires = state.expires.max(deadline);
        state.attempts.remove(account).unwrap_or_default()
    }

    fn put_attempt(&self, request_id: &str, account: &str, attempt: Attempt) {
        if let Some(state) = self
            .requests
            .lock()
            .expect("requests lock")
            .get_mut(request_id)
        {
            state.attempts.insert(account.to_owned(), attempt);
        }
    }

    /// 本 Key 范围内可替代 `account` 的别的降智账号和普通账号，用来判断账号能否让位。
    fn fallback_for(&self, request_id: &str, account: &str) -> Fallback {
        self.requests
            .lock()
            .expect("requests lock")
            .get(request_id)
            .map(|state| Fallback {
                alternatives: state
                    .alternatives
                    .iter()
                    .filter(|id| id.as_str() != account)
                    .cloned()
                    .collect(),
                ordinary: state.ordinary,
            })
            .unwrap_or_default()
    }

    async fn prepare_step(
        self: &Arc<Self>,
        caps: &Caps,
        params: &RoutePrepareParams,
        deadline: f64,
        attempt: &mut Attempt,
        started: Instant,
    ) -> Result<Step, PoolFailure> {
        let account = params.account_id.as_str();
        let settings = self.settings().await?;
        let Some(row) = self.load_account(caps, account).await? else {
            return Err(PoolFailure::new("account_missing", "账号已被删除或归档"));
        };
        let Some(identity) = settings.member_id(
            &row.id,
            row.upstream_account_id.as_deref(),
            row.user_id.as_deref(),
        ) else {
            return Err(PoolFailure::new(
                "member_unknown",
                "无法生成池成员标识：账号缺少上游账号 ID 或用户 ID，请重新登录该账号或在网关池设置里指定",
            ));
        };
        self.eligible(caps, &row).await?;
        if self.now() >= deadline - GIVE_UP_BEFORE_DEADLINE {
            return Err(PoolFailure::new(
                "wait_timeout",
                format!("等待满血窗口超过请求期限：{}", attempt.waiting),
            ));
        }
        if attempt.held.is_none() {
            self.plan_window(
                caps, params, &settings, &identity, deadline, attempt, started,
            )
            .await?;
        }
        let Some(held) = attempt.held.as_mut() else {
            return Ok(Step::Wait);
        };
        match self
            .ticket_stage(caps, &settings, account, &params.model, held, started)
            .await
        {
            TicketStage::Ready => {}
            TicketStage::Pending => {
                attempt.waiting = format!("正在为模型 {} 获取该窗口的票", params.model);
                return Ok(Step::Wait);
            }
            TicketStage::Failed(failure) => return Err(failure),
        }
        // 登记业务访问：窗口已被退役时预约作废，重新选择，不能凭旧预约发送到退役组合。
        let access_id = self.new_id();
        let sent = self
            .runtime(json!({"action": "sent", "accountId": account, "leaseId": held.lease, "purpose": "business",
                "timingVersion": 2, "accessId": access_id}))
            .await?;
        if sent["accepted"] != true {
            let lease = held.lease.clone();
            attempt.held = None;
            self.release(account, &lease, None).await;
            attempt.waiting = "窗口刚刚切换，正在重新预约".to_owned();
            return Ok(Step::Wait);
        }
        let Some(held) = attempt.held.take() else {
            return Ok(Step::Wait);
        };
        let (cookie, state) = match &held.ticket {
            Some(ticket) => (ticket.cookie.clone(), ticket.state.clone()),
            None => (held.material.cookie.clone(), held.material.state.clone()),
        };
        self.leases.lock().expect("leases lock").insert(
            held.lease.clone(),
            LeaseState {
                account: account.to_owned(),
                model: params.model.clone(),
                gateway: held.material.pair.gateway.clone(),
                cookie: cookie.clone(),
                timing: Some(AccessTiming::new(access_id, self.now())),
                expires: deadline,
            },
        );
        Ok(Step::Use {
            lease_id: held.lease,
            cookie,
            state,
        })
    }

    /// `plan` 一步：启动后台打票、取窗口材料、预约窗口。预约成功时 `attempt.held` 有值。
    #[allow(clippy::too_many_arguments)]
    async fn plan_window(
        self: &Arc<Self>,
        caps: &Caps,
        params: &RoutePrepareParams,
        settings: &Arc<PoolSettings>,
        identity: &str,
        deadline: f64,
        attempt: &mut Attempt,
        started: Instant,
    ) -> Result<(), PoolFailure> {
        let account = params.account_id.as_str();
        let catalog = match self
            .catalog_within(caps, settings, account, identity, started)
            .await
        {
            CatalogState::Ready(catalog) => catalog,
            CatalogState::Pending => {
                attempt.waiting = "正在读取池服务的网关目录".to_owned();
                return Ok(());
            }
        };
        let fallback = self.fallback_for(&params.request_id, account);
        let plan = self
            .runtime(json!({"action": "plan", "accountId": account, "demand": true, "catalog": catalog,
                "queueTurn": attempt.queue_turn, "alternatives": fallback.alternatives, "ordinary": fallback.ordinary}))
            .await?;
        if plan["yieldAccount"] == true {
            return Err(PoolFailure::new(
                "yielded",
                "该账号本轮网关已轮完并让位给同一 Key 内的其他账号，请重试",
            ));
        }
        if let Some(failure) = PoolFailure::from_block(&plan["blocked"]) {
            return Err(failure);
        }
        attempt.queue_turn = plan["queueTurn"].clone();
        if !plan["probe"].is_null() {
            attempt.waiting = format!(
                "正在检测网关 {}，尚未得到满血判定",
                plan["probe"]["gateway"].as_str().unwrap_or("?")
            );
            self.start_prepare(
                caps,
                settings,
                account,
                true,
                Some(deadline),
                fallback,
                catalog,
            )
            .await?;
        } else if plan["preparing"] == true {
            attempt.waiting = "另一个请求正在为该账号检测网关".to_owned();
        }
        let Some(pair) = Pair::from_value(&plan["ready"]) else {
            if plan["probe"].is_null() && plan["preparing"] != true {
                // 所有候选均冷却，不绕过静置；交给调度器尝试其他授权账号。
                return Err(PoolFailure::new(
                    "no_candidate",
                    "该账号目前没有可检测的网关（全部在静置或目录为空）",
                ));
            }
            return Ok(());
        };
        let material = self.window_material(account, &pair).await?;
        let reservation = self
            .runtime(
                json!({"action": "reserve", "accountId": account, "gateway": pair.gateway,
                "generation": pair.generation, "deadline": deadline}),
            )
            .await?;
        let Some(material) = material else {
            // 重启或 A/B 切换时不能把仅有持久化状态、没有配套材料的组合当成可用。
            attempt.waiting = format!("网关 {} 缺少配套材料，正在重新检测", pair.gateway);
            if let Some(lease) = reservation["leaseId"].as_str() {
                self.runtime(
                    json!({"action": "invalidate", "accountId": account, "leaseId": lease}),
                )
                .await?;
                self.runtime(json!({"action": "finish", "accountId": account, "leaseId": lease}))
                    .await?;
            }
            return Ok(());
        };
        if let Some(failure) = PoolFailure::from_block(&reservation["blocked"]) {
            return Err(failure);
        }
        if reservation["reserved"] == true {
            let lease = reservation["leaseId"]
                .as_str()
                .ok_or_else(PoolFailure::control_plane)?
                .to_owned();
            attempt.held = Some(Held {
                lease,
                material,
                ticket: None,
                task: None,
            });
        } else {
            attempt.waiting = format!("网关 {} 的窗口刚刚切换，正在重新预约", pair.gateway);
        }
        Ok(())
    }

    /// 窗口的配套材料：先读本进程缓存，缺失时读共享存储（A/B 与重启共用私有材料，
    /// 不因另一个进程的内存缓存缺失而退役可用组合）。到期或 Cookie 指向别的网关的材料不可用。
    async fn window_material(
        &self,
        account: &str,
        pair: &Pair,
    ) -> Result<Option<Material>, PoolFailure> {
        let key = format!("{account}:{}", pair.generation);
        let now = self.now();
        let cached = self
            .materials
            .lock()
            .expect("materials lock")
            .get(&key)
            .cloned()
            .filter(|material| material.expires > now);
        if cached.is_some() {
            return Ok(cached);
        }
        let shared = self
            .runtime(
                json!({"action": "material", "accountId": account, "generation": pair.generation}),
            )
            .await?;
        Ok(shared["state"]
            .as_str()
            .zip(shared["cookie"].as_str())
            .and_then(|(state, cookie)| {
                let expires = shared["expires"].as_f64()?;
                (expires > now && cookie_gateway(cookie).as_deref() == Some(pair.gateway.as_str()))
                    .then(|| Material {
                        pair: pair.clone(),
                        state: state.to_owned(),
                        cookie: cookie.to_owned(),
                        expires,
                    })
            }))
    }
}
