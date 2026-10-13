use super::*;
use gateway_core::account::scope::{AccountGroupId, RoutingGroupSnapshot};
use std::sync::atomic::{AtomicBool, Ordering};

const ACCOUNT: &str = "acct_gateway_pool";
const GROUP: &str = "grp_00000000000000000000000000000001";

#[cfg(feature = "gateway-pool-experiment")]
#[tokio::test]
#[ignore = "requires explicit authorization and private experiment input"]
async fn authorized_real_gateway_cache_experiment() {
    use provider_openai::credential::{
        CodexAccountProfile, CodexCredentialAdmin, CodexCredentialData, CodexOAuthSecret,
    };
    use std::io::Write;
    let root = std::path::PathBuf::from(
        std::env::var("GATEWAY_POOL_EXPERIMENT_ROOT").expect("private experiment root"),
    );
    let input: Value =
        serde_json::from_slice(&std::fs::read(root.join("input.json")).unwrap()).unwrap();
    let mut body: Value =
        serde_json::from_slice(&std::fs::read(root.join("request.json")).unwrap()).unwrap();
    let session = input["sessionId"].as_str().unwrap();
    assert_eq!(body["prompt_cache_key"], input["cacheKey"]);
    let account_id = input["accountId"].as_str().unwrap();
    let credential: CodexCredentialData =
        serde_json::from_value(input["credential"].clone()).unwrap();
    let oauth = credential.oauth().unwrap();
    assert!(oauth.refresh_token.is_none());
    let mut prepared = CodexCredentialAdmin
        .prepare_import(ImportCodexOAuthCredential {
            account_id: account_id.into(),
            name: "authorized cache experiment".into(),
            secret: CodexOAuthSecret {
                access_token: SecretString::from(oauth.access_token.clone()),
                refresh_token: None,
                id_token: None,
            },
            verified_account: CodexAccountProfile {
                email: None,
                oauth_subject: input["subject"].as_str().unwrap().into(),
                poid: None,
                chatgpt_account_id: input["upstreamAccountId"].as_str().unwrap().into(),
                chatgpt_user_id: input["upstreamUserId"].as_str().unwrap().into(),
                plan_type: Some("plus".into()),
                access_token_expires_at: Some(
                    DateTime::parse_from_rfc3339(input["expiresAt"].as_str().unwrap())
                        .unwrap()
                        .with_timezone(&Utc),
                ),
            },
            next_refresh_at: None,
            enabled: true,
        })
        .unwrap();
    prepared.credential = CodexCredentialCodec::encode_complete(credential).unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    store.create_account(prepared).await.unwrap();
    let proxy =
        gateway_core::account::OutboundProxy::parse(input["outboundProxyUrl"].as_str().unwrap())
            .expect("configured account egress");
    store.set_egress(account_id, Some(proxy), None);
    let lanes: Vec<String> = input["lanes"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_else(|| vec!["a".into(), "b".into()]);
    assert!((2..=6).contains(&lanes.len()));
    assert!(
        lanes
            .iter()
            .enumerate()
            .all(|(index, lane)| lane == &char::from(b'a' + index as u8).to_string())
    );
    let max_seconds = input["maxSeconds"].as_u64().unwrap_or(300);
    assert!((300..=600).contains(&max_seconds));
    let append = input["conversationMode"] == "append";
    let questions = input["questions"].as_array();
    if append {
        assert_eq!(questions.unwrap().len(), lanes.len() * 3);
    }
    let mut runners = Vec::new();
    for lane in &lanes {
        let mut config = valid_config();
        config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
            runtime_url: format!("{}/{lane}", input["runtimeUrl"].as_str().unwrap()),
            key_file: root.join("runtime.key"),
            fallback_url: None,
        });
        let bundle = provider_openai::initialize(
            config.config.clone(),
            provider_ports_with(store.clone(), Arc::new(TestOAuthPending::default())),
        )
        .await
        .unwrap();
        let runner = bundle.gateway_pool_experiment().unwrap();
        if input["directPoolMetadata"] == true {
            runner.allow_direct_pool_metadata();
        }
        runners.push(runner);
    }
    let id = ProviderAccountId::new(account_id).unwrap();
    let started = std::time::Instant::now();
    let stop = CancellationToken::new();
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("samples.jsonl"))
        .unwrap();
    let mut record = |data: Value| {
        let item = json!({"at":Utc::now().to_rfc3339(),"elapsedSeconds":started.elapsed().as_secs_f64(),"data":data});
        writeln!(log, "{item}").unwrap();
        log.flush().unwrap();
        log.sync_data().unwrap();
        std::fs::write(root.join("status.tmp"), item.to_string()).unwrap();
        std::fs::rename(root.join("status.tmp"), root.join("status.json")).unwrap();
    };
    record(
        json!({"result":"starting","mode":"cache","conversationMode":if append {"append"} else {"fixed"},"maxSeconds":max_seconds,"gatewayTarget":lanes.len(),"headersOnly":false,"fixedSession":true,"fixedCacheKey":true}),
    );
    if !runners[0].refresh_quota(&id).await {
        record(json!({"result":"stopped","reason":"quota_refresh_failed_or_exhausted"}));
        return;
    }
    let mut expected_gateways = vec![String::new(); lanes.len()];
    let mut positive = false;
    let mut reason = "completed";
    // 每个新网关连续两次；首次网关增加正对照，最后两个网关回切验证。
    let mut schedule = vec![
        (0, "A1".to_owned()),
        (0, "A2".to_owned()),
        (0, "A3".to_owned()),
    ];
    for (index, lane) in lanes.iter().enumerate().skip(1) {
        schedule.extend([
            (index, format!("{}1", lane.to_uppercase())),
            (index, format!("{}2", lane.to_uppercase())),
        ]);
    }
    let penultimate = lanes.len() - 2;
    schedule.push((
        penultimate,
        format!(
            "{}{}",
            lanes[penultimate].to_uppercase(),
            if penultimate == 0 { 4 } else { 3 }
        ),
    ));
    schedule.push((
        lanes.len() - 1,
        format!("{}3", lanes.last().unwrap().to_uppercase()),
    ));
    if append {
        schedule = lanes
            .iter()
            .enumerate()
            .flat_map(|(index, lane)| {
                (1..=3).map(move |turn| (index, format!("{}{turn}", lane.to_uppercase())))
            })
            .collect();
    }
    for (turn, (lane, phase)) in schedule.into_iter().enumerate() {
        if root.join("STOP").exists() {
            reason = "manual_stop";
            break;
        }
        if append {
            body["input"].as_array_mut().unwrap().push(json!({"role":"user","content":[{"type":"input_text","text":questions.unwrap()[turn].as_str().unwrap()}]}));
        }
        use sha2::{Digest, Sha256};
        let request_bytes = serde_json::to_vec(&body).unwrap();
        let request_sha = hex::encode(Sha256::digest(&request_bytes));
        let message_count = body["input"].as_array().unwrap().len();
        // 保存每轮合成对话，用于核对完整历史确实仅追加；不包含头部或路由材料。
        if append {
            std::fs::write(root.join(format!("request-{phase}.json")), &request_bytes).unwrap();
        }
        record(
            json!({"result":"requesting","phase":phase,"messageCount":message_count,"requestBytes":request_bytes.len(),"requestSha256":request_sha}),
        );
        let mut result = tokio::time::timeout(
            Duration::from_secs(100),
            runners[lane].cache_sample(
                &id,
                body.as_object().unwrap().clone(),
                session,
                SystemTime::now() + Duration::from_secs(60),
                &stop,
            ),
        )
        .await
        .unwrap_or_else(|_| json!({"result":"deadline"}));
        result["phase"] = json!(phase);
        result["messageCount"] = json!(message_count);
        result["requestBytes"] = json!(request_bytes.len());
        result["requestSha256"] = json!(request_sha);
        record(result.clone());
        if result["classification"] != "21"
            || result["usage"].is_null()
            || result["inputTokens"].as_u64().unwrap_or(0) < 1024
        {
            reason = "invalid_sample_or_missing_usage";
            break;
        }
        let gateway = result["gateway"].as_str().unwrap();
        let expected = &mut expected_gateways[lane];
        if expected.is_empty() {
            *expected = gateway.into();
        } else if expected != gateway {
            reason = "unexpected_gateway_change";
            break;
        }
        if expected_gateways
            .iter()
            .enumerate()
            .any(|(index, other)| index != lane && other == gateway)
        {
            reason = "gateway_not_changed";
            break;
        }
        if phase == "A2" || phase == "A3" {
            positive |= result["cachedTokens"].as_u64().unwrap_or(0) > 0;
        }
        if phase == "A3" && !positive && !append {
            reason = "no_same_gateway_positive_control";
            break;
        }
        if append {
            let Some(text) = result["assistantText"]
                .as_str()
                .filter(|text| !text.is_empty())
            else {
                reason = "missing_assistant_reply";
                break;
            };
            body["input"]
                .as_array_mut()
                .unwrap()
                .push(json!({"role":"assistant","content":[{"type":"output_text","text":text}]}));
        }
        if started.elapsed() > Duration::from_secs(max_seconds - 30) {
            reason = "experiment_time_limit";
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    stop.cancel();
    record(
        json!({"result":"stopped","reason":reason,"positiveControl":positive,"gateways":expected_gateways}),
    );
}

/// 授权实验共用：读取私有输入，只导入 Access Token，按输入设置账号出口（直连账号不设）。
#[cfg(feature = "gateway-pool-experiment")]
async fn experiment_account(
    root: &std::path::Path,
) -> (Value, ProviderAccountId, Arc<MemoryAccountStore>) {
    use provider_openai::credential::{
        CodexAccountProfile, CodexCredentialAdmin, CodexCredentialData, CodexOAuthSecret,
    };
    let input: Value =
        serde_json::from_slice(&std::fs::read(root.join("input.json")).unwrap()).unwrap();
    let account_id = input["accountId"].as_str().unwrap();
    let credential: CodexCredentialData =
        serde_json::from_value(input["credential"].clone()).unwrap();
    let oauth = credential.oauth().unwrap();
    assert!(oauth.refresh_token.is_none());
    let mut prepared = CodexCredentialAdmin
        .prepare_import(ImportCodexOAuthCredential {
            account_id: account_id.into(),
            name: "authorized gateway pool experiment".into(),
            secret: CodexOAuthSecret {
                access_token: SecretString::from(oauth.access_token.clone()),
                refresh_token: None,
                id_token: None,
            },
            verified_account: CodexAccountProfile {
                email: None,
                oauth_subject: input["subject"].as_str().unwrap().into(),
                poid: None,
                chatgpt_account_id: input["upstreamAccountId"].as_str().unwrap().into(),
                chatgpt_user_id: input["upstreamUserId"].as_str().unwrap().into(),
                plan_type: Some("plus".into()),
                access_token_expires_at: Some(
                    DateTime::parse_from_rfc3339(input["expiresAt"].as_str().unwrap())
                        .unwrap()
                        .with_timezone(&Utc),
                ),
            },
            next_refresh_at: None,
            enabled: true,
        })
        .unwrap();
    prepared.credential = CodexCredentialCodec::encode_complete(credential).unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    store.create_account(prepared).await.unwrap();
    if let Some(url) = input["outboundProxyUrl"].as_str() {
        let proxy =
            gateway_core::account::OutboundProxy::parse(url).expect("configured account egress");
        store.set_egress(account_id, Some(proxy), None);
    }
    let id = ProviderAccountId::new(account_id).unwrap();
    (input, id, store)
}

/// 每条通道一个独立的网关池运行时入口，共用同一账号存储。
#[cfg(feature = "gateway-pool-experiment")]
async fn experiment_runner(
    root: &std::path::Path,
    input: &Value,
    store: &Arc<MemoryAccountStore>,
    lane: &str,
) -> provider_openai::gateway_pool::GatewayPoolExperiment {
    let mut config = valid_config();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        runtime_url: format!("{}/{lane}", input["runtimeUrl"].as_str().unwrap()),
        key_file: root.join("runtime.key"),
        fallback_url: None,
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store.clone(), Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let runner = bundle.gateway_pool_experiment().unwrap();
    if input["directPoolMetadata"] == true {
        runner.allow_direct_pool_metadata();
    }
    runner
}

/// 同一张票（同一 S0）能否同时保留两份缓存：X、Y 两段不同前缀，各自固定
/// session 与 prompt_cache_key，交替请求 X1→Y1→X2→Y2→X3→Y3，读完整 SSE usage。
#[cfg(feature = "gateway-pool-experiment")]
#[tokio::test]
#[ignore = "requires explicit authorization and private experiment input"]
async fn authorized_real_dual_cache_experiment() {
    use std::io::Write;
    let root = std::path::PathBuf::from(
        std::env::var("GATEWAY_POOL_EXPERIMENT_ROOT").expect("private experiment root"),
    );
    let (input, id, store) = experiment_account(&root).await;
    let bodies: Vec<Value> = ["x", "y"]
        .iter()
        .map(|name| {
            serde_json::from_slice(
                &std::fs::read(root.join(format!("request-{name}.json"))).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let sessions = [
        input["sessionX"].as_str().unwrap().to_owned(),
        input["sessionY"].as_str().unwrap().to_owned(),
    ];
    assert_ne!(bodies[0]["prompt_cache_key"], bodies[1]["prompt_cache_key"]);
    assert_ne!(sessions[0], sessions[1]);
    let runner = experiment_runner(&root, &input, &store, "a").await;
    let started = std::time::Instant::now();
    let stop = CancellationToken::new();
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("samples.jsonl"))
        .unwrap();
    let mut record = |data: Value| {
        let item = json!({"at":Utc::now().to_rfc3339(),"elapsedSeconds":started.elapsed().as_secs_f64(),"data":data});
        writeln!(log, "{item}").unwrap();
        log.flush().unwrap();
        log.sync_data().unwrap();
        std::fs::write(root.join("status.tmp"), item.to_string()).unwrap();
        std::fs::rename(root.join("status.tmp"), root.join("status.json")).unwrap();
    };
    record(
        json!({"result":"starting","mode":"dual-cache","schedule":["X1","Y1","X2","Y2","X3","Y3"]}),
    );
    if !runner.refresh_quota(&id).await {
        record(json!({"result":"stopped","reason":"quota_refresh_failed_or_exhausted"}));
        return;
    }
    let mut reason = "completed";
    let mut ticket: Option<Value> = None;
    for (turn, phase) in ["X1", "Y1", "X2", "Y2", "X3", "Y3"].iter().enumerate() {
        let which = turn % 2;
        let mut result = tokio::time::timeout(
            Duration::from_secs(170),
            runner.cache_sample(
                &id,
                bodies[which].as_object().unwrap().clone(),
                &sessions[which],
                SystemTime::now() + Duration::from_secs(150),
                &stop,
            ),
        )
        .await
        .unwrap_or_else(|_| json!({"result":"deadline"}));
        result["phase"] = json!(phase);
        // 合成回答与原始 usage 不需要保留，只留命中数字。
        result["assistantText"] = Value::Null;
        let current = json!([result["gateway"], result["ticketFingerprint"]]);
        let changed = ticket.as_ref().is_some_and(|ticket| ticket != &current);
        ticket.get_or_insert(current);
        result["ticketChanged"] = json!(changed);
        record(result.clone());
        if result["classification"] != "21" || changed {
            reason = if changed {
                "ticket_changed"
            } else {
                "invalid_sample"
            };
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    stop.cancel();
    record(json!({"result":"stopped","reason":reason}));
}

/// 跨模型复用票：每个网关一条通道，打票后在同一张票上按计划依次发送各模型的 `hi`，
/// 只看响应头。计划中 `classify` 为 false 的步骤只记录事实，不影响组合判定。
#[cfg(feature = "gateway-pool-experiment")]
#[tokio::test]
#[ignore = "requires explicit authorization and private experiment input"]
async fn authorized_real_cross_model_experiment() {
    use std::io::Write;
    let root = std::path::PathBuf::from(
        std::env::var("GATEWAY_POOL_EXPERIMENT_ROOT").expect("private experiment root"),
    );
    let (input, id, store) = experiment_account(&root).await;
    let lanes: Vec<String> = input["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    assert!((1..=6).contains(&lanes.len()));
    let plan: Vec<(String, bool)> = input["plan"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["model"].as_str().unwrap().to_owned(),
                step["classify"].as_bool().unwrap(),
            )
        })
        .collect();
    assert!((1..=6).contains(&plan.len()));
    let max_seconds = input["maxSeconds"].as_u64().unwrap_or(300);
    assert!((60..=600).contains(&max_seconds));
    let mut runners = Vec::new();
    for lane in &lanes {
        runners.push(experiment_runner(&root, &input, &store, lane).await);
    }
    let started = std::time::Instant::now();
    let stop = CancellationToken::new();
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("samples.jsonl"))
        .unwrap();
    let mut record = |data: Value| {
        let item = json!({"at":Utc::now().to_rfc3339(),"elapsedSeconds":started.elapsed().as_secs_f64(),"data":data});
        writeln!(log, "{item}").unwrap();
        log.flush().unwrap();
        log.sync_data().unwrap();
        std::fs::write(root.join("status.tmp"), item.to_string()).unwrap();
        std::fs::rename(root.join("status.tmp"), root.join("status.json")).unwrap();
    };
    record(
        json!({"result":"starting","mode":"cross-model","ticketModel":input["ticketModel"],"plan":input["plan"],"gatewayTarget":lanes.len(),"maxSeconds":max_seconds,"headersOnly":true}),
    );
    if !runners[0].refresh_quota(&id).await {
        record(json!({"result":"stopped","reason":"quota_refresh_failed_or_exhausted"}));
        return;
    }
    let mut reason = "completed";
    'lanes: for (lane, runner) in lanes.iter().zip(&runners) {
        let lane_started = std::time::Instant::now();
        let mut gateway: Option<String> = None;
        for (step, (model, classify)) in plan.iter().enumerate() {
            if root.join("STOP").exists() {
                reason = "manual_stop";
                break 'lanes;
            }
            if started.elapsed() > Duration::from_secs(max_seconds - 20) {
                reason = "experiment_time_limit";
                break 'lanes;
            }
            let mut result = tokio::time::timeout(
                Duration::from_secs(170),
                runner.model_sample(
                    &id,
                    model,
                    *classify,
                    SystemTime::now() + Duration::from_secs(150),
                    &stop,
                ),
            )
            .await
            .unwrap_or_else(|_| json!({"result":"deadline"}));
            result["lane"] = json!(lane);
            result["step"] = json!(step + 1);
            result["laneElapsedMs"] = json!(lane_started.elapsed().as_millis());
            let current = result["gateway"].as_str().map(str::to_owned);
            if step == 0 {
                gateway = current.clone();
            } else if current.is_some() && current != gateway {
                // 同一通道换了网关说明票已退役，后续步骤不再是同一张票。
                result["ticketChanged"] = json!(true);
            }
            record(result.clone());
            if result["result"] != "headers" || result["ticketChanged"] == true {
                continue 'lanes;
            }
        }
    }
    stop.cancel();
    record(json!({"result":"stopped","reason":reason}));
}

/// 必须手动提供私有输入目录；CI 不读取凭据，也不会运行真实上游实验。
#[cfg(feature = "gateway-pool-experiment")]
#[tokio::test]
#[ignore = "requires explicit authorization and private experiment input"]
async fn authorized_real_header_only_experiment() {
    use provider_openai::credential::{
        CodexAccountProfile, CodexCredentialAdmin, CodexCredentialData, CodexOAuthSecret,
    };
    use std::io::Write;
    let root = std::path::PathBuf::from(
        std::env::var("GATEWAY_POOL_EXPERIMENT_ROOT").expect("private experiment root"),
    );
    let input: Value =
        serde_json::from_slice(&std::fs::read(root.join("input.json")).unwrap()).unwrap();
    let account_id = input["accountId"].as_str().unwrap();
    let credential: CodexCredentialData =
        serde_json::from_value(input["credential"].clone()).unwrap();
    let oauth = credential.oauth().unwrap();
    assert!(
        oauth.refresh_token.is_none(),
        "experiment must not rotate production RT"
    );
    let mut prepared = CodexCredentialAdmin
        .prepare_import(ImportCodexOAuthCredential {
            account_id: account_id.into(),
            name: "yxx experiment".into(),
            secret: CodexOAuthSecret {
                access_token: SecretString::from(oauth.access_token.clone()),
                refresh_token: None,
                id_token: None,
            },
            verified_account: CodexAccountProfile {
                email: None,
                oauth_subject: input["subject"].as_str().unwrap().into(),
                poid: None,
                chatgpt_account_id: input["upstreamAccountId"].as_str().unwrap().into(),
                chatgpt_user_id: input["upstreamUserId"].as_str().unwrap().into(),
                plan_type: Some("plus".into()),
                access_token_expires_at: Some(
                    DateTime::parse_from_rfc3339(input["expiresAt"].as_str().unwrap())
                        .unwrap()
                        .with_timezone(&Utc),
                ),
            },
            next_refresh_at: None,
            enabled: true,
        })
        .unwrap();
    prepared.credential = CodexCredentialCodec::encode_complete(credential).unwrap();
    let store = Arc::new(MemoryAccountStore::default());
    store.create_account(prepared).await.unwrap();
    let mut config = valid_config();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        runtime_url: input["runtimeUrl"].as_str().unwrap().into(),
        key_file: root.join("runtime.key"),
        fallback_url: None,
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let runner = bundle.gateway_pool_experiment().unwrap();
    let id = ProviderAccountId::new(account_id).unwrap();
    let started = std::time::Instant::now();
    let stop = CancellationToken::new();
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(root.join("samples.jsonl"))
        .unwrap();
    let mut record = |data: Value| {
        let item = json!({"at":Utc::now().to_rfc3339(),"elapsedSeconds":started.elapsed().as_secs_f64(),"data":data});
        writeln!(log, "{item}").unwrap();
        log.flush().unwrap();
        log.sync_data().unwrap();
        std::fs::write(root.join("status.tmp"), item.to_string()).unwrap();
        std::fs::rename(root.join("status.tmp"), root.join("status.json")).unwrap();
    };
    record(json!({"result":"starting","maxSeconds":7200,"intervalSeconds":10,"headersOnly":true}));
    if !runner.refresh_quota(&id).await {
        record(json!({"result":"stopped","reason":"quota_refresh_failed_or_exhausted"}));
        return;
    }
    let background = runner.clone();
    let token = stop.clone();
    let task = tokio::spawn(async move {
        loop {
            tokio::select! {biased; _=token.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{background.background(&token).await;}}
        }
    });
    let limit = started + Duration::from_secs(7200);
    let mut schedule = tokio::time::interval(Duration::from_secs(10));
    schedule.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let reason = loop {
        if root.join("STOP").exists() {
            break "manual_stop";
        }
        tokio::select! {biased; _=tokio::time::sleep_until(limit.into())=>break "two_hours",_=schedule.tick()=>{}}
        let deadline = SystemTime::now() + Duration::from_secs(90);
        let result = tokio::select! {biased; _=tokio::time::sleep_until(limit.into())=>break "two_hours",result=runner.sample(&id,deadline,&stop)=>result};
        let terminal = match result["result"].as_str() {
            Some("route_unavailable") => Some("no_available_pair_or_runtime_failure"),
            Some("quota_or_account_unavailable") => Some("quota_or_account_unavailable"),
            _ => None,
        };
        record(result);
        if let Some(reason) = terminal {
            break reason;
        }
    };
    stop.cancel();
    let _ = task.await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    record(json!({"result":"stopped","reason":reason}));
}

fn scope() -> Arc<FrozenAccountScope> {
    scope_for(&[ACCOUNT])
}

fn scope_for(ids: &[&str]) -> Arc<FrozenAccountScope> {
    let group = AccountGroupId::new(GROUP).unwrap();
    let provider = ProviderKind::new("openai").unwrap();
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(
            ids.iter()
                .map(|id| {
                    (
                        ProviderAccountId::new(*id).unwrap(),
                        RuntimeAccount::new(provider.clone(), BTreeSet::from([group.clone()])),
                    )
                })
                .collect(),
        )),
        ClientRoutingScope::restricted(
            vec![RoutingGroupSnapshot::new(group.clone(), "Pool".into())],
            BTreeSet::from([group]),
            BTreeSet::from([provider]),
        )
        .unwrap(),
    ))
}

fn request(tools: Value) -> ProviderRequest {
    let body = json!({"model":"gpt-6-astra","input":[{"role":"user","content":[{"type":"input_text","text":"original user input"}]}],"tools":tools,"client_metadata":{"x-codex-turn-state":"old-body-state","custom":"keep"},"unknown_test_field":"preserve","store":true,"stream":true});
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
            .unwrap()
            .with_original_json(bytes::Bytes::from(body.to_string()))
            .with_context(Map::from_iter([
                ("use_websocket".into(), json!(false)),
                ("turn_state".into(), json!("client-state-must-not-win")),
            ])),
    ));
    let provider = ProviderKind::new("openai").unwrap();
    let model = UpstreamModelId::new("gpt-6-astra").unwrap();
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        account_policy(),
        vec![provider.clone()],
        vec![ProviderModel::new(
            provider,
            model.clone(),
            ModelCapabilities::new(BTreeSet::from([operation.kind()]), None)
                .with_upstream_feature_validation(),
        )],
        vec![],
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new(model.as_str()).unwrap(),
            &operation,
            scope(),
            &RoutingContext::default(),
        )
        .unwrap();
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn context() -> AttemptContext {
    context_for(scope())
}

fn context_for(account_scope: Arc<FrozenAccountScope>) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_gateway_pool").unwrap(),
            ClientApiKeyId::new("key_gateway_pool").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(10),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(account_scope),
        None,
        CancellationToken::new(),
    )
}

async fn run(exhausted: bool, tools: Value) -> (Vec<String>, usize) {
    let upstream = MockServer::start().await;
    let runtime = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.into(),
            name: "Demo".into(),
            secret: secret("demo-token"),
            verified_account: profile("demo-upstream"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account = store.account(ACCOUNT).unwrap();
    let now = SystemTime::now();
    store.compare_and_swap_quota(QuotaObservation {plan_type:None,account_id:account.id().clone(),expected_revision:account.revision(),observed_at:now,state:if exhausted {QuotaState::exhausted(QuotaEvidence::ProviderDenied,now,None)} else {QuotaState::allowed(now)},quota:OpaqueProviderData::new(json!({"rate_limit":{"allowed":!exhausted,"limit_reached":exhausted,"primary_window":{"used_percent":if exhausted {100}else{10},"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone())}).await.unwrap();
    let second = Arc::new(AtomicBool::new(false));
    let selected = Arc::clone(&second);
    Mock::given(method("POST")).respond_with(move |input:&wiremock::Request| {
        let data:Value = input.body_json().unwrap();
        let generation = if selected.load(Ordering::SeqCst) {"second"}else{"first"};
        let value = match data["action"].as_str().unwrap() {
            "config" => json!({"enabled":true,"groupIds":[GROUP],"poolAccountIds":{ACCOUNT:"demo"},"probeIntervalMs":50,"probeModel":"gpt-6-astra"}),
            "active" => json!({"accounts":[ACCOUNT],"queue":true}),
            "plan" => json!({"ready":{"gateway":if generation=="first" {"unified-142"}else{"unified-143"},"generation":generation},"probe":null,"preparing":false}),
            "material" => {
                let host = if data["generation"]=="first" {"unified-142"}else{"unified-143"};
                let payload = URL_SAFE_NO_PAD.encode(json!({"host":format!("{host}.example.invalid"),"exp":Utc::now().timestamp()+600}).to_string());
                json!({"state":format!("fixed-s0-{}",data["generation"].as_str().unwrap()),"cookie":format!("__oailb=h.{payload}.s; __cflb=demo"),"expires":Utc::now().timestamp()+600})
            },
            "reserve" => json!({"reserved":true,"leaseId":data["generation"]}),
            "businessResult" => {
                let bad = data["stateReturned"]==true;
                if bad {selected.store(true,Ordering::SeqCst);}
                json!({"accepted":true,"classification":if bad {"29"}else{"21"}})
            },
            _ => json!({"accepted":true}),
        };
        ResponseTemplate::new(200).set_body_json(value)
    }).mount(&runtime).await;
    Mock::given(method("POST")).and(path("/codex/responses")).respond_with(|input:&wiremock::Request| {
        let state = input.headers.get("x-codex-turn-state").unwrap().to_str().unwrap();
        if state=="fixed-s0-first" {
            ResponseTemplate::new(200).insert_header("content-type","text/event-stream").insert_header("x-codex-turn-state","returned-state-that-must-not-replace-s0").set_body_string("data: {\"type\":\"response.output_text.delta\",\"delta\":\"BAD ANSWER MUST NOT BE DELIVERED\"}\n\n")
        } else {
            assert_eq!(state,"fixed-s0-second");
            ResponseTemplate::new(200).insert_header("content-type","text/event-stream").set_body_string("data: {\"type\":\"response.output_text.delta\",\"item_id\":\"item\",\"output_index\":0,\"content_index\":0,\"delta\":\"complete good answer\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_pool\",\"status\":\"completed\",\"output\":[]}}\n\n")
        }
    }).mount(&upstream).await;
    let mut config = valid_config();
    let key = config._runtime.path().join("pool.key");
    std::fs::write(&key, "a".repeat(64)).unwrap();
    config.config.api.base_url = upstream.uri();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        runtime_url: runtime.uri(),
        key_file: key,
        fallback_url: None,
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let result = bundle
        .core_provider()
        .execute(request(tools), context())
        .await;
    let mut events = Vec::new();
    if let Ok(mut stream) = result {
        while let Some(event) = stream.next().await {
            match event {
                Ok(event) => {
                    if let Some(wire) = event.wire_event() {
                        events.push(wire.data().to_string());
                    }
                }
                Err(_) => {
                    events.push("failed".into());
                    break;
                }
            }
        }
    }
    let requests = upstream.received_requests().await.unwrap();
    for input in &requests {
        let body: Value =
            serde_json::from_slice(&zstd::stream::decode_all(input.body.as_slice()).unwrap())
                .unwrap();
        assert_eq!(
            body["input"][0]["content"][0]["text"],
            "original user input"
        );
        assert_eq!(body["client_metadata"]["custom"], "keep");
        assert_eq!(body["unknown_test_field"], "preserve");
        // 规范化 HTTP 请求可以剔除 transport metadata；若保留，必须与固定头一致。
        if let Some(state) = body["client_metadata"].get("x-codex-turn-state") {
            assert_eq!(state, input.headers["x-codex-turn-state"].to_str().unwrap());
        }
        assert!(
            input
                .headers
                .get("cookie")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("__cflb=demo")
        );
    }
    (events, requests.len())
}

#[tokio::test]
async fn pool_29_cancels_before_delivery_and_replays_original_input_on_same_account() {
    let (events, count) = run(false, json!([])).await;
    assert_eq!(count, 2);
    assert!(!events.join(" ").contains("BAD ANSWER"));
    assert!(events.join(" ").contains("complete good answer"));
}

#[tokio::test]
async fn pool_does_not_replay_upstream_tools_with_unknown_side_effects() {
    let (events, count) = run(false, json!([{"type":"web_search"}])).await;
    assert_eq!(count, 1);
    assert_eq!(events, vec!["failed"]);
}

#[tokio::test]
async fn pool_skips_account_with_confirmed_exhausted_quota() {
    let (_, count) = run(true, json!([])).await;
    assert_eq!(count, 0);
}

/// 打票模型为 sol 时，Astra 请求在同一窗口内先用 Astra 发 `hi` 只拿 state，
/// 不做第二轮判定；业务请求带 Astra 自己的票，不使用 sol 的 S0。
#[tokio::test]
async fn pool_fetches_model_ticket_within_shared_window() {
    let upstream = MockServer::start().await;
    let runtime = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.into(),
            name: "Demo".into(),
            secret: secret("demo-token"),
            verified_account: profile("demo-upstream"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account = store.account(ACCOUNT).unwrap();
    let now = SystemTime::now();
    store.compare_and_swap_quota(QuotaObservation {plan_type:None,account_id:account.id().clone(),expected_revision:account.revision(),observed_at:now,state:QuotaState::allowed(now),quota:OpaqueProviderData::new(json!({"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10,"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone())}).await.unwrap();
    let actions = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let seen = Arc::clone(&actions);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({"host":"unified-142.example.invalid","exp":Utc::now().timestamp()+600}).to_string(),
    );
    let cookie = format!("__oailb=h.{payload}.s; __cflb=demo");
    Mock::given(method("POST")).respond_with(move |input:&wiremock::Request| {
        let data:Value = input.body_json().unwrap();
        seen.lock().unwrap().push(data.clone());
        let value = match data["action"].as_str().unwrap() {
            "config" => json!({"enabled":true,"groupIds":[GROUP],"poolAccountIds":{ACCOUNT:"demo"},"probeIntervalMs":50,"probeModel":"gpt-6.1-sol"}),
            "active" => json!({"accounts":[ACCOUNT],"queue":true}),
            "plan" => json!({"ready":{"gateway":"unified-142","generation":"first"},"probe":null,"preparing":false}),
            "material" => json!({"state":"sol-s0","cookie":cookie,"expires":Utc::now().timestamp()+600}),
            "reserve" => json!({"reserved":true,"leaseId":"lease-1"}),
            "ticket" => Value::Null,
            "businessResult" => json!({"accepted":true,"classification":if data["stateReturned"]==true {"29"} else {"21"}}),
            _ => json!({"accepted":true}),
        };
        ResponseTemplate::new(200).set_body_json(value)
    }).mount(&runtime).await;
    Mock::given(method("POST")).and(path("/codex/responses")).respond_with(|input:&wiremock::Request| {
        let state = input.headers.get("x-codex-turn-state").map(|value| value.to_str().unwrap().to_owned());
        assert_ne!(state.as_deref(), Some("sol-s0"), "sol 的 S0 不能用于 Astra");
        match state.as_deref() {
            None => ResponseTemplate::new(200).insert_header("content-type","text/event-stream").insert_header("x-codex-turn-state","astra-ticket").set_body_string("data: {}

"),
            Some("astra-ticket") => ResponseTemplate::new(200).insert_header("content-type","text/event-stream").set_body_string("data: {\"type\":\"response.output_text.delta\",\"item_id\":\"item\",\"output_index\":0,\"content_index\":0,\"delta\":\"complete good answer\"}

data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_pool\",\"status\":\"completed\",\"output\":[]}}

"),
            Some(other) => panic!("unexpected state {other}"),
        }
    }).mount(&upstream).await;
    let mut config = valid_config();
    let key = config._runtime.path().join("pool.key");
    std::fs::write(&key, "a".repeat(64)).unwrap();
    config.config.api.base_url = upstream.uri();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        runtime_url: runtime.uri(),
        key_file: key,
        fallback_url: None,
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let mut stream = bundle
        .core_provider()
        .execute(request(json!([])), context())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        if let Some(wire) = event.unwrap().wire_event() {
            events.push(wire.data().to_string());
        }
    }
    assert!(events.join(" ").contains("complete good answer"));
    let requests = upstream.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let decode = |body: &[u8]| -> Value {
        serde_json::from_slice(&zstd::stream::decode_all(body).unwrap_or_else(|_| body.to_vec()))
            .unwrap()
    };
    let ticket_request = decode(&requests[0].body);
    assert_eq!(ticket_request["model"], "gpt-6-astra");
    assert_eq!(ticket_request["input"][0]["content"][0]["text"], "hi");
    assert_eq!(requests[1].headers["x-codex-turn-state"], "astra-ticket");
    drop(stream);
    // Guard 的回报由析构异步提交；等待回报后核对收头与流终态是独立事实。
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if actions
                .lock()
                .unwrap()
                .iter()
                .any(|a| a["action"] == "finish")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let actions = actions.lock().unwrap();
    let ticket = actions
        .iter()
        .find(|a| a["action"] == "ticketResult")
        .expect("ticket recorded");
    assert_eq!(ticket["model"], "gpt-6-astra");
    assert_eq!(ticket["material"]["state"], "astra-ticket");
    let business = actions
        .iter()
        .find(|a| a["action"] == "businessResult")
        .unwrap();
    assert_eq!(business["model"], "gpt-6-astra");
    assert_eq!(ticket["access"]["ended"], true);
    assert_eq!(business["access"]["ended"], false);
    assert_ne!(ticket["access"]["id"], business["access"]["id"]);
    let finished = actions.iter().find(|a| a["action"] == "finish").unwrap();
    assert_eq!(finished["access"]["id"], business["access"]["id"]);
    assert_eq!(finished["access"]["ended"], true);
    assert_eq!(finished["access"]["outcome"], "completed");
    assert!(
        finished["access"]["elapsedMs"].as_u64().unwrap()
            >= business["access"]["elapsedMs"].as_u64().unwrap()
    );
    // 拿票只是材料获取，不单独判定，也不触发找票。
    assert!(
        !actions
            .iter()
            .any(|a| a["action"] == "probeResult" || a["action"] == "claim")
    );
}

/// 单窗口：续接请求的原账号没有窗口而窗口在别的账号上时，不另开窗口，
/// 报告原账号不可调度，交给核心排除原账号后在持窗账号上重放。
#[tokio::test]
async fn pool_pinned_continuation_yields_to_active_window_account() {
    use gateway_core::engine::ContinuationAttempt;
    use gateway_core::engine::continuation::{
        ContinuationBinding, NativeContinuationPin, PreviousResponseId,
    };
    use gateway_core::error::ProviderErrorKind;
    let upstream = MockServer::start().await;
    let runtime = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.into(),
            name: "Demo".into(),
            secret: secret("demo-token"),
            verified_account: profile("demo-upstream"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    Mock::given(method("POST"))
        .respond_with(|input: &wiremock::Request| {
            let data: Value = input.body_json().unwrap();
            let value = match data["action"].as_str().unwrap() {
                "config" => json!({"enabled":true,"groupIds":[GROUP],"probeIntervalMs":50}),
                "active" => json!({"accounts":["acct_window_holder"]}),
                other => panic!("unexpected runtime action {other}"),
            };
            ResponseTemplate::new(200).set_body_json(value)
        })
        .mount(&runtime)
        .await;
    let mut config = valid_config();
    let key = config._runtime.path().join("pool.key");
    std::fs::write(&key, "a".repeat(64)).unwrap();
    config.config.api.base_url = upstream.uri();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        // 主入口是已关闭的控制面槽，必须自动改用备用入口。
        runtime_url: "http://127.0.0.1:9/".into(),
        key_file: key,
        fallback_url: Some(runtime.uri()),
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let pin = NativeContinuationPin::new(
        PreviousResponseId::new("resp_previous"),
        PreviousResponseId::new("resp_upstream"),
        ClientApiKeyId::new("key_gateway_pool").unwrap(),
        ProviderKind::new("openai").unwrap(),
        ProviderAccountId::new(ACCOUNT).unwrap(),
    );
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_gateway_pool_pinned").unwrap(),
            ClientApiKeyId::new("key_gateway_pool").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(10),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope()),
        Some(ContinuationBinding::Pinned(pin)),
        CancellationToken::new(),
    )
    .with_continuation_attempt(ContinuationAttempt::Native);
    let Err(error) = bundle
        .core_provider()
        .execute(request(json!([])), context)
        .await
    else {
        panic!("pinned account without the window must yield");
    };
    assert_eq!(error.kind(), ProviderErrorKind::NoEligibleAccount);
    assert!(upstream.received_requests().await.unwrap().is_empty());
}

// 走完整找票流程，响应体故意不是 SSE：探测只读头，错误最多重试一次。
async fn probe_retries(status: u16, phase: &'static str, recovers: bool) {
    use std::sync::atomic::AtomicUsize;
    let upstream = MockServer::start().await;
    let runtime = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.into(),
            name: "Demo".into(),
            secret: secret("demo-token"),
            verified_account: profile("demo-upstream"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account = store.account(ACCOUNT).unwrap();
    let now = SystemTime::now();
    store.compare_and_swap_quota(QuotaObservation {plan_type:None,account_id:account.id().clone(),expected_revision:account.revision(),observed_at:now,state:QuotaState::allowed(now),quota:OpaqueProviderData::new(json!({"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10,"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone())}).await.unwrap();
    let payload = URL_SAFE_NO_PAD.encode(
        json!({"host":"unified-142.example.invalid","exp":Utc::now().timestamp()+600}).to_string(),
    );
    let cookie = format!("__oailb=h.{payload}.s; __cflb=demo");
    Mock::given(method("GET"))
        .and(path("/cookie"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"cookie":cookie})))
        .expect(1)
        .mount(&upstream)
        .await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&attempts);
    Mock::given(method("POST")).and(path("/codex/responses")).respond_with(move |input:&wiremock::Request| {
        let replay = input.headers.get("x-codex-turn-state").is_some();
        if replay { assert_eq!(input.headers["x-codex-turn-state"].to_str().unwrap(),"verified-bootstrap-state"); }
        let in_phase = replay == (phase=="replay");
        let attempt = if in_phase { count.fetch_add(1,Ordering::SeqCst) } else {0};
        if in_phase && (!recovers || attempt==0) {
            ResponseTemplate::new(status).insert_header("retry-after","0").set_body_string("not read")
        } else if !replay {
            ResponseTemplate::new(200).insert_header("x-codex-turn-state","verified-bootstrap-state").set_body_string("not read")
        } else {
            ResponseTemplate::new(200).insert_header("content-type","text/event-stream").set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_pool\",\"status\":\"completed\",\"output\":[]}}\n\n")
        }
    }).mount(&upstream).await;
    let claimed = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    let (c, d, r) = (Arc::clone(&claimed), Arc::clone(&done), Arc::clone(&ready));
    let pool_url = upstream.uri();
    let shared_cookie = cookie.clone();
    Mock::given(method("POST")).respond_with(move |input:&wiremock::Request| {
        let data:Value = input.body_json().unwrap();
        let value = match data["action"].as_str().unwrap() {
            "config" => json!({"enabled":true,"groupIds":[GROUP],"poolAccountIds":{ACCOUNT:"demo"},"poolUrl":pool_url,"poolToken":"demo-pool-token","probeIntervalMs":50}),
            "active" => json!({"accounts":[ACCOUNT],"queue":true}),
            "plan" => if r.load(Ordering::SeqCst) { json!({"ready":{"gateway":"unified-142","generation":"probe"},"probe":null,"preparing":false}) } else { json!({"ready":null,"probe":if d.load(Ordering::SeqCst) {Value::Null}else{json!({"gateway":"unified-142"})},"preparing":c.load(Ordering::SeqCst)&&!d.load(Ordering::SeqCst)}) },
            "claim" => if !c.swap(true,Ordering::SeqCst) {json!({"claimed":true,"leaseId":"probe","pair":{"gateway":"unified-142","generation":"probe"}})} else {json!({"claimed":false})},
            "probeResult" => {d.store(true,Ordering::SeqCst);r.store(data["status"]==200 && data["paired"]==true,Ordering::SeqCst); json!({"accepted":true})},
            "material" => json!({"state":"verified-bootstrap-state","cookie":shared_cookie,"expires":Utc::now().timestamp()+600}),
            "reserve" => json!({"reserved":true,"leaseId":"business"}),
            "businessResult" => json!({"accepted":true,"classification":"21"}),
            _ => json!({"accepted":true}),
        };
        ResponseTemplate::new(200).set_body_json(value)
    }).mount(&runtime).await;
    let mut config = valid_config();
    let key = config._runtime.path().join("pool.key");
    std::fs::write(&key, "a".repeat(64)).unwrap();
    config.config.api.base_url = upstream.uri();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        runtime_url: runtime.uri(),
        key_file: key,
        fallback_url: None,
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let result = bundle
        .core_provider()
        .execute(request(json!([])), context())
        .await;
    if let Ok(mut stream) = result {
        while stream.next().await.is_some() {}
    }
    let records = runtime
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.body_json::<Value>().unwrap())
        .collect::<Vec<_>>();
    let probe_attempts = records
        .iter()
        .filter(|v| v["action"] == "probeAttempt" && v["phase"] == phase)
        .collect::<Vec<_>>();
    assert_eq!(probe_attempts.len(), 2, "exactly one retry");
    assert_eq!(probe_attempts[0]["status"], status);
    assert_eq!(probe_attempts[0]["retry"], 0);
    assert_eq!(probe_attempts[1]["retry"], 1);
    assert_ne!(
        probe_attempts[0]["access"]["id"],
        probe_attempts[1]["access"]["id"]
    );
    for attempt in &probe_attempts {
        assert_eq!(attempt["access"]["ended"], true);
        assert!(attempt["access"]["startedAt"].as_f64().unwrap() > 0.0);
    }
    let final_record = records
        .iter()
        .find(|v| v["action"] == "probeResult")
        .unwrap();
    assert_eq!(final_record["status"], if recovers { 200 } else { status });
    assert_eq!(final_record["stateReturned"], false);
    assert!(
        records
            .iter()
            .any(|v| v["action"] == "finish" && v["leaseId"] == "probe")
    );
}

#[tokio::test]
async fn pool_bootstrap_402_retries_once_and_keeps_actual_error() {
    probe_retries(402, "bootstrap", false).await;
}
#[tokio::test]
async fn pool_bootstrap_429_retries_once_and_keeps_actual_error() {
    probe_retries(429, "bootstrap", false).await;
}
#[tokio::test]
async fn pool_replay_402_retries_once_and_keeps_actual_error() {
    probe_retries(402, "replay", false).await;
}
#[tokio::test]
async fn pool_bootstrap_transient_error_recovers_then_replays_material() {
    probe_retries(402, "bootstrap", true).await;
}

#[tokio::test]
async fn pool_bootstrap_401_retries_once_and_keeps_actual_error() {
    probe_retries(401, "bootstrap", false).await;
}

#[cfg(feature = "gateway-pool-experiment")]
#[tokio::test]
async fn cache_experiment_reloads_account_projection_after_route_preparation() {
    let upstream = MockServer::start().await;
    let runtime = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_oauth_credential(ImportCodexOAuthCredential {
            account_id: ACCOUNT.into(),
            name: "Demo".into(),
            secret: secret("demo-token"),
            verified_account: profile("demo-upstream"),
            next_refresh_at: None,
            enabled: true,
        })
        .await;
    let account = store.account(ACCOUNT).unwrap();
    let now = SystemTime::now();
    store.compare_and_swap_quota(QuotaObservation { plan_type: None, account_id: account.id().clone(), expected_revision: account.revision(), observed_at: now, state: QuotaState::allowed(now), quota: OpaqueProviderData::new(json!({"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10,"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone()) }).await.unwrap();
    let payload = URL_SAFE_NO_PAD.encode(
        json!({"host":"unified-142.example.invalid","exp":Utc::now().timestamp()+600}).to_string(),
    );
    let cookie = format!("__oailb=h.{payload}.s; __cflb=demo");
    let changed = store.clone();
    Mock::given(method("POST")).respond_with(move |request:&wiremock::Request| {
        let data: Value = request.body_json().unwrap();
        let value = match data["action"].as_str().unwrap() {
            "config" => json!({"enabled":true,"mode":"lazy","poolAccountIds":{ACCOUNT:"demo"}}),
            "plan" => { changed.set_scheduling(ACCOUNT, None, gateway_core::account::AccountWeight::new(77).unwrap()); json!({"ready":{"gateway":"unified-142","generation":"cache"},"probe":null}) },
            "material" => json!({"state":"paired-state","cookie":cookie,"expires":Utc::now().timestamp()+600}),
            "reserve" => json!({"reserved":true,"leaseId":"cache-business"}),
            "businessResult" => json!({"accepted":true,"classification":"21"}),
            _ => json!({"accepted":true}),
        };
        ResponseTemplate::new(200).set_body_json(value)
    }).mount(&runtime).await;
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let upstream_calls = calls.clone();
    Mock::given(method("POST")).and(path("/codex/responses")).respond_with(move |request:&wiremock::Request| {
        assert_eq!(request.headers["session-id"], "fixed-cache-session");
        assert_eq!(request.headers["thread-id"], "fixed-cache-session");
        assert_eq!(request.headers["x-codex-turn-state"], "paired-state");
        let bytes = zstd::stream::decode_all(std::io::Cursor::new(&request.body)).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["prompt_cache_key"], "fixed-cache-key");
        let index = upstream_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(body["input"][0]["content"][0]["text"], "stable background");
        assert_eq!(body["input"][1]["content"][0]["text"], "first question");
        if index == 1 {
            assert_eq!(body["input"].as_array().unwrap().len(), 4);
            assert_eq!(body["input"][2], json!({"role":"assistant","content":[{"type":"output_text","text":"real completed reply"}]}));
            assert_eq!(body["input"][3]["content"][0]["text"], "follow-up question");
        }
        ResponseTemplate::new(200).set_body_string("data: {\"type\":\"response.output_text.delta\",\"delta\":\"real completed \"}\n\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"reply\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":4096,\"input_tokens_details\":{\"cached_tokens\":3072},\"output_tokens\":1}}}\n\n")
    }).expect(2).mount(&upstream).await;
    let mut config = valid_config();
    config.config.api.base_url = upstream.uri();
    let key = config._runtime.path().join("runtime.key");
    std::fs::write(&key, "a".repeat(64)).unwrap();
    config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
        runtime_url: runtime.uri(),
        key_file: key,
        fallback_url: None,
    });
    let bundle = provider_openai::initialize(
        config.config.clone(),
        provider_ports_with(store, Arc::new(TestOAuthPending::default())),
    )
    .await
    .unwrap();
    let runner = bundle.gateway_pool_experiment().unwrap();
    let mut body = json!({"model":"gpt-6-astra","input":[{"role":"user","content":[{"type":"input_text","text":"stable background"}]},{"role":"user","content":[{"type":"input_text","text":"first question"}]}],"stream":true,"prompt_cache_key":"fixed-cache-key"});
    let result = runner
        .cache_sample(
            &ProviderAccountId::new(ACCOUNT).unwrap(),
            body.as_object().unwrap().clone(),
            "fixed-cache-session",
            SystemTime::now() + Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(result["cachedTokens"], 3072, "{result}");
    assert_eq!(result["classification"], "21");
    assert_eq!(result["assistantText"], "real completed reply");
    body["input"].as_array_mut().unwrap().extend([json!({"role":"assistant","content":[{"type":"output_text","text":result["assistantText"]}]}),json!({"role":"user","content":[{"type":"input_text","text":"follow-up question"}]})]);
    let follow_up = runner
        .cache_sample(
            &ProviderAccountId::new(ACCOUNT).unwrap(),
            body.as_object().unwrap().clone(),
            "fixed-cache-session",
            SystemTime::now() + Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await;
    assert_eq!(follow_up["classification"], "21", "{follow_up}");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn shared_pool_queue_controls_selection_with_scope_and_quota_guards() {
    const SECOND: &str = "acct_queue_second";
    // 首个案例故意让队首权重较低；其余覆盖停用白名单、Key 范围和额度不可用。
    for (queue, ids, exhausted, expected) in [
        (vec![SECOND, ACCOUNT], vec![ACCOUNT, SECOND], false, SECOND),
        (vec![ACCOUNT], vec![ACCOUNT, SECOND], false, ACCOUNT),
        (vec![SECOND, ACCOUNT], vec![ACCOUNT], false, ACCOUNT),
        (vec![SECOND, ACCOUNT], vec![ACCOUNT, SECOND], true, ACCOUNT),
    ] {
        let runtime = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        for id in [ACCOUNT, SECOND] {
            store
                .seed_oauth_credential(ImportCodexOAuthCredential {
                    account_id: id.into(),
                    name: id.into(),
                    secret: secret("demo-token"),
                    verified_account: profile(id),
                    next_refresh_at: None,
                    enabled: true,
                })
                .await;
            store.set_scheduling(
                id,
                None,
                gateway_core::account::AccountWeight::new(if id == ACCOUNT { 90 } else { 1 })
                    .unwrap(),
            );
            let account = store.account(id).unwrap();
            let now = SystemTime::now();
            let denied = exhausted && id == SECOND;
            store.compare_and_swap_quota(QuotaObservation {
                plan_type: None, account_id: account.id().clone(), expected_revision: account.revision(), observed_at: now,
                state: if denied { QuotaState::exhausted(QuotaEvidence::ProviderDenied, now, None) } else { QuotaState::allowed(now) },
                quota: OpaqueProviderData::new(json!({"rate_limit":{"allowed":!denied,"limit_reached":denied,"primary_window":{"used_percent":if denied {100}else{10},"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone()),
            }).await.unwrap();
        }
        let authorized = ids.clone();
        Mock::given(method("POST")).respond_with(move |input: &wiremock::Request| {
            let data: Value = input.body_json().unwrap();
            let value = match data["action"].as_str().unwrap() {
                "config" => json!({"enabled":true,"groupIds":[GROUP],"probeIntervalMs":50,"probeModel":"gpt-6-astra"}),
                "active" => {
                    let candidates = data["candidates"].as_array().unwrap();
                    assert_eq!(candidates.len(), authorized.len());
                    assert!(candidates.iter().all(|item| authorized.contains(&item["id"].as_str().unwrap())));
                    json!({"accounts":queue,"queue":true})
                },
                "plan" => json!({"ready":null,"probe":null,"preparing":false}),
                "quota" => json!({"accepted":true}),
                other => panic!("unexpected runtime action {other}"),
            };
            ResponseTemplate::new(200).set_body_json(value)
        }).mount(&runtime).await;
        let mut config = valid_config();
        let key = config._runtime.path().join("pool.key");
        std::fs::write(&key, "a".repeat(64)).unwrap();
        config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
            runtime_url: runtime.uri(),
            key_file: key,
            fallback_url: None,
        });
        let bundle = provider_openai::initialize(
            config.config.clone(),
            provider_ports_with(store, Arc::new(TestOAuthPending::default())),
        )
        .await
        .unwrap();
        // 无可用网关时停止于选定账号的计划查询，不向真实上游发送请求。
        let mut stream = bundle
            .core_provider()
            .execute(request(json!([])), context_for(scope_for(&ids)))
            .await
            .unwrap();
        let mut failed = false;
        while let Some(event) = stream.next().await {
            if event.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        let records = runtime.received_requests().await.unwrap();
        let plans: Vec<Value> = records
            .iter()
            .map(|r| r.body_json::<Value>().unwrap())
            .filter(|v| v["action"] == "plan")
            .collect();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0]["accountId"], expected);
    }
}

#[tokio::test]
async fn mixed_key_prefers_pool_then_falls_back_and_recovers_for_accounts_and_groups() {
    use gateway_core::engine::ContinuationAttempt;
    use gateway_core::engine::continuation::{
        ContinuationBinding, NativeContinuationPin, PreviousResponseId,
    };
    use std::sync::atomic::AtomicUsize;
    const NORMAL: &str = "acct_normal";
    const LOWER: &str = "acct_normal_lower";
    const OUTSIDE: &str = "acct_outside";
    for direct in [false, true] {
        let upstream = MockServer::start().await;
        let runtime = MockServer::start().await;
        let store = Arc::new(MemoryAccountStore::default());
        let provider = ProviderKind::new("openai").unwrap();
        let pool_group = AccountGroupId::new(GROUP).unwrap();
        let normal_group = AccountGroupId::new("grp_00000000000000000000000000000002").unwrap();
        let outside_group = AccountGroupId::new("grp_00000000000000000000000000000003").unwrap();
        let mut directory = BTreeMap::new();
        for (id, weight, groups) in [
            (
                ACCOUNT,
                1,
                BTreeSet::from([pool_group.clone(), normal_group.clone()]),
            ),
            (NORMAL, 80, BTreeSet::from([normal_group.clone()])),
            (LOWER, 10, BTreeSet::from([normal_group.clone()])),
            (OUTSIDE, 100, BTreeSet::from([outside_group])),
        ] {
            directory.insert(
                ProviderAccountId::new(id).unwrap(),
                RuntimeAccount::new(provider.clone(), groups),
            );
            store
                .seed_oauth_credential(ImportCodexOAuthCredential {
                    account_id: id.into(),
                    name: id.into(),
                    secret: secret(id),
                    verified_account: profile(id),
                    next_refresh_at: None,
                    enabled: true,
                })
                .await;
            store.set_scheduling(
                id,
                None,
                gateway_core::account::AccountWeight::new(weight).unwrap(),
            );
            let account = store.account(id).unwrap();
            let now = SystemTime::now();
            store.compare_and_swap_quota(QuotaObservation {
                plan_type: None, account_id: account.id().clone(), expected_revision: account.revision(), observed_at: now,
                state: QuotaState::allowed(now), quota: OpaqueProviderData::new(json!({"rate_limit":{"allowed":true,"limit_reached":false,"primary_window":{"used_percent":10,"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone()),
            }).await.unwrap();
        }
        let directory = Arc::new(RuntimeAccountDirectory::new(directory));
        let scope = if direct {
            ClientRoutingScope::accounts(
                [ACCOUNT, NORMAL, LOWER]
                    .iter()
                    .map(|id| ProviderAccountId::new(*id).unwrap())
                    .collect(),
                &directory,
            )
        } else {
            ClientRoutingScope::restricted(
                vec![
                    RoutingGroupSnapshot::new(pool_group.clone(), "Pool".into()),
                    RoutingGroupSnapshot::new(normal_group.clone(), "Normal".into()),
                ],
                BTreeSet::from([pool_group, normal_group]),
                BTreeSet::from([provider]),
            )
            .unwrap()
        };
        let scope = Arc::new(FrozenAccountScope::new(directory, scope));
        let phase = Arc::new(AtomicUsize::new(0));
        let current = phase.clone();
        let payload = URL_SAFE_NO_PAD.encode(
            json!({"host":"unified-142.example.invalid","exp":Utc::now().timestamp()+600})
                .to_string(),
        );
        let cookie = format!("__oailb=h.{payload}.s; __cflb=demo");
        Mock::given(method("POST")).respond_with(move |input: &wiremock::Request| {
            let data: Value = input.body_json().unwrap();
            let value = match data["action"].as_str().unwrap() {
                "config" => json!({"enabled":true,"groupIds":[GROUP],"probeIntervalMs":50,"probeModel":"gpt-6-astra"}),
                "active" => {
                    let candidates = data["candidates"].as_array().unwrap();
                    assert_eq!(candidates.len(), 1, "ordinary and unauthorized accounts must never enter pool queue");
                    assert_eq!(candidates[0]["id"], ACCOUNT);
                    if current.load(Ordering::SeqCst) == 3 { return ResponseTemplate::new(503); }
                    json!({"accounts":if current.load(Ordering::SeqCst)==1 {vec![]}else{vec![ACCOUNT]},"queue":true})
                },
                "plan" => {
                    assert_eq!(data["accountId"], ACCOUNT);
                    json!({"ready":{"gateway":"unified-142","generation":"mixed"},"probe":null,"preparing":false})
                },
                "material" => json!({"state":"pool-state","cookie":cookie,"expires":Utc::now().timestamp()+600}),
                "reserve" => json!({"reserved":true,"leaseId":"mixed-business"}),
                "businessResult" => json!({"accepted":true,"classification":"21"}),
                _ => json!({"accepted":true}),
            };
            ResponseTemplate::new(200).set_body_json(value)
        }).mount(&runtime).await;
        Mock::given(method("POST")).and(path("/codex/responses")).respond_with(|input: &wiremock::Request| {
            if input.headers["authorization"] == format!("Bearer {ACCOUNT}") {
                assert_eq!(input.headers["x-codex-turn-state"], "pool-state");
                assert!(input.headers.contains_key("cookie"));
            } else {
                assert_eq!(input.headers["authorization"], format!("Bearer {NORMAL}"));
                assert_ne!(input.headers.get("x-codex-turn-state").and_then(|v|v.to_str().ok()), Some("pool-state"));
                assert!(!input.headers.contains_key("cookie"));
            }
            ResponseTemplate::new(200).insert_header("content-type","text/event-stream").set_body_string("data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_mixed\",\"status\":\"completed\",\"output\":[]}}\n\n")
        }).mount(&upstream).await;
        let mut config = valid_config();
        let key = config._runtime.path().join("pool.key");
        std::fs::write(&key, "a".repeat(64)).unwrap();
        config.config.api.base_url = upstream.uri();
        config.config.gateway_pool = Some(provider_openai::gateway_pool::GatewayPoolConfig {
            runtime_url: runtime.uri(),
            key_file: key,
            fallback_url: None,
        });
        let bundle = provider_openai::initialize(
            config.config.clone(),
            provider_ports_with(store.clone(), Arc::new(TestOAuthPending::default())),
        )
        .await
        .unwrap();
        // 池可用、池停用/冷却、恢复、控制面失败、请求内已排除池账号。
        for (step, expected) in [
            (0, ACCOUNT),
            (1, NORMAL),
            (2, ACCOUNT),
            (3, NORMAL),
            (4, NORMAL),
            (5, NORMAL),
            (6, NORMAL),
            (7, NORMAL),
        ] {
            phase.store(step, Ordering::SeqCst);
            if step == 7 {
                let account = store.account(ACCOUNT).unwrap();
                let now = SystemTime::now();
                store.compare_and_swap_quota(QuotaObservation {
                    plan_type: None, account_id: account.id().clone(), expected_revision: account.revision(), observed_at: now,
                    state: QuotaState::exhausted(QuotaEvidence::ProviderDenied, now, None),
                    quota: OpaqueProviderData::new(json!({"rate_limit":{"allowed":false,"limit_reached":true,"primary_window":{"used_percent":100,"reset_at":Utc::now().timestamp()+18000}}}).as_object().unwrap().clone()),
                }).await.unwrap();
            }

            let mut context = context_for(scope.clone());
            if step == 4 {
                context = AttemptContext::new(
                    RequestAttemptContext::new(
                        ModelRequestId::new("req_mixed_retry").unwrap(),
                        ClientApiKeyId::new("key_gateway_pool").unwrap(),
                    ),
                    NonZeroU32::new(2).unwrap(),
                    SystemTime::now() + Duration::from_secs(10),
                    account_policy(),
                    AccountAttemptContext::new(
                        BTreeSet::from([ProviderAccountId::new(ACCOUNT).unwrap()]),
                        None,
                        None,
                    )
                    .with_account_scope(scope.clone()),
                    None,
                    CancellationToken::new(),
                );
            }
            if step == 5 {
                let pin = NativeContinuationPin::new(
                    PreviousResponseId::new("resp_previous"),
                    PreviousResponseId::new("resp_upstream"),
                    ClientApiKeyId::new("key_gateway_pool").unwrap(),
                    ProviderKind::new("openai").unwrap(),
                    ProviderAccountId::new(NORMAL).unwrap(),
                );
                context = AttemptContext::new(
                    RequestAttemptContext::new(
                        ModelRequestId::new("req_mixed_native").unwrap(),
                        ClientApiKeyId::new("key_gateway_pool").unwrap(),
                    ),
                    NonZeroU32::new(1).unwrap(),
                    SystemTime::now() + Duration::from_secs(10),
                    account_policy(),
                    AccountAttemptContext::new(BTreeSet::new(), None, None)
                        .with_account_scope(scope.clone()),
                    Some(ContinuationBinding::Pinned(pin)),
                    CancellationToken::new(),
                )
                .with_continuation_attempt(ContinuationAttempt::Native);
            }
            if step == 6 {
                context = AttemptContext::new(
                    RequestAttemptContext::new(
                        ModelRequestId::new("req_mixed_owner").unwrap(),
                        ClientApiKeyId::new("key_gateway_pool").unwrap(),
                    ),
                    NonZeroU32::new(1).unwrap(),
                    SystemTime::now() + Duration::from_secs(10),
                    account_policy(),
                    AccountAttemptContext::new(
                        BTreeSet::new(),
                        Some(ProviderAccountId::new(NORMAL).unwrap()),
                        None,
                    )
                    .with_account_scope(scope.clone()),
                    None,
                    CancellationToken::new(),
                );
            }
            let result = bundle
                .core_provider()
                .execute(request(json!([])), context)
                .await;
            if step == 5 {
                // 未提供合法续接历史时保持原恢复错误，不把状态交给池账号。
                let Err(error) = result else {
                    panic!("missing native history must require recovery")
                };
                assert_eq!(
                    error.kind(),
                    gateway_core::error::ProviderErrorKind::ContinuationRecoveryRequired
                );
                continue;
            }
            let mut stream = result.unwrap();
            assert_eq!(stream.metadata().provider_account_id().as_str(), expected);
            while let Some(event) = stream.next().await {
                event.unwrap();
            }
            drop(stream);
        }
        assert_eq!(upstream.received_requests().await.unwrap().len(), 7);
    }
}
