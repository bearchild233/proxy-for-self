//! 一次性账号维护：专用授权、正常资格与共享租约，绝不借用故障诊断旁路。
use crate::credential::{CodexAccountQuotaSnapshot, CodexCredentialCatalogService};
use crate::plugin::{OpenAiPluginUpstream, hi_request};
use crate::transport::{CodexRequestContext, catalog::CodexCatalogCapabilityEvidence};
use gateway_core::{
    account::{
        AccountCandidate, AccountEligibilityPolicy, AccountSelectionContext, AccountSelector,
        AccountStatus, ProviderAccount, ProviderAccountId,
    },
    plugin::*,
    policy::ClientApiKeyId,
    provider_ports::{
        ProviderLeaseAcquisition, ProviderLeasePort, ProviderLeaseRequest,
        ProviderRuntimePolicyPort, ProviderSchedulingLeaseRequest,
    },
};
use secrecy::ExposeSecret;
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) struct WarmupDependencies {
    pub leases: Arc<dyn ProviderLeasePort>,
    pub policy: Arc<dyn ProviderRuntimePolicyPort>,
    pub catalog: Arc<CodexCredentialCatalogService>,
}

fn reason(account: &ProviderAccount) -> Option<&'static str> {
    if account.provider().as_str() != "openai" || account.authentication_kind() != "oauth" {
        Some("unsupported")
    } else if !account.enabled() {
        Some("disabled")
    } else {
        None
    }
}

/// 仅投影真实账号级五小时窗口；保留到期的 resetAt 供插件检测真实重置事件。
fn short_term(snapshot: &CodexAccountQuotaSnapshot) -> Option<AccountWarmupShortTerm> {
    let window = snapshot
        .windows()
        .iter()
        .filter(|window| window.is_account_wide())
        .filter(|window| {
            window
                .window_seconds()
                .is_some_and(|seconds| seconds.abs_diff(18000) <= 900)
        })
        .max_by_key(|window| window.reset_at())?;
    Some(AccountWarmupShortTerm {
        observed_at: Some(
            chrono::DateTime::<chrono::Utc>::from(snapshot.observed_at()).to_rfc3339(),
        ),
        window_seconds: window.window_seconds()?,
        used_percent: window.used_percent(),
        reset_at: window.reset_at().map(|at| at.to_rfc3339()),
    })
}

fn terminal(
    outcome: AccountWarmupOutcome,
    reason: &str,
    started: tokio::time::Instant,
) -> AccountWarmupRunResult {
    AccountWarmupRunResult {
        outcome,
        reason_code: Some(reason.into()),
        status: None,
        duration_ms: started.elapsed().as_millis() as u64,
        send_state: AccountWarmupSendState::NotSent,
        short_term: None,
    }
}

/// 上游有时只返回相对 reset-after，而额度快照统一保存绝对 reset-at。
/// 在同一次观测中把相对值换算成绝对值，避免把旧快照的计时器冒充本次响应事实。
fn normalize_reset_after_headers(headers: &[(String, String)]) -> Vec<(String, String)> {
    const RESET_AFTER_SUFFIX: &str = "-reset-after-seconds";
    let reset_at_names = headers
        .iter()
        .filter_map(|(name, _)| name.strip_suffix("-reset-at"))
        .map(|name| format!("{name}-reset-at"))
        .collect::<BTreeSet<_>>();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    let mut normalized = headers.to_vec();
    let Some(now) = now else {
        return normalized;
    };
    for (name, value) in headers {
        if !name.ends_with(RESET_AFTER_SUFFIX) {
            continue;
        }
        let reset_at_name = format!(
            "{}-reset-at",
            &name[..name.len().saturating_sub(RESET_AFTER_SUFFIX.len())]
        );
        if reset_at_names.contains(&reset_at_name) {
            continue;
        }
        let Some(seconds) = value
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|seconds| *seconds > 0)
        else {
            continue;
        };
        let Some(seconds) = i64::try_from(seconds).ok() else {
            continue;
        };
        let Some(reset_at) = now.checked_add(seconds) else {
            continue;
        };
        normalized.push((reset_at_name, reset_at.to_string()));
    }
    normalized
}

impl OpenAiPluginUpstream {
    pub(super) async fn read_warmup(
        &self,
        access: &dyn PluginAccess,
        params: AccountWarmupReadParams,
    ) -> Result<AccountWarmupReadResult, RpcError> {
        require_warmup(access, ACCOUNT_WARMUP_READ)?;
        if params.ids.as_ref().is_some_and(|ids| ids.len() > 500)
            || params.include_models
                && !params
                    .ids
                    .as_ref()
                    .is_some_and(|ids| (1..=20).contains(&ids.len()))
            || params
                .model
                .as_ref()
                .is_some_and(|model| model.is_empty() || model.len() > 128)
        {
            return Err(RpcError::new(
                INVALID_PARAMS,
                "invalid warmup directory scope",
            ));
        }
        tokio::time::timeout(
            Duration::from_secs(7),
            self.read_warmup_inner(access, params),
        )
        .await
        .map_err(|_| RpcError::new(UNAVAILABLE, "warmup directory timeout"))?
    }

    async fn read_warmup_inner(
        &self,
        access: &dyn PluginAccess,
        params: AccountWarmupReadParams,
    ) -> Result<AccountWarmupReadResult, RpcError> {
        require_warmup(access, ACCOUNT_WARMUP_READ)?;
        let deps = self
            .warmup
            .as_ref()
            .ok_or_else(|| RpcError::new(NOT_SUPPORTED, "account warmup unavailable"))?;
        let accounts = if let Some(ids) = params.ids {
            let mut accounts = Vec::new();
            let mut seen = BTreeSet::new();
            for id in ids {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let id =
                    ProviderAccountId::new(id).map_err(|_| RpcError::new(INVALID_PARAMS, "ids"))?;
                if let Some(account) = self
                    .accounts
                    .get_account(&id)
                    .await
                    .map_err(|_| RpcError::new(UNAVAILABLE, "account directory"))?
                {
                    accounts.push(account);
                }
            }
            accounts
        } else {
            self.accounts
                .list_accounts()
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account directory"))?
        };
        let mut result = Vec::with_capacity(accounts.len());
        for account in accounts {
            require_warmup(access, ACCOUNT_WARMUP_READ)?;
            let supported =
                account.provider().as_str() == "openai" && account.authentication_kind() == "oauth";
            let snapshot = if supported {
                self.quota.read_account(account.id()).await.ok().flatten()
            } else {
                None
            };
            let cooldown = if supported {
                self.quota.cooldown(account.id()).await.ok().flatten()
            } else {
                None
            };
            let protected = snapshot
                .as_ref()
                .and_then(CodexAccountQuotaSnapshot::scheduling_signals)
                .is_some_and(|signals| {
                    account
                        .quota_protection()
                        .reached_usage(signals.protection_usage())
                });
            let models = if params.include_models && supported && account.enabled() {
                deps.catalog
                    .warmup_models(&account)
                    .await
                    .ok()
                    .map(|models| {
                        models
                            .into_iter()
                            .filter(|model| {
                                account
                                    .model_access()
                                    .allows(model.request_model().as_str())
                            })
                            .map(|model| AccountWarmupModel {
                                id: model.request_model().as_str().into(),
                                label: model.display_name().into(),
                                reasoning_efforts: model
                                    .capabilities()
                                    .reasoning_efforts()
                                    .to_vec(),
                            })
                            .collect()
                    })
            } else {
                None
            };
            result.push(AccountWarmupAccount {
                id: account.id().as_str().into(),
                provider: account.provider().as_str().into(),
                authentication_kind: account.authentication_kind().into(),
                enabled: account.enabled(),
                status: account
                    .clone()
                    .with_quota_protection_reached(protected)
                    .status_projection(SystemTime::now(), cooldown)
                    .status
                    .as_str()
                    .into(),
                supported,
                reason_code: reason(&account).map(str::to_owned),
                model_allowed: params
                    .model
                    .as_deref()
                    .map(|model| account.model_access().allows(model)),
                models,
                short_term: snapshot.as_ref().and_then(short_term),
            });
        }
        require_warmup(access, ACCOUNT_WARMUP_READ)?;
        Ok(AccountWarmupReadResult { accounts: result })
    }

    pub(super) async fn run_warmup(
        &self,
        access: &dyn PluginAccess,
        params: AccountWarmupRunParams,
    ) -> Result<AccountWarmupRunResult, RpcError> {
        require_warmup(access, ACCOUNT_WARMUP_RUN)?;
        if !(1000..=15000).contains(&params.timeout_ms)
            || params.model.is_empty()
            || params.model.len() > 128
            || params.prompt.trim().is_empty()
            || params.prompt.len() > 4096
            || params
                .prompt
                .chars()
                .any(|value| value.is_control() && !matches!(value, '\n' | '\r' | '\t'))
            || !matches!(
                params.reasoning_effort.as_str(),
                "auto" | "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
            || params.attempt_id.is_empty()
            || params.attempt_id.len() > 128
            || !params
                .attempt_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(RpcError::new(INVALID_PARAMS, "invalid warmup attempt"));
        }
        let deps = self
            .warmup
            .as_ref()
            .ok_or_else(|| RpcError::new(NOT_SUPPORTED, "account warmup unavailable"))?;
        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_millis(params.timeout_ms);
        let mut dispatched = false;
        let mut received = None;
        let mut lease_guard = None;
        let work = async {
            let account_id = ProviderAccountId::new(&params.account_id)
                .map_err(|_| RpcError::new(INVALID_PARAMS, "accountId"))?;
            let Some(account) = self
                .accounts
                .get_account(&account_id)
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account"))?
            else {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    "account_missing",
                    started,
                ));
            };
            if let Some(reason) = reason(&account) {
                return Ok(terminal(AccountWarmupOutcome::Skipped, reason, started));
            }
            if !account.model_access().allows(&params.model) {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    "model_not_allowed",
                    started,
                ));
            }
            self.quota
                .prepare_scheduling(std::slice::from_ref(&account))
                .await;
            let quota_signals = self.quota.scheduling_signals(&account);
            let protected = account.quota_protection().reached_usage(
                quota_signals
                    .map(|signals| signals.protection_usage())
                    .unwrap_or_default(),
            );
            let cooldown = self
                .quota
                .cooldown(account.id())
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account cooldown"))?;
            let status = account
                .clone()
                .with_quota_protection_reached(protected)
                .status_projection(SystemTime::now(), cooldown)
                .status;
            if status != AccountStatus::Normal {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    if protected {
                        "quota_protection"
                    } else {
                        status.as_str()
                    },
                    started,
                ));
            }
            let policy = deps
                .policy
                .load_account_selection_policy()
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account selection policy"))?;
            let actor = ClientApiKeyId::new("account_warmup")
                .map_err(|_| RpcError::new(INTERNAL, "warmup actor"))?;
            let scheduling = deps
                .leases
                .load_state(
                    &actor,
                    account.provider(),
                    std::slice::from_ref(account.id()),
                )
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account scheduling"))?;
            let Some(signals) = scheduling.signals().get(account.id()).cloned() else {
                return Err(RpcError::new(UNAVAILABLE, "account scheduling"));
            };
            let candidate = AccountCandidate {
                account: account.clone().with_quota_protection_reached(protected),
                signals: signals
                    .with_provider_quota(quota_signals)
                    .with_rate_limit(cooldown),
            };
            let context = AccountSelectionContext {
                policy,
                now: SystemTime::now(),
                excluded_accounts: BTreeSet::new(),
                preferred_account: Some(account.id().clone()),
                preferred_account_overrides_weight: true,
                round_robin_cursor: 0,
                eligibility: AccountEligibilityPolicy::Enforce,
                account_scope: None,
            };
            if AccountSelector
                .select(std::slice::from_ref(&candidate), &context)
                .is_none()
            {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    "account_busy",
                    started,
                ));
            }
            let models = deps
                .catalog
                .warmup_models(&account)
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "model catalog"))?;
            let Some(model) = models
                .iter()
                .find(|model| model.request_model().as_str() == params.model)
            else {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    "model_unsupported",
                    started,
                ));
            };
            let efforts = model.capabilities().reasoning_efforts();
            let reasoning_unsupported = model.capabilities().reasoning()
                == CodexCatalogCapabilityEvidence::DeclaredUnsupported;
            // 显式强度必须有目录证据，不能通过删字段或降档伪装成已生效。
            let effort = if params.reasoning_effort == "auto" {
                ["none", "minimal", "low", "medium", "high", "xhigh"]
                    .into_iter()
                    .find(|effort| efforts.iter().any(|value| value == effort))
                    .unwrap_or("low")
            } else if reasoning_unsupported
                || !efforts
                    .iter()
                    .any(|value| value == &params.reasoning_effort)
            {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    "reasoning_unsupported",
                    started,
                ));
            } else {
                params.reasoning_effort.as_str()
            };
            let deadline_at = SystemTime::now()
                .checked_add(deadline.saturating_duration_since(tokio::time::Instant::now()))
                .ok_or_else(|| RpcError::new(INTERNAL, "warmup deadline"))?;
            let lease = match deps
                .leases
                .try_acquire(ProviderLeaseRequest::Scheduling(
                    ProviderSchedulingLeaseRequest::new(
                        account.provider().clone(),
                        account.id().clone(),
                        account.revision(),
                        account.effective_concurrency(policy.max_concurrent_per_account()),
                        policy.request_interval(),
                        deadline_at,
                    ),
                ))
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account lease"))?
            {
                ProviderLeaseAcquisition::Acquired(lease) => lease,
                ProviderLeaseAcquisition::Busy { .. }
                | ProviderLeaseAcquisition::IntervalPending { .. } => {
                    return Ok(terminal(
                        AccountWarmupOutcome::Skipped,
                        "account_busy",
                        started,
                    ));
                }
            };
            lease_guard = Some(lease);
            require_warmup(access, ACCOUNT_WARMUP_RUN)?;
            // 等待目录与租约之后重读；任何账号变更均留到下次计划，不追着新事实反复发送。
            if self
                .accounts
                .get_account(account.id())
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account"))?
                .as_ref()
                != Some(&account)
            {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    "account_changed",
                    started,
                ));
            }
            let latest_signals = self.quota.scheduling_signals(&account);
            let latest_protected = account.quota_protection().reached_usage(
                latest_signals
                    .map(|signals| signals.protection_usage())
                    .unwrap_or_default(),
            );
            let latest_cooldown = self
                .quota
                .cooldown(account.id())
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "account cooldown"))?;
            let latest_status = account
                .clone()
                .with_quota_protection_reached(latest_protected)
                .status_projection(SystemTime::now(), latest_cooldown)
                .status;
            if latest_status != AccountStatus::Normal {
                return Ok(terminal(
                    AccountWarmupOutcome::Skipped,
                    if latest_protected {
                        "quota_protection"
                    } else {
                        latest_status.as_str()
                    },
                    started,
                ));
            }
            let credential = self
                .repository
                .load_runtime_credential(&account)
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "credential unavailable"))?;
            let authorization = credential
                .authentication
                .authorization_header()
                .map_err(|_| RpcError::new(UNAVAILABLE, "credential unavailable"))?;
            let client = self
                .client
                .for_account(&account)
                .map_err(|_| RpcError::new(UNAVAILABLE, "account egress unavailable"))?;
            let mut request =
                hi_request(&params.model).ok_or_else(|| RpcError::new(INVALID_PARAMS, "model"))?;
            request.body_mut().insert(
                "input".into(),
                json!([{"role":"user","content":[{"type":"input_text","text":params.prompt}]}]),
            );
            if reasoning_unsupported {
                request.body_mut().remove("reasoning");
            } else {
                request
                    .body_mut()
                    .insert("reasoning".into(), json!({"effort":effort}));
            }
            crate::transport::request::scope_request_to_account(
                &mut request,
                &credential.installation_id,
                crate::transport::request::RequestAccountScope::Unknown,
            );
            let context = CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                account.upstream_account_id(),
                &params.attempt_id,
                Some(&credential.installation_id),
            );
            require_warmup(access, ACCOUNT_WARMUP_RUN)?;
            dispatched = true;
            let observation = client.plugin_probe_headers(&request, context).await;
            let mut observation = match observation {
                Ok(observation) => observation,
                Err(kind) => {
                    let mut result = terminal(AccountWarmupOutcome::Unknown, kind, started);
                    result.send_state = AccountWarmupSendState::Ambiguous;
                    return Ok(result);
                }
            };
            observation.rate_limit_headers =
                normalize_reset_after_headers(&observation.rate_limit_headers);
            let result = AccountWarmupRunResult {
                outcome: if observation.status == 200 {
                    AccountWarmupOutcome::Accepted
                } else {
                    AccountWarmupOutcome::Failed
                },
                reason_code: (observation.status != 200).then(|| "upstream_rejected".into()),
                status: Some(observation.status),
                duration_ms: started.elapsed().as_millis() as u64,
                send_state: AccountWarmupSendState::Sent,
                short_term: None,
            };
            received = Some((account, observation));
            Ok(result)
        };
        let mut result = match tokio::time::timeout_at(deadline, work).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) if error.code == FORBIDDEN || error.code == INVALID_PARAMS => {
                return Err(error);
            }
            Ok(Err(error)) => terminal(
                AccountWarmupOutcome::Skipped,
                &error.message.replace(' ', "_"),
                started,
            ),
            Err(_) => {
                let mut result = terminal(
                    if dispatched {
                        AccountWarmupOutcome::Unknown
                    } else {
                        AccountWarmupOutcome::Skipped
                    },
                    "timeout",
                    started,
                );
                if dispatched {
                    result.send_state = AccountWarmupSendState::Ambiguous;
                }
                result
            }
        };
        // 网络预算结束后仍须收敛被动额度和429冷却；不能被外层网络timeout丢掉。
        if let Some((account, observation)) = received {
            // 只把本次响应明确给出的5h事实作为本次确认，不能把合并前旧resetAt冒认新证据。
            let parsed = gateway_protocol::openai::events::parse_rate_limit_headers(
                &observation.rate_limit_headers,
            );
            let current = parsed
                .as_ref()
                .and_then(|limits| limits.limits.get("codex"))
                .and_then(|limit| {
                    [limit.primary, limit.secondary]
                        .into_iter()
                        .flatten()
                        .find(|window| {
                            window
                                .window_minutes
                                .is_some_and(|minutes| minutes.abs_diff(300) <= 15)
                        })
                });
            let observed = self
                .quota
                .synchronize_passive_headers(&account, &observation.rate_limit_headers)
                .await
                .unwrap_or(false);
            if observation.status == 429 {
                let _ = self
                    .quota
                    .apply_rate_limit_429(&account, observation.retry_after, SystemTime::now())
                    .await;
            }
            if observed && let Some(reset) = current.and_then(|window| window.reset_at) {
                let snapshot = self.quota.read_account(account.id()).await.ok().flatten();
                result.short_term = snapshot.as_ref().and_then(short_term).filter(|window| {
                    // 相对reset-after在两次解析时可能跨秒，只容许解析/落库的一秒舍入差。
                    window
                        .reset_at
                        .as_ref()
                        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
                        .is_some_and(|at| at.timestamp().abs_diff(reset) <= 2)
                });
            }
        }
        result.duration_ms = started.elapsed().as_millis() as u64;
        drop(lease_guard);
        Ok(result)
    }
}
