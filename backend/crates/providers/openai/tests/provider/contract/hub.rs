use super::*;

#[tokio::test]
async fn quota_replay_carries_completed_tool_history_without_old_account_handles() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/codex/responses"))
        .respond_with(|request: &wiremock::Request| {
            let decoded = zstd::stream::decode_all(std::io::Cursor::new(&request.body));
            let body: Value = serde_json::from_slice(decoded.as_deref().unwrap_or(&request.body)).unwrap();
            if body.get("previous_response_id").is_some() {
                return ResponseTemplate::new(429).set_body_json(json!({"error":{"code":"usage_limit_reached","type":"usage_limit_reached","message":"You have reached your usage limit."}}));
            }
            let output = if body["input"].as_array().is_some_and(|items| items.len() > 1) {
                assert_eq!(body["input"][1]["call_id"], "call_tool");
                assert_eq!(body["input"][2]["type"], "function_call_output");
                assert_eq!(body["input"][2]["output"], "already executed result");
                assert!(body["input"][1].get("id").is_none());
                assert!(!request.headers.contains_key("x-codex-turn-state"));
                json!([])
            } else {
                json!([
                    {"type":"reasoning","id":"rs_old","encrypted_content":"account-bound"},
                    {"type":"function_call","id":"fc_old","call_id":"call_tool","name":"read","arguments":"{}","status":"completed"}
                ])
            };
            ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {}\n\ndata: {}\n\n", json!({"type":"response.created","response":{"id":"resp_history","model":"gpt-5.4"}}), json!({"type":"response.completed","response":{"id":"resp_history","model":"gpt-5.4","status":"completed","output":output}})))
        }).mount(&server).await;
    let provider = provider_with_base_url(&store, server.uri());
    let generate = |body: Value| {
        GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body.as_object().unwrap().clone())
                .unwrap()
                .with_original_json(Bytes::from(body.to_string()))
                .with_context(Map::from_iter([("use_websocket".to_owned(), json!(false))])),
        )
    };
    let first = generate(
        json!({"model":"gpt-5.4","input":[{"role":"user","content":"read once"}],"store":true,"stream":true}),
    );
    let mut stream = provider
        .execute(
            planned_request("openai", Operation::Generate(first)),
            context("req_history_first", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut state = None;
    while let Some(event) = stream.next().await {
        if let Some(update) = event.unwrap().take_session_update() {
            state = Some(update);
        }
    }
    drop(stream);
    let state = state.expect("completed history captured");
    assert_eq!(
        state.payload()["replay_history"].as_array().unwrap().len(),
        2
    );
    let next = generate(json!({"model":"gpt-5.4","input":[{"type":"function_call_output","call_id":"call_tool","output":"already executed result"}],"previous_response_id":"resp_history","store":true,"stream":true})).with_provider_session_state(state);
    let mut stream = provider
        .execute(
            planned_request("openai", Operation::Generate(next.clone())),
            context("req_history_delta", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::Native),
        )
        .await
        .unwrap();
    let error = loop {
        match stream.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("quota rejection expected"),
        }
    };
    assert_eq!(
        error.continuation_recovery_disposition(),
        Some(ContinuationRecoveryDisposition::ProviderReplayAllowed)
    );
    drop(stream);
    create_account(&store, "acct_affinity_switch_b").await;
    let mut stream = provider
        .execute(
            planned_request("openai", Operation::Generate(next)),
            context("req_history_replay", CancellationToken::new())
                .with_continuation_attempt(ContinuationAttempt::ReplayAny),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().provider_account_id().as_str(),
        "acct_affinity_switch_b"
    );
    while let Some(event) = stream.next().await {
        event.unwrap();
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn native_websocket_keeps_original_messages_and_metadata() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let original = r#" { "type" : "response.create", "model":"gpt-5.4", "input":"\u4f60\u597d", "future":1.23000000000000000001 } "#;
    let replies = vec![
        r#" { "type":"response.metadata", "headers":{"x-codex-turn-state":"opaque-state","x-future":["a","b"]}, "future":1.23000000000000000001 } "#.to_owned(),
        r#"{"type":"response.created","response":{"id":"resp_ws_exact","model":"gpt-5.4"}}"#.to_owned(),
        r#"{"type":"codex.rate_limits","future":123456789012345678901234567890}"#.to_owned(),
        "{\n  \"type\": \"future.extension\", \"value\": 1.23000000000000000001\n}".to_owned(),
        r#"{"type":"response.completed","response":{"id":"resp_ws_exact","model":"gpt-5.4","status":"completed","output":[],"usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7,"future":9}}}"#.to_owned(),
    ];
    let expected = replies.clone();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = accept_codex_test_websocket(socket).await;
        let message = websocket.next().await.unwrap().unwrap();
        assert_eq!(message.to_text().unwrap(), original);
        for reply in replies {
            websocket.send(Message::Text(reply.into())).await.unwrap();
        }
    });
    let mut body = serde_json::from_str::<Value>(original)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    body.remove("type");
    let mut generate = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", body)
            .unwrap()
            .with_context(Map::from_iter([(
                "downstream_websocket_connection_id".to_owned(),
                json!("ws_fidelity"),
            )])),
    );
    generate.set_original_websocket_json(Bytes::from_static(original.as_bytes()));
    let mut stream = provider_with_base_url(&store, base_url)
        .execute(
            planned_request("openai", Operation::Generate(generate)),
            context("req_ws_exact", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut actual = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        if let Some(raw) = event.wire_event().and_then(|wire| wire.raw_json_message()) {
            actual.push(std::str::from_utf8(raw).unwrap().to_owned());
        }
    }
    assert_eq!(actual, expected);
    server.await.unwrap();
}

#[tokio::test]
async fn native_http_keeps_body_and_entire_sse_entity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let original = br#" { "model" : "gpt-5.4", "stream" : true, "input":"\u4f60\u597d", "future": {"n":123456789012345678901234567890,"f":1.230000000000001} } "#;
    let response = concat!(
        ": original-comment\r\n\r\n",
        "id: first\nevent: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_exact\",\"model\":\"upstream-original\"}}\n\n",
        "event: future.extension\nretry: 17\ndata: { \"type\": \"future.extension\", \"value\": 123456789012345678901234567890 }\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_exact\",\"model\":\"upstream-original\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":2,\"total_tokens\":7,\"future_detail\":9}}}\n\n",
        ": after-completion\n\ndata: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-codex-turn-state", "opaque-state")
                .set_body_string(response),
        )
        .expect(1)
        .mount(&server)
        .await;
    let object = serde_json::from_slice::<Value>(original)
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object("openai", object)
            .unwrap()
            .with_original_json(Bytes::from_static(original)),
    ));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", operation),
            context("req_exact_http", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut wire = Vec::new();
    let mut completed = 0;
    while let Some(event) = stream.next().await {
        let event = event.expect("native response");
        if let Some(raw) = event.wire_event().and_then(|wire| wire.raw_sse_frame()) {
            wire.extend_from_slice(raw);
        }
        completed += event
            .canonical_facts()
            .iter()
            .filter(|fact| matches!(fact, GatewayEvent::Completed(_)))
            .count();
    }
    assert_eq!(wire, response.as_bytes());
    assert_eq!(completed, 1);
    let requests = server.received_requests().await.unwrap();
    let decoded = zstd::stream::decode_all(std::io::Cursor::new(&requests[0].body)).unwrap();
    assert_eq!(decoded, original);
}

#[tokio::test]
async fn native_json_does_not_force_stream_or_reserialize_response() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let original = br#"{ "model":"gpt-5.4", "input":"test", "stream":false, "future":true }"#;
    let response = br#" { "id":"resp_json", "model":"original", "unknown":1.23000000000000000001, "usage":{"input_tokens":5,"output_tokens":2,"total_tokens":7,"future_detail":{"x":1}} } "#;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(201)
                .insert_header("content-type", "application/json")
                .set_body_bytes(response.to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            serde_json::from_slice::<Value>(original)
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_original_json(Bytes::from_static(original)),
    ));
    let mut stream = provider_with_base_url(&store, server.uri())
        .execute(
            planned_request("openai", operation),
            context("req_exact_json", CancellationToken::new()),
        )
        .await
        .unwrap();
    let mut raw = None;
    let mut status = None;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        if let Some(body) = event.wire_event().and_then(|wire| wire.raw_json_body()) {
            raw = Some(body.clone());
        }
        if let Some(observation) = event.response_observation() {
            status = observation.status_code();
        }
    }
    assert_eq!(raw.unwrap().as_ref(), response);
    assert_eq!(status, Some(201));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        zstd::stream::decode_all(std::io::Cursor::new(&requests[0].body)).unwrap(),
        original
    );
}

#[cfg(unix)]
#[tokio::test]
async fn excel_worker_receives_only_frozen_key_and_selected_account() {
    assert_excel_worker_request(None).await;
}

#[cfg(unix)]
#[tokio::test]
async fn excel_image_generation_uses_worker_not_native() {
    assert_excel_worker_request(Some(ImageRequestKind::Generation)).await;
}

#[cfg(unix)]
#[tokio::test]
async fn excel_image_edit_uses_worker_not_native() {
    assert_excel_worker_request(Some(ImageRequestKind::Edit)).await;
}

#[cfg(unix)]
async fn assert_excel_worker_request(image_kind: Option<ImageRequestKind>) {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    create_account(&store, "acct_wrong").await;
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("worker.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let response = if image_kind.is_some() {
        r#"{"created":1,"data":[{"b64_json":"fake"}],"future":"preserved"}"#
    } else {
        concat!(
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_excel\",\"model\":\"gpt-5.6-sol-excel\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_excel\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
            "data: [DONE]\n\n",
        )
    };
    let endpoint = match image_kind {
        None => "/internal/responses",
        Some(ImageRequestKind::Generation) => "/internal/images/generations",
        Some(ImageRequestKind::Edit) => "/internal/images/edits",
    };
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = Vec::new();
        let split = loop {
            let mut chunk = [0_u8; 4096];
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buffer.extend_from_slice(&chunk[..n]);
            if let Some(index) = buffer.windows(4).position(|part| part == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8_lossy(&buffer[..split]);
        assert!(headers.starts_with(&format!("POST {endpoint} ")));
        assert!(!headers.to_ascii_lowercase().contains("authorization:"));
        let length: usize = headers
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            })
            .unwrap();
        while buffer.len() - split < length {
            let mut chunk = [0_u8; 4096];
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            buffer.extend_from_slice(&chunk[..n]);
        }
        let envelope: Value = serde_json::from_slice(&buffer[split..split + length]).unwrap();
        assert_eq!(envelope["binding"]["key_id"], "key_excel_test");
        assert_eq!(envelope["binding"]["revision"], 7);
        assert_eq!(envelope["binding"]["account_id"], "acct_provider_contract");
        assert_eq!(
            envelope["credential"]["chatgpt_account_id"],
            "chatgpt-acct_provider_contract"
        );
        assert!(envelope["credential"].get("refresh_token").is_none());
        if image_kind.is_some() {
            assert_eq!(envelope["body"]["model"], "gpt-image-2");
        }
        let content_type = if image_kind.is_some() {
            "application/json"
        } else {
            "text/event-stream"
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    let provider = provider_with_base_url(&store, "http://127.0.0.1:1".to_owned())
        .with_excel_worker_socket(Some(&socket))
        .unwrap();
    let kind = ProviderKind::new("openai").unwrap();
    let id = ProviderAccountId::new("acct_provider_contract").unwrap();
    let directory = Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
        id.clone(),
        RuntimeAccount::new(kind.clone(), BTreeSet::new()),
    )])));
    let fixed = ClientRoutingScope::fixed_account(id, &directory);
    let scope = Arc::new(
        FrozenAccountScope::new(directory, fixed)
            .with_binding_revision(7)
            .with_excel_bridge_enabled(true),
    );
    let models = provider
        .query_client_model_catalog(&scope, "codex", "0.155.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(models.len(), 12);
    let operation = if let Some(kind) = image_kind {
        Operation::GenerateImage(ImageRequest::from_raw_json(kind, RawJsonPayload::new(
            "openai", Bytes::from_static(br#"{"model":"gpt-image-2","prompt":"test","images":[{"image_url":"data:image/png;base64,Zg=="}]}"#),
        ).unwrap()))
    } else {
        Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                json!({"model":"gpt-5.6-sol-excel","input":"test","stream":true})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .unwrap(),
        ))
    };
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        account_policy(),
        vec![kind],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new("gpt-5.6-sol-excel").unwrap(),
            &operation,
            scope.clone(),
            &RoutingContext::default(),
        )
        .unwrap();
    let request = ProviderRequest::new(operation, plan.candidates()[0].clone());
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_excel_test").unwrap(),
            ClientApiKeyId::new("key_excel_test").unwrap(),
        ),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(10),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    );
    let mut stream = provider.execute(request, context).await.unwrap();
    let mut actual = Vec::new();
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        if let Some(wire) = event.wire_event()
            && let Some(raw) = wire.raw_sse_frame().or_else(|| wire.raw_json_body())
        {
            actual.extend_from_slice(raw);
        }
    }
    assert_eq!(actual, response.as_bytes());
    server.await.unwrap();
}
