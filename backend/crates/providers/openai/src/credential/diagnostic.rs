//! 管理诊断的凭据边界：插件只得到观察结果和随机引用，材料留在 Provider 内存。
use super::{CodexCookiePolicy, CodexCredentialRepository};
use crate::transport::{
    CodexBackendClient, CodexRequestContext, endpoints::endpoint_url,
    protocol::responses::CodexResponsesRequest,
};
use chrono::Utc;
use gateway_admin::{
    model::provider_credentials::ProviderDocument,
    ports::provider::{ProviderAdminError, ProviderAdminErrorKind},
};
use gateway_core::account::{OpaqueProviderData, ProviderAccount};
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    phase: String,
    model: String,
    #[serde(default)]
    prompt: String,
    #[serde(default = "default_effort")]
    effort: String,
    material_ref: Option<String>,
}
fn default_effort() -> String {
    "low".into()
}
struct Pair {
    account: ProviderAccount,
    reference: String,
    state: SecretString,
    cookies: SecretString,
    expires: Instant,
    session: String,
}

pub(crate) struct DiagnosticService {
    repository: CodexCredentialRepository,
    client: CodexBackendClient,
    materials: Mutex<HashMap<String, Pair>>,
}

impl DiagnosticService {
    pub(crate) fn new(repository: CodexCredentialRepository, client: CodexBackendClient) -> Self {
        Self {
            repository,
            client,
            materials: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn run(
        &self,
        account: ProviderAccount,
        document: ProviderDocument,
    ) -> Result<ProviderDocument, ProviderAdminError> {
        let invalid = || ProviderAdminError::new(ProviderAdminErrorKind::Invalid);
        let input: Input = serde_json::from_value(serde_json::Value::Object(
            document.into_provider_data().into_inner(),
        ))
        .map_err(|_| invalid())?;
        if !matches!(input.phase.as_str(), "prepare" | "probe" | "text")
            || input.model.is_empty()
            || input.model.len() > 128
            || input.prompt.len() > 32768
            || !matches!(
                input.effort.as_str(),
                "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
            )
        {
            return Err(invalid());
        }
        // 管理诊断只允许一项在途任务；无需创建常驻账号任务或无界等待队列。
        let mut materials = self
            .materials
            .try_lock()
            .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::Conflict))?;
        materials.retain(|_, pair| pair.expires > Instant::now());
        let key = account.id().as_str().to_owned();
        let valid = materials
            .get(&key)
            .is_some_and(|pair| pair.account == account);
        if !valid {
            materials.remove(&key);
        }
        if input.phase == "prepare"
            && let Some(pair) = materials.get(&key)
        {
            return Ok(output(
                json!({"phase":"prepare","materialRef":pair.reference,"prepared":true,"bootstrap":false}),
            ));
        }
        if input.phase == "probe"
            && !materials
                .get(&key)
                .is_some_and(|pair| Some(&pair.reference) == input.material_ref.as_ref())
        {
            return Err(invalid());
        }
        let credential = self
            .repository
            .load_runtime_credential(&account)
            .await
            .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::Unavailable))?;
        if credential.authentication.oauth().is_none() {
            return Err(ProviderAdminError::new(ProviderAdminErrorKind::Unsupported));
        }
        let authorization = credential
            .authentication
            .authorization_header()
            .map_err(|_| invalid())?;
        // 必须使用账号出口，任何代理构造错误均终止，不能回退直连。
        let client = self
            .client
            .for_account(&account)
            .map_err(|_| ProviderAdminError::new(ProviderAdminErrorKind::Unavailable))?;
        let target = url::Url::parse(&endpoint_url(
            client.base_url_for_diagnostic(),
            crate::transport::CODEX_RESPONSES_PATH,
        ))
        .map_err(|_| invalid())?;
        let policy = CodexCookiePolicy::official().map_err(|_| invalid())?;
        let mut material_expiry = materials
            .get(&key)
            .map_or(Instant::now() + Duration::from_secs(600), |pair| {
                pair.expires
            });
        for cookie in &credential.cookies {
            if let Some(expiry) = cookie.expires_at.filter(|expiry| *expiry > Utc::now())
                && let Ok(remaining) = (expiry - Utc::now()).to_std()
            {
                material_expiry = material_expiry.min(Instant::now() + remaining);
            }
        }
        let initial_cookies = credential
            .cookies
            .iter()
            .filter(|cookie| {
                cookie.expires_at.is_none_or(|expiry| expiry > Utc::now())
                    && policy.may_replay(
                        &target,
                        &cookie.domain,
                        &cookie.path,
                        cookie.host_only,
                        cookie.secure,
                    )
            })
            .map(|cookie| format!("{}={}", cookie.name, cookie.value.expose_secret()))
            .collect::<Vec<_>>()
            .join("; ");
        let session = materials
            .get(&key)
            .map(|pair| pair.session.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let request_id = Uuid::new_v4().to_string();
        let mut context = CodexRequestContext::auxiliary(
            authorization.expose_secret(),
            account.upstream_account_id(),
            &request_id,
            Some(&credential.installation_id),
        );
        context.session_id = Some(&session);
        let pair = materials.get(&key).filter(|_| input.phase == "probe");
        context.turn_state = pair.map(|p| p.state.expose_secret());
        context.cookie_header = pair
            .map(|p| p.cookies.expose_secret())
            .or_else(|| (!initial_cookies.is_empty()).then_some(initial_cookies.as_str()));
        let sent_material = pair.map(|p| p.reference.clone());
        let sent_cookies = context.cookie_header.unwrap_or_default().to_owned();
        let body = json!({"model":input.model,"input":[{"role":"user","content":[{"type":"input_text","text": if input.phase == "text" { input.prompt.as_str() } else { "hi" }}]}],"instructions":"Do not use external tools.","tools":[],"stream":true,"store":false,"reasoning":{"effort":input.effort}});
        let request =
            CodexResponsesRequest::from_body(body.as_object().cloned().ok_or_else(invalid)?);
        let start = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(if input.phase == "text" { 60 } else { 15 }),
            client.diagnostic_observation(&request, context, input.phase != "text"),
        )
        .await;
        let elapsed = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        let Ok(Ok(result)) = result else {
            return Ok(output(
                json!({"phase":input.phase,"status":null,"stateReturned":false,"durationMs":elapsed,"failed":true,"reason":"timeout_or_transport","at":Utc::now().to_rfc3339()}),
            ));
        };
        let mut next_reference = None;
        if input.phase != "text"
            && result.status == 200
            && let Some(state) = result.state.as_ref().filter(|state| state.len() <= 16384)
        {
            let mut cookies: HashMap<String, String> = sent_cookies
                .split("; ")
                .filter_map(|item| {
                    item.split_once('=')
                        .map(|(name, value)| (name.to_owned(), value.to_owned()))
                })
                .collect();
            for cookie in policy
                .parse_response_headers(
                    &key,
                    account.revision().get(),
                    &target,
                    &result.cookies,
                    Utc::now(),
                )
                .inputs
            {
                if cookie.delete {
                    cookies.remove(&cookie.name);
                } else if policy.may_replay(
                    &target,
                    cookie
                        .domain_attribute
                        .as_deref()
                        .unwrap_or(target.host_str().unwrap_or_default())
                        .trim_start_matches('.'),
                    &cookie.path,
                    cookie.domain_attribute.is_none(),
                    cookie.secure,
                ) {
                    if let Some(expiry) = cookie.expires_at
                        && let Ok(remaining) = (expiry - Utc::now()).to_std()
                    {
                        material_expiry = material_expiry.min(Instant::now() + remaining);
                    }
                    cookies.insert(cookie.name, cookie.value.expose_secret().to_owned());
                }
            }
            if !cookies.is_empty() && (materials.len() < 64 || materials.contains_key(&key)) {
                let reference = Uuid::new_v4().to_string();
                next_reference = Some(reference.clone());
                materials.insert(
                    key,
                    Pair {
                        account,
                        reference,
                        state: SecretString::from(state.clone()),
                        cookies: SecretString::from(
                            cookies
                                .into_iter()
                                .map(|(name, value)| format!("{name}={value}"))
                                .collect::<Vec<_>>()
                                .join("; "),
                        ),
                        expires: material_expiry,
                        session,
                    },
                );
            }
        }
        Ok(output(
            json!({"phase":input.phase,"status":result.status,"stateReturned":result.state.is_some(),"durationMs":elapsed,"at":Utc::now().to_rfc3339(),"sentMaterialRef":sent_material,"materialRef":next_reference,"prepared":next_reference.is_some(),"bootstrap":input.phase=="prepare","text":result.text,"failed":result.status!=200}),
        ))
    }
}

fn output(value: serde_json::Value) -> ProviderDocument {
    ProviderDocument::new(OpaqueProviderData::new(
        value.as_object().cloned().unwrap_or_default(),
    ))
}
