//! 请求路径的通用插件桥；只维护资格、交付及重放边界，不解释业务原因码。
use super::*;
use gateway_core::{account::ProviderAccountId, plugin::*};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct Selection {
    pub preferred: Vec<ProviderAccountId>,
    pub members: Vec<ProviderAccountId>,
    pub owners: BTreeMap<ProviderAccountId, String>,
    pub watchers: BTreeMap<ProviderAccountId, Vec<String>>,
}

pub(super) fn unavailable(plugin: &str, code: &str, message: &str, sent: bool) -> ProviderError {
    provider_error(
        ProviderErrorKind::Unavailable,
        if sent {
            UpstreamSendState::Sent
        } else {
            UpstreamSendState::NotSent
        },
    )
    .with_diagnostic(
        ProviderDiagnostic::new(format!("插件 {plugin}：{message}"))
            .with_plugin_classification(code),
    )
}

pub(super) async fn select(
    hooks: &dyn PluginHooks,
    candidates: Vec<ProviderAccount>,
    context: &mut AttemptContext,
    model: &str,
) -> Result<Selection, ProviderError> {
    let mut selection = Selection::default();
    let mut grouped: BTreeMap<String, Vec<ProviderAccountId>> = BTreeMap::new();
    let mut unavailable_plugins = BTreeSet::new();
    // 同一账号的分组分属多个插件时单独记录：失败关闭，并以 plugin_ownership_conflict 报告。
    let mut conflicting_plugins = BTreeSet::new();
    let mut excluded = Vec::new();
    for account in &candidates {
        let mut owners = BTreeMap::new();
        let mut watchers = BTreeSet::new();
        if let Some(entry) = context
            .account_scope()
            .and_then(|scope| scope.directory().account(account.id()))
        {
            for group in entry.group_ids() {
                match hooks.ownership(group.as_str()) {
                    Ownership::Owned { plugin, available } => {
                        owners.insert(plugin, available);
                    }
                    Ownership::Watched { plugins } => watchers.extend(plugins),
                    Ownership::NotOwned => {}
                }
            }
        }
        if owners.len() > 1 {
            // 不做仲裁：账号排除出插件候选，全部候选都被排除时返回冲突诊断。
            conflicting_plugins.extend(owners.into_keys());
            excluded.push(account.id().clone());
        } else if let Some((plugin, available)) = owners.into_iter().next() {
            if available {
                grouped
                    .entry(plugin)
                    .or_default()
                    .push(account.id().clone());
            } else {
                unavailable_plugins.insert(plugin);
                excluded.push(account.id().clone());
            }
        } else if !watchers.is_empty() {
            selection
                .watchers
                .insert(account.id().clone(), watchers.into_iter().collect());
        }
    }
    for (plugin, accounts) in &grouped {
        let pinned = if context.continuation_attempt() == ContinuationAttempt::Native {
            context
                .continuation()
                .and_then(ContinuationBinding::pinned)
                .map(|binding| binding.account())
                .filter(|id| accounts.contains(id))
                .map(|id| id.as_str().to_owned())
        } else {
            None
        };
        let params = RouteSelectParams {
            request_id: context.request_id().as_str().to_owned(),
            model: model.to_owned(),
            accounts: accounts.iter().map(|id| id.as_str().to_owned()).collect(),
            ordinary_available: candidates.iter().any(|account| {
                !accounts.contains(account.id()) && !excluded.contains(account.id())
            }),
            pinned: pinned.clone(),
        };
        let result = tokio::select! {
            biased;
            _ = context.cancellation().cancelled() => return Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)),
            _ = tokio::time::sleep(remaining(context.deadline()).ok_or_else(|| provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent))?) => return Err(provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent)),
            result = hooks.route_select(plugin, params) => result,
        };
        let result = match result {
            Ok(result) if pinned.is_none() || result.pinned_decision.is_some() => result,
            _ => {
                unavailable_plugins.insert(plugin.clone());
                excluded.extend(accounts.iter().cloned());
                continue;
            }
        };
        if pinned.is_some() && result.pinned_decision == Some(PinnedDecision::Escape) {
            return Err(provider_error(
                ProviderErrorKind::NoEligibleAccount,
                UpstreamSendState::NotSent,
            ));
        }
        for id in result.preferred {
            if let Some(account) = accounts.iter().find(|account| account.as_str() == id)
                && !result
                    .blocked
                    .iter()
                    .any(|blocked| blocked.account_id == id)
                && !selection.preferred.contains(account)
            {
                selection.preferred.push(account.clone());
            }
        }
        selection.members.extend(accounts.iter().cloned());
        selection
            .owners
            .extend(accounts.iter().cloned().map(|id| (id, plugin.clone())));
    }
    if !conflicting_plugins.is_empty()
        && candidates
            .iter()
            .all(|account| excluded.contains(account.id()))
    {
        return Err(unavailable(
            &conflicting_plugins
                .into_iter()
                .collect::<Vec<_>>()
                .join(", "),
            "plugin_ownership_conflict",
            "账号的分组归属多个插件，已失败关闭",
            false,
        ));
    }
    if !unavailable_plugins.is_empty()
        && candidates
            .iter()
            .all(|account| excluded.contains(account.id()))
    {
        return Err(unavailable(
            &unavailable_plugins
                .into_iter()
                .collect::<Vec<_>>()
                .join(", "),
            "plugin_unavailable",
            "插件不可用",
            false,
        ));
    }
    context.exclude_accounts(excluded);
    Ok(selection)
}

struct FinishData {
    leases: Vec<String>,
    started: Option<Instant>,
    first_byte_ms: Option<u64>,
    outcome: RequestOutcome,
}

pub(super) struct Finish {
    hooks: Arc<dyn PluginHooks>,
    plugin: String,
    request_id: String,
    cancellation: CancellationToken,
    created: Instant,
    data: Mutex<FinishData>,
}
impl Finish {
    pub fn new(hooks: Arc<dyn PluginHooks>, plugin: String, context: &AttemptContext) -> Arc<Self> {
        Arc::new(Self {
            hooks,
            plugin,
            request_id: context.request_id().as_str().to_owned(),
            cancellation: context.cancellation().clone(),
            created: context.timing_started_at(),
            data: Mutex::new(FinishData {
                leases: vec![],
                started: None,
                first_byte_ms: None,
                outcome: RequestOutcome::Interrupted,
            }),
        })
    }
}
impl Drop for Finish {
    fn drop(&mut self) {
        let data = self.data.get_mut().expect("plugin finish");
        // 没有取得租约（未收到 route.prepare 的 use）时插件没有需要结束的状态，不发送 request.finish。
        let leases = std::mem::take(&mut data.leases);
        if leases.is_empty() {
            return;
        }
        let outcome = if self.cancellation.is_cancelled()
            && !matches!(data.outcome, RequestOutcome::Completed)
        {
            RequestOutcome::Cancelled
        } else {
            data.outcome.clone()
        };
        let hooks = self.hooks.clone();
        let plugin = self.plugin.clone();
        let params = RequestFinishParams {
            request_id: self.request_id.clone(),
            // 占位仅用于 clone 基础字段，每个租约投递时都会被替换为真实 leaseId。
            lease_id: String::new(),
            outcome,
            duration_ms: self.created.elapsed().as_millis() as u64,
            first_byte_ms: data.first_byte_ms,
            usage: None,
        };
        tokio::spawn(async move {
            for lease_id in leases {
                hooks
                    .request_finish(
                        &plugin,
                        RequestFinishParams {
                            lease_id,
                            ..params.clone()
                        },
                    )
                    .await;
            }
        });
    }
}

pub(super) fn track(mut stream: EventStream, finish: Option<Arc<Finish>>) -> EventStream {
    let Some(finish) = finish else {
        return stream;
    };
    Box::pin(async_stream::stream! {
        // guard 在流被取消或丢弃时同样释放，不能把 finish 放在流尾等待再次 poll。
        let finish = finish;
        while let Some(event) = stream.next().await {
            {
                let mut data = finish.data.lock().expect("plugin finish");
                match &event {
                    Ok(event) => {
                        if event.wire_event().is_some() && data.first_byte_ms.is_none() {
                            data.first_byte_ms = data.started.map(|started| started.elapsed().as_millis() as u64);
                        }
                        if terminal_response_is_incomplete(std::slice::from_ref(event)) {
                            data.outcome = RequestOutcome::Failed;
                        } else if terminal_response_output(event).is_some()
                            || event.wire_event().is_some_and(|wire| wire.raw_json_body().is_some())
                            || event.canonical_facts().iter().any(|event| matches!(event, GatewayEvent::Completed(_))) {
                            data.outcome = RequestOutcome::Completed;
                        }
                    }
                    Err(error) => data.outcome = if error.kind() == ProviderErrorKind::Cancelled { RequestOutcome::Cancelled } else { RequestOutcome::Failed },
                }
            }
            yield event;
        }
    })
}

pub(super) struct Attempt<'a> {
    pub hooks: &'a dyn PluginHooks,
    pub plugin: &'a str,
    pub client: &'a CodexBackendClient,
    pub request: &'a mut CodexResponsesRequest,
    pub context: &'a AttemptContext,
    pub lease: &'a CodexCredentialLease,
    pub account: &'a ProviderAccount,
    pub authorization: &'a SecretString,
    pub selection: CodexAccountSelectionTelemetry<'a>,
    pub finish: &'a Finish,
    pub failure_context: &'a OpenAiFailureContext<'a>,
}

async fn bounded<T>(
    context: &AttemptContext,
    future: impl std::future::Future<Output = T>,
) -> Result<T, CodexHandshakeAttemptError> {
    tokio::select! {
        biased;
        _ = context.cancellation().cancelled() => Err(CodexHandshakeAttemptError::Cancelled),
        _ = tokio::time::sleep(remaining(context.deadline()).ok_or(CodexHandshakeAttemptError::Timeout)?) => Err(CodexHandshakeAttemptError::Timeout),
        result = future => Ok(result),
    }
}

async fn delay(
    context: &AttemptContext,
    milliseconds: u64,
) -> Result<(), CodexHandshakeAttemptError> {
    let available = remaining(context.deadline())
        .and_then(|duration| duration.checked_sub(Duration::from_millis(1500)))
        .filter(|duration| !duration.is_zero())
        .ok_or(CodexHandshakeAttemptError::Timeout)?;
    bounded(
        context,
        tokio::time::sleep(Duration::from_millis(milliseconds).min(available)),
    )
    .await
}

fn materials(
    request: &mut CodexResponsesRequest,
    upstream: &Upstream,
) -> Result<(), CodexHandshakeAttemptError> {
    request.passthrough_headers.remove("x-codex-turn-state");
    request.turn_state = upstream.turn_state.clone();
    let replace = |entity: &mut Value| {
        if let Some(metadata) = entity
            .get_mut("client_metadata")
            .and_then(Value::as_object_mut)
            && metadata.contains_key("x-codex-turn-state")
        {
            if let Some(state) = &upstream.turn_state {
                metadata.insert("x-codex-turn-state".into(), Value::String(state.clone()));
            } else {
                metadata.remove("x-codex-turn-state");
            }
        }
    };
    if let Some(original) = &request.original_json {
        let mut entity: Value = serde_json::from_slice(original).map_err(|_| {
            CodexHandshakeAttemptError::Policy(provider_error(
                ProviderErrorKind::Protocol,
                UpstreamSendState::NotSent,
            ))
        })?;
        replace(&mut entity);
        request.original_json = Some(serde_json::to_vec(&entity).expect("JSON entity").into());
    }
    if let Some(Value::Object(metadata)) = request.body_mut().get_mut("client_metadata")
        && metadata.contains_key("x-codex-turn-state")
    {
        if let Some(state) = &upstream.turn_state {
            metadata.insert("x-codex-turn-state".into(), Value::String(state.clone()));
        } else {
            metadata.remove("x-codex-turn-state");
        }
    }
    Ok(())
}

impl Attempt<'_> {
    pub async fn send(self) -> Result<CodexBackendStreamingResponse, CodexHandshakeAttemptError> {
        let replay_safe = request_replay_safe(self.request);
        let mut attempt = self.context.plugin_attempt_budget().next_attempt();
        let mut sent = false;
        let mut held: Option<(String, Upstream)> = None;
        let failure = |code: &str, message: &str, sent| {
            let error = unavailable(self.plugin, code, message, sent);
            CodexHandshakeAttemptError::Policy(
                if sent && replay_safe && self.context.plugin_attempt_budget().available() {
                    error.with_replay_safe()
                } else {
                    error
                },
            )
        };
        loop {
            if held.is_none() {
                let params = RoutePrepareParams {
                    request_id: self.context.request_id().as_str().to_owned(),
                    account_id: self.account.id().as_str().to_owned(),
                    attempt,
                    model: self
                        .request
                        .body()
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    deadline_unix_ms: self
                        .context
                        .deadline()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64,
                    replay_safe,
                };
                match bounded(self.context, self.hooks.route_prepare(self.plugin, params))
                    .await?
                    .map_err(|_| failure("plugin_unavailable", "route.prepare 不可用", sent))?
                {
                    RoutePrepareResult::Wait { retry_after_ms, .. } => {
                        delay(self.context, retry_after_ms).await?;
                        continue;
                    }
                    RoutePrepareResult::Fail { reason } => {
                        return Err(failure(&reason.code, &reason.message, sent));
                    }
                    RoutePrepareResult::Use {
                        account_id,
                        lease_id,
                        upstream,
                    } => {
                        // use 不能绕过调度器把凭据换成插件返回的另一个账号。
                        if account_id != self.account.id().as_str() {
                            return Err(failure(
                                "plugin_unavailable",
                                "route.prepare 账号不匹配",
                                sent,
                            ));
                        }
                        {
                            let mut data = self.finish.data.lock().expect("plugin finish");
                            if !data.leases.contains(&lease_id) {
                                data.leases.push(lease_id.clone());
                            }
                        }
                        materials(self.request, &upstream)?;
                        held = Some((lease_id, upstream));
                    }
                }
            }
            let (lease_id, upstream) = held.as_ref().expect("prepared lease");
            self.context
                .plugin_attempt_budget()
                .claim()
                .ok_or_else(|| failure("plugin_unavailable", "上游尝试预算已耗尽", sent))?;
            let trace = self.context.trace();
            let mut wire = codex_request_context(
                self.request,
                self.context.request_id().as_str(),
                self.account,
                self.lease.installation_id(),
                self.authorization,
                None,
                self.selection,
            )
            .with_trace(&trace);
            wire.cookie_header = upstream.cookie.as_deref();
            wire.turn_state = upstream.turn_state.as_deref();
            let started = Instant::now();
            self.finish
                .data
                .lock()
                .expect("plugin finish")
                .started
                .get_or_insert(started);
            sent = true;
            let response = create_response_attempt(
                self.client,
                self.request,
                wire,
                self.account.id().as_str(),
                self.context.deadline(),
                self.context.cancellation(),
            )
            .await;
            let (status, error_kind, turn_state, set_cookies) = match &response {
                Ok(response) => (
                    response.diagnostics.status_code,
                    None,
                    response_turn_state(response),
                    response.set_cookie_headers.clone(),
                ),
                Err(error) => {
                    let upstream = match error {
                        CodexHandshakeAttemptError::Client(error) => error.upstream_failure(),
                        _ => None,
                    };
                    let status = upstream
                        .as_ref()
                        .and_then(|failure| failure.status.map(|status| status.as_u16()));
                    let turn_state = upstream
                        .as_ref()
                        .and_then(|failure| failure.client_response.as_ref())
                        .and_then(|response| {
                            response
                                .client_headers()
                                .iter()
                                .find(|(name, _)| name.eq_ignore_ascii_case("x-codex-turn-state"))
                        })
                        .and_then(|(_, value)| std::str::from_utf8(value).ok())
                        .map(str::to_owned);
                    (
                        status,
                        status.is_none().then(|| {
                            match error {
                                CodexHandshakeAttemptError::Cancelled => "cancelled",
                                CodexHandshakeAttemptError::Timeout => "timeout",
                                _ => "connect",
                            }
                            .to_owned()
                        }),
                        turn_state,
                        upstream
                            .map(|failure| failure.set_cookie_headers)
                            .unwrap_or_default(),
                    )
                }
            };
            // 即使网络失败或取消也给插件一次响应头事实；运行时自身的钩子预算保持有界。
            let head = self.hooks.response_head(
                self.plugin,
                ResponseHeadParams {
                    request_id: self.context.request_id().as_str().to_owned(),
                    lease_id: lease_id.clone(),
                    attempt,
                    status,
                    error_kind,
                    state_returned: turn_state.is_some(),
                    turn_state,
                    set_cookies,
                    duration_ms: started.elapsed().as_millis() as u64,
                    replay_safe,
                },
            );
            let verdict =
                tokio::time::timeout(remaining(self.context.deadline()).unwrap_or_default(), head)
                    .await
                    .unwrap_or_else(|_| Err(RpcError::new(UNAVAILABLE, "response.head deadline")));
            if matches!(verdict, Ok(ResponseHeadResult::Accept)) {
                return response;
            }
            // 插件拒绝不能吞掉账号级 401/429 等反馈，先执行原有失败处理再解释插件动作。
            if let Err(error) = response {
                let mapped = map_handshake_attempt_error(error);
                apply_failure(self.failure_context, self.account, &mapped).await;
            }
            match verdict {
                Ok(ResponseHeadResult::Retry { delay_ms })
                    if replay_safe && self.context.plugin_attempt_budget().available() =>
                {
                    bounded(
                        self.context,
                        tokio::time::sleep(Duration::from_millis(delay_ms)),
                    )
                    .await?;
                }
                Ok(ResponseHeadResult::Switch)
                    if replay_safe && self.context.plugin_attempt_budget().available() =>
                {
                    held = None;
                }
                Ok(ResponseHeadResult::Fail { reason }) => {
                    return Err(failure(&reason.code, &reason.message, true));
                }
                // 插件要求 retry/switch，但内核边界拒绝：不可安全重放优先于尝试上限，两者都说明插件原本的动作。
                Ok(action @ (ResponseHeadResult::Retry { .. } | ResponseHeadResult::Switch)) => {
                    let requested = match action {
                        ResponseHeadResult::Retry { .. } => "同网关重试（retry）",
                        _ => "换网关重发（switch）",
                    };
                    let (code, message) = if replay_safe {
                        (
                            "plugin_attempt_limit",
                            format!("插件要求{requested}，但已达到每个请求 4 次上游尝试上限"),
                        )
                    } else {
                        (
                            "plugin_retry_unsafe",
                            format!("插件要求{requested}，但请求不可安全重放"),
                        )
                    };
                    return Err(failure(code, &message, true));
                }
                // 插件无响应、超时或返回无效结果，沿用插件不可用。
                _ => {
                    return Err(failure("plugin_unavailable", "插件响应不可用", true));
                }
            }
            attempt += 1;
        }
    }
}

pub(super) async fn observe(
    hooks: &dyn PluginHooks,
    plugins: &[String],
    context: &AttemptContext,
    account: &ProviderAccount,
    request: &CodexResponsesRequest,
    response: &mut CodexBackendStreamingResponse,
) -> Result<(), ProviderError> {
    let replay_safe = request_replay_safe(request);
    let can_switch = context.continuation_attempt() == ContinuationAttempt::None
        && context.required_account().is_none()
        && !context.is_diagnostic_required_account()
        && replay_safe
        && context.plugin_attempt_budget().available();
    for plugin in plugins {
        let turn_state = response_turn_state(response);
        let result = bounded(
            context,
            hooks.response_observe(
                plugin,
                ResponseObserveParams {
                    request_id: context.request_id().as_str().to_owned(),
                    account_id: account.id().as_str().to_owned(),
                    attempt: context.attempt_index().get(),
                    model: request
                        .body()
                        .get("model")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into(),
                    request_had_turn_state: request_carried_turn_state(request),
                    status: response.diagnostics.status_code,
                    error_kind: None,
                    state_returned: turn_state.is_some(),
                    turn_state,
                    set_cookies: response.set_cookie_headers.clone(),
                    replay_safe,
                    can_switch,
                },
            ),
        )
        .await;
        match result {
            Ok(Ok(ResponseObserveResult::SwitchAccount { reason })) if can_switch => {
                return Err(unavailable(plugin, &reason.code, &reason.message, true)
                    .with_pre_delivery_retry());
            }
            Ok(Ok(_)) => {}
            _ => tracing::warn!(
                plugin_id = plugin,
                request_id = context.request_id().as_str(),
                "response.observe unavailable"
            ),
        }
    }
    Ok(())
}
