//! Key 级 Excel 分发：共享 worker，账号 lease 与请求生命周期由 Core 保持。
use super::*;
use gateway_core::operation::RawJsonPayload;
use gateway_core::routing::{
    ProviderCatalogUnavailable, ProviderModelContent, ProviderModelDescriptor,
};
use std::path::Path;

const WORKER_RESPONSES: &str = "http://localhost/internal/responses";
const WORKER_IMAGE_GENERATIONS: &str = "http://localhost/internal/images/generations";
const WORKER_IMAGE_EDITS: &str = "http://localhost/internal/images/edits";
const MAX_WORKER_BODY: usize = 8 * 1024 * 1024;

fn catalog() -> Result<Vec<ProviderModelDescriptor>, ProviderCatalogUnavailable> {
    let data: Value = serde_json::from_str(include_str!("excel_catalog.json"))
        .map_err(|_| ProviderCatalogUnavailable)?;
    data.get("models")
        .and_then(Value::as_array)
        .ok_or(ProviderCatalogUnavailable)?
        .iter()
        .map(|model| {
            let id = model
                .get("slug")
                .and_then(Value::as_str)
                .ok_or(ProviderCatalogUnavailable)?;
            Ok(ProviderModelDescriptor {
                model: UpstreamModelId::new(id).map_err(|_| ProviderCatalogUnavailable)?,
                content: ProviderModelContent::Native(
                    RawJsonPayload::new(
                        PROVIDER_NAME,
                        Bytes::from(
                            serde_json::to_vec(model).map_err(|_| ProviderCatalogUnavailable)?,
                        ),
                    )
                    .map_err(|_| ProviderCatalogUnavailable)?,
                ),
            })
        })
        .collect()
}

impl CodexProvider {
    pub fn with_excel_worker_socket(
        mut self,
        socket: Option<&Path>,
    ) -> Result<Self, CodexProviderConfigError> {
        if let Some(socket) = socket {
            if !socket.is_absolute() {
                return Err(CodexProviderConfigError::InvalidWorkerSocket);
            }
            #[cfg(unix)]
            {
                self.excel_worker = Some(
                    Client::builder()
                        .unix_socket(socket)
                        .no_proxy()
                        .redirect(reqwest::redirect::Policy::none())
                        .connect_timeout(Duration::from_secs(3))
                        .pool_max_idle_per_host(4)
                        .build()
                        .map_err(|_| CodexProviderConfigError::InvalidWorkerSocket)?,
                );
            }
            #[cfg(not(unix))]
            {
                return Err(CodexProviderConfigError::InvalidWorkerSocket);
            }
        }
        Ok(self)
    }

    pub(super) fn excel_catalog(
        &self,
    ) -> Result<Vec<ProviderModelDescriptor>, ProviderCatalogUnavailable> {
        self.excel_worker
            .as_ref()
            .ok_or(ProviderCatalogUnavailable)?;
        catalog()
    }

    pub(super) async fn execute_excel(
        &self,
        request: ProviderRequest,
        context: AttemptContext,
    ) -> Result<ProviderStream, ProviderError> {
        let fail = || {
            provider_error(
                ProviderErrorKind::InvalidRequest,
                UpstreamSendState::NotSent,
            )
        };
        let client = self.excel_worker.clone().ok_or_else(|| {
            provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::NotSent)
        })?;
        if request.candidate().provider().as_str() != PROVIDER_NAME {
            return Err(fail());
        }
        let (body, endpoint) = match request.operation() {
            Operation::Generate(generate) => (
                Value::Object(generate.protocol_payload().body().clone()),
                WORKER_RESPONSES,
            ),
            Operation::GenerateImage(image) => (
                serde_json::from_slice::<Value>(image.payload().body()).map_err(|_| fail())?,
                match image.kind() {
                    ImageRequestKind::Generation => WORKER_IMAGE_GENERATIONS,
                    ImageRequestKind::Edit => WORKER_IMAGE_EDITS,
                },
            ),
            _ => return Err(fail()),
        };
        let model = body
            .get("model")
            .and_then(Value::as_str)
            .or_else(|| (endpoint != WORKER_RESPONSES).then_some("gpt-image-2"))
            .ok_or_else(fail)?
            .to_owned();
        // 模型别名与拒绝响应由原 Bridge 决定，Rust 不再引入第二套映射。
        let scope = context.account_scope().ok_or_else(fail)?;
        if scope.binding_revision() == 0 {
            return Err(fail());
        }
        let revision = scope.binding_revision();
        let session_affinity = match request.operation() {
            Operation::Generate(generate) => encode_generate_request(generate, &model, None)
                .ok()
                .and_then(|request| {
                    derive_codex_session_affinity(&request, context.client_api_key_ref())
                }),
            _ => None,
        };
        let started = Instant::now();
        let lease = Arc::new(
            self.selector
                .select_for_provider_endpoint(&SelectCodexProviderEndpointCredential {
                    request_url: &self.responses_url,
                    attempt: &context,
                    session_affinity: session_affinity.as_ref(),
                })
                .await
                .map_err(map_selection_error)?,
        );
        let secret = lease.authentication().oauth().ok_or_else(fail)?;
        let upstream_account = lease.account().upstream_account_id().ok_or_else(fail)?;
        let envelope = json!({
            "binding": {"key_id": context.client_api_key_ref().as_str(), "account_id": lease.account_id().as_str(), "revision": revision, "excel_enabled": true},
            "credential": {"account_id": lease.account_id().as_str(), "chatgpt_account_id": upstream_account,
                "access_token": secret.access_token.expose_secret(), "user_id": lease.account().upstream_user_id(),
                "proxy": lease.account().outbound_proxy().map(|proxy| proxy.expose_url())},
            "body": body,
        });
        let encoded = serde_json::to_vec(&envelope).map_err(|_| fail())?;
        if encoded.len() > MAX_WORKER_BODY {
            return Err(fail());
        }
        let transport = UpstreamTransport::new(HTTP_SSE_TRANSPORT).map_err(|_| fail())?;
        let metadata = ProviderCallMetadata::new(
            ProviderKind::new(PROVIDER_NAME).map_err(|_| fail())?,
            UpstreamModelId::new(&model).map_err(|_| fail())?,
            lease.account_id().clone(),
            transport.clone(),
        )
        .with_selection_observation(ProviderSelectionObservation::new(
            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            lease.capacity_snapshot(),
        ));
        let affinity = session_affinity.map(|affinity| ExcelAffinity {
            selector: Arc::clone(&self.selector),
            lease: Arc::clone(&lease),
            key: affinity.into_key(),
        });
        let events = excel_response_stream(
            client, endpoint, encoded, model, context, transport, affinity,
        );
        // Excel 拒绝或配额不应把同账号的 Native 凭据标成失效。
        Ok(ProviderStream::new(metadata, events, lease))
    }
}

struct ExcelAffinity {
    selector: Arc<CodexCredentialSelector>,
    lease: Arc<CodexCredentialLease>,
    key: ProviderSessionAffinityKey,
}

fn excel_response_stream(
    client: Client,
    endpoint: &'static str,
    body: Vec<u8>,
    model: String,
    context: AttemptContext,
    transport: UpstreamTransport,
    affinity: Option<ExcelAffinity>,
) -> EventStream {
    Box::pin(async_stream::try_stream! {
        let timeout = remaining(context.deadline()).ok_or_else(|| provider_error(ProviderErrorKind::Timeout, UpstreamSendState::NotSent))?;
        let response = tokio::select! {
            biased;
            () = context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::NotSent)),
            result = client.post(endpoint).header("content-type", "application/json").body(body).timeout(timeout).send() =>
                result.map_err(|_| provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::Ambiguous)),
        }?;
        let status = response.status().as_u16();
        if status < 400 && let Some(affinity) = affinity.as_ref() {
            affinity.selector.update_session_affinity(&affinity.key, affinity.lease.affinity_expected_account_id(), affinity.lease.account_id()).await;
        }
        let content_type = response.headers().get("content-type").map(|value| value.as_bytes().to_vec());
        let is_sse = content_type.as_ref().is_some_and(|value| value.starts_with(b"text/event-stream"));
        let headers: Vec<_> = crate::transport::response_meta::client_headers(response.headers()).into_iter().map(|(name,value)| ProviderResponseHeader::new(name,value)).collect();
        yield ProviderEvent::observation(ProviderResponseObservation::new(transport).with_status_code(status).with_client_headers(headers.clone()));
        if status >= 400 || !is_sse {
            let mut collected = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = tokio::select! {
                biased;
                () = context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::Sent)),
                item = stream.next() => Ok(item),
            }? {
                let chunk = chunk.map_err(|_| provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::Sent))?;
                if collected.len().saturating_add(chunk.len()) > MAX_WORKER_BODY { Err(provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?; }
                collected.extend_from_slice(&chunk);
            }
            let raw = Bytes::from(collected);
            if status >= 400 {
                Err(provider_error(ProviderErrorKind::InvalidRequest, UpstreamSendState::Sent).with_status(status)
                    .with_client_visible_upstream_response(ClientVisibleUpstreamResponse::new(status, content_type, raw).with_headers(headers)))?;
            } else {
                let meta = json_response_meta(&raw, context.request_id().as_str(), &model);
                yield ProviderEvent::canonical(GatewayEvent::Started(meta.clone()));
                if let Some(usage) = json_response_usage(&raw) {
                    yield ProviderEvent::canonical(GatewayEvent::Usage(usage));
                }
                yield ProviderEvent::wire(ProtocolWireEvent::raw_json(PROVIDER_NAME, raw).map_err(|_| provider_error(ProviderErrorKind::Protocol, UpstreamSendState::Sent))?);
                yield ProviderEvent::canonical(GatewayEvent::Completed(meta.with_finish_reason(FinishReason::Stop)));
            }
        } else {
            let mut decoder = CodexCanonicalDecoder::new(model).with_raw_sse_passthrough().with_exact_wire(true);
            let mut stream = response.bytes_stream();
            loop {
                let next = tokio::select! {
                    biased;
                    () = context.cancellation().cancelled() => Err(provider_error(ProviderErrorKind::Cancelled, UpstreamSendState::Sent)),
                    item = stream.next() => Ok(item),
                }?;
                let ended = next.is_none();
                let outcome = if let Some(chunk) = next {
                    decoder.push(&chunk.map_err(|_| provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::Sent))?)
                } else { decoder.finish() };
                match outcome {
                    CodexCanonicalOutcome::Events(events) => { for event in events { yield event; } }
                    CodexCanonicalOutcome::Failed(failure) => {
                        let (events,error,_) = failure.into_parts();
                        for event in events { yield event; }
                        Err(match error { CodexCanonicalError::Protocol(error) => error, CodexCanonicalError::Upstream(_) => provider_error(ProviderErrorKind::Unavailable, UpstreamSendState::Sent) })?;
                    }
                }
                if ended { break; }
            }
        }
    })
}
