//! 插件能力的 OpenAI/账号出口适配，不参与正常推理请求路径。
use crate::{
    credential::{CodexCredentialQuotaService, CodexCredentialRepository},
    transport::{CodexBackendClient, CodexRequestContext},
};
use async_trait::async_trait;
use futures::StreamExt;
use gateway_core::{
    account::{ProviderAccount, ProviderAccountId, ProviderAccountStore},
    plugin::*,
};
use secrecy::ExposeSecret;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::{Duration, SystemTime},
};
use tokio::sync::Semaphore;

pub struct OpenAiPluginUpstream {
    accounts: Arc<dyn ProviderAccountStore>,
    repository: CodexCredentialRepository,
    quota: Arc<CodexCredentialQuotaService>,
    client: CodexBackendClient,
    global: Semaphore,
    per_account: Mutex<HashMap<String, Weak<Semaphore>>>,
}
impl OpenAiPluginUpstream {
    pub fn new(
        accounts: Arc<dyn ProviderAccountStore>,
        repository: CodexCredentialRepository,
        quota: Arc<CodexCredentialQuotaService>,
        client: CodexBackendClient,
    ) -> Self {
        Self {
            accounts,
            repository,
            quota,
            client,
            global: Semaphore::new(8),
            per_account: Mutex::new(HashMap::new()),
        }
    }
    async fn account(&self, id: &str, openai: bool) -> Result<ProviderAccount, RpcError> {
        let id =
            ProviderAccountId::new(id).map_err(|_| RpcError::new(INVALID_PARAMS, "accountId"))?;
        let account = self
            .accounts
            .get_account(&id)
            .await
            .map_err(|_| RpcError::new(UNAVAILABLE, "account"))?
            .ok_or_else(|| RpcError::new(FORBIDDEN, "account unavailable"))?;
        if openai && account.provider().as_str() != "openai" {
            return Err(RpcError::new(NOT_SUPPORTED, "OpenAI accounts only"));
        }
        Ok(account)
    }
}
#[async_trait]
impl PluginUpstream for OpenAiPluginUpstream {
    async fn quota(&self, params: QuotaReadParams) -> Result<QuotaReadResult, RpcError> {
        let account = self.account(&params.account_id, true).await?;
        let snapshot = self
            .quota
            .read_account(account.id())
            .await
            .map_err(|_| RpcError::new(UNAVAILABLE, "quota.read"))?;
        let signals = self.quota.scheduling_signals(&account);
        // 协议 0.x 原样透传额度与调度快照，字段合同在 P1 网关池插件使用时冻结。
        let snapshot = snapshot.map(|snapshot| json!({
            "accountId": snapshot.account_id().as_str(), "credentialRevision": snapshot.credential_revision().get(),
            "observedAt": chrono::DateTime::<chrono::Utc>::from(snapshot.observed_at()), "planType": snapshot.plan_type(),
            "credits": snapshot.credits().map(|credits| json!({"balance": credits.balance.map(|balance| balance.to_string()), "unlimited": credits.unlimited})),
            "remainingPercent": snapshot.fact().remaining_percent(), "resetsAt": snapshot.fact().resets_at(),
            "quota": {"access": snapshot.quota().access().as_str(), "evidence": snapshot.quota().evidence().map(|value| value.as_str()), "observedAt": snapshot.quota().observed_at().map(chrono::DateTime::<chrono::Utc>::from), "resetAt": snapshot.quota().reset_at().map(chrono::DateTime::<chrono::Utc>::from)},
            "windows": snapshot.windows().iter().map(|window| json!({"key": window.key(), "source": window.source(), "accountWide": window.is_account_wide(), "limitName": window.limit_name(), "kind": format!("{:?}", window.kind()), "role": format!("{:?}", window.role()), "windowSeconds": window.window_seconds(), "usedPercent": window.used_percent(), "resetAt": window.reset_at(), "limitReached": window.limit_reached()})).collect::<Vec<_>>()
        }));
        Ok(QuotaReadResult {
            snapshot: json!({ "quota": snapshot, "schedulingSignals": signals.map(|signals| json!({"resetAt": signals.reset_at().map(chrono::DateTime::<chrono::Utc>::from), "remainingRank": signals.remaining_rank()})) }),
        })
    }
    async fn probe(
        &self,
        access: &dyn PluginAccess,
        accounts: &dyn PluginAccounts,
        params: UpstreamProbeParams,
    ) -> Result<UpstreamProbeResult, RpcError> {
        validate_timeout(params.timeout_ms)?;
        let started = tokio::time::Instant::now();
        let deadline = started + Duration::from_millis(params.timeout_ms);
        let account_limit = {
            let mut limits = self.per_account.lock().expect("probe limits");
            limits.retain(|_, value| value.strong_count() > 0);
            if let Some(limit) = limits.get(&params.account_id).and_then(Weak::upgrade) {
                limit
            } else {
                let limit = Arc::new(Semaphore::new(2));
                limits.insert(params.account_id.clone(), Arc::downgrade(&limit));
                limit
            }
        };
        // 先排账号队列，再占全局名额；一个繁忙账号不能耗尽其他账号的探测许可。
        let (_account, _global) = tokio::time::timeout_at(deadline, async {
            (account_limit.acquire().await, self.global.acquire().await)
        })
        .await
        .map_err(|_| RpcError::new(UNAVAILABLE, "probe_busy"))?;
        let result = tokio::time::timeout_at(deadline, async {
            require_account(access, accounts, &params.account_id).await?;
            let account = self.account(&params.account_id, true).await?;
            if !account.enabled() {
                return Err(RpcError::new(FORBIDDEN, "account disabled"));
            }
            if account.authentication_kind() != "oauth" {
                return Err(RpcError::new(FORBIDDEN, "OAuth required"));
            }
            if !account.model_access().allows(&params.model) {
                return Err(RpcError::new(FORBIDDEN, "model not allowed"));
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
            let mut request = crate::gateway_pool::hi_request(&params.model)
                .ok_or_else(|| RpcError::new(INVALID_PARAMS, "model"))?;
            crate::transport::request::scope_request_to_account(
                &mut request,
                &credential.installation_id,
                crate::transport::request::RequestAccountScope::Unknown,
            );
            let session = uuid::Uuid::new_v4().to_string();
            let mut context = CodexRequestContext::auxiliary(
                authorization.expose_secret(),
                account.upstream_account_id(),
                &session,
                Some(&credential.installation_id),
            );
            context.session_id = Some(&session);
            context.turn_state = params.turn_state.as_deref();
            let own_cookie = if params.include_account_cookies {
                crate::provider::plugin_cookie_header(&credential.cookies)
                    .map_err(|_| RpcError::new(UNAVAILABLE, "account cookies unavailable"))?
            } else {
                None
            };
            let cookie = crate::provider::merged_cookie_header(
                own_cookie.as_ref().map(ExposeSecret::expose_secret),
                &params
                    .cookie
                    .as_deref()
                    .into_iter()
                    .flat_map(|value| value.split(';').map(str::to_owned))
                    .collect::<Vec<_>>(),
            );
            context.cookie_header = cookie.as_deref();
            let observation = client.plugin_probe_headers(&request, context).await;
            Ok((account, observation))
        })
        .await;
        let (account, observation) = match result {
            Ok(result) => result?,
            Err(_) => return Ok(no_response("timeout", started)),
        };
        match observation {
            Ok(observation) => {
                // 响应头到达后按现有网关池完成额度观测，不能因网络预算耗尽跳过 429 冷却。
                let observe_quota = async {
                    let _ = self
                        .quota
                        .synchronize_passive_headers(&account, &observation.rate_limit_headers)
                        .await;
                    if observation.status == 429 {
                        let _ = self
                            .quota
                            .apply_rate_limit_429(
                                &account,
                                observation.retry_after,
                                SystemTime::now(),
                            )
                            .await;
                    }
                };
                observe_quota.await;
                Ok(UpstreamProbeResult {
                    status: Some(observation.status),
                    error_kind: None,
                    state_returned: observation.state_returned,
                    turn_state: observation.state,
                    set_cookies: observation.cookies,
                    rate_limit_headers: observation.rate_limit_headers,
                    retry_after_ms: observation
                        .retry_after
                        .map(|duration| duration.as_millis() as u64),
                    duration_ms: started.elapsed().as_millis() as u64,
                })
            }
            Err(kind) => Ok(no_response(kind, started)),
        }
    }

    async fn fetch(&self, params: HttpFetchParams) -> Result<HttpFetchResult, RpcError> {
        validate_timeout(params.timeout_ms)?;
        let client = match &params.egress {
            Egress::Direct => crate::transport::client::build_reqwest_client(),
            Egress::Account { account_id } => {
                let account = self.account(account_id, false).await?;
                crate::transport::client::build_account_http_client(
                    account.id().as_str(),
                    account.outbound_proxy(),
                )
            }
        }
        .map_err(|_| RpcError::new(UNAVAILABLE, "http client"))?;
        let method = match params.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
        };
        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in params.headers {
            headers.append(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "header name"))?,
                reqwest::header::HeaderValue::from_str(&value)
                    .map_err(|_| RpcError::new(INVALID_PARAMS, "header value"))?,
            );
        }
        let response = async {
            let mut request = client.request(method, &params.url).headers(headers);
            if let Some(body) = params.body {
                request = request.body(body);
            }
            let response = request
                .send()
                .await
                .map_err(|_| RpcError::new(UNAVAILABLE, "http.fetch"))?;
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.to_string(), value.to_owned()))
                })
                .collect();
            let mut stream = response.bytes_stream();
            let mut bytes = Vec::new();
            let mut truncated = false;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|_| RpcError::new(UNAVAILABLE, "http.fetch body"))?;
                // 多读一个 UTF-8 字符，避免原始字节上限把合法字符截成替换字符。
                let remaining = (512 * 1024usize + 4).saturating_sub(bytes.len());
                bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                if chunk.len() > remaining {
                    truncated = true;
                    break;
                }
            }
            let mut body = String::from_utf8_lossy(&bytes).into_owned();
            // 按完整 JSON 字符串编码长度计数（含双引号），替换字符同样消耗预算。
            let mut encoded_len = 2;
            for (offset, ch) in body.char_indices() {
                let length = match ch {
                    '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
                    '\u{00}'..='\u{1f}' => 6,
                    _ => ch.len_utf8(),
                };
                if encoded_len + length > 512 * 1024 {
                    body.truncate(offset);
                    truncated = true;
                    break;
                }
                encoded_len += length;
            }
            Ok(HttpFetchResult {
                status,
                headers,
                body,
                truncated: Some(truncated),
            })
        };
        tokio::time::timeout(Duration::from_millis(params.timeout_ms), response)
            .await
            .map_err(|_| RpcError::new(TIMEOUT, "http.fetch"))?
    }
}
fn no_response(kind: &str, started: tokio::time::Instant) -> UpstreamProbeResult {
    UpstreamProbeResult {
        status: None,
        error_kind: Some(kind.into()),
        state_returned: false,
        turn_state: None,
        set_cookies: vec![],
        rate_limit_headers: vec![],
        retry_after_ms: None,
        duration_ms: started.elapsed().as_millis() as u64,
    }
}
