use kernel_protocol::*;
use serde_json::{Value, json};

fn example<T: serde::Serialize + serde::de::DeserializeOwned>(wire: Value) {
    let mut extended = wire.clone();
    extended["futureField"] = json!(true);
    let value: T = serde_json::from_value(extended).unwrap();
    assert_eq!(serde_json::to_value(value).unwrap(), wire);
}

#[test]
fn every_capability_matches_wire_contract() {
    example::<UpstreamProbeParams>(
        json!({"accountId":"a","model":"m","turnState":null,"cookie":null,"includeAccountCookies":false,"timeoutMs":7}),
    );
    example::<UpstreamProbeResult>(
        json!({"status":200,"errorKind":null,"stateReturned":true,"turnState":"s","setCookies":["test"],"rateLimitHeaders":[["remaining","1"]],"retryAfterMs":null,"durationMs":7}),
    );
    for (method, egress) in [
        ("GET", json!({"kind":"direct"})),
        ("POST", json!({"kind":"account","accountId":"a"})),
    ] {
        example::<HttpFetchParams>(
            json!({"url":"https://example.invalid/","method":method,"headers":[["accept","text/plain"]],"body":null,"egress":egress,"timeoutMs":7}),
        );
    }
    example::<HttpFetchResult>(
        json!({"status":200,"headers":[["content-type","text/plain"]],"body":"test"}),
    );
    example::<AccountsReadParams>(json!({"ids":["a"],"model":null}));
    assert_eq!(
        serde_json::from_value::<AccountsReadParams>(json!({}))
            .unwrap()
            .ids,
        None
    );
    example::<AccountsReadResult>(
        json!({"accounts":[{"id":"a","enabled":true,"status":"ready","authenticationKind":"oauth","groups":["g"],"upstreamAccountId":null,"userId":null,"modelAllowed":null}]}),
    );
    example::<GroupsMoveParams>(
        json!({"accountId":"a","removeGroup":null,"addGroup":"g","reason":{"code":"test","message":"test"}}),
    );
    for outcome in ["moved", "unchanged"] {
        example::<GroupsMoveResult>(json!({"outcome":outcome}));
    }
    example::<QuotaReadParams>(json!({"accountId":"a","refreshIfMissing":false}));
    example::<QuotaReadResult>(json!({"snapshot":{"remaining":1}}));
    example::<LogParams>(json!({"level":"info","message":"test","fields":{"key":"value"}}));
}

#[test]
fn fetch_truncation_is_optional() {
    example::<HttpFetchResult>(
        json!({"status":200,"headers":[],"body":"partial","truncated":true}),
    );
}

#[test]
fn protocol_six_defaults_and_model_fields() {
    let old: QuotaReadParams = serde_json::from_value(json!({"accountId":"a"})).unwrap();
    assert!(!old.refresh_if_missing);
    let old: AccountsReadParams = serde_json::from_value(json!({"ids":["a"]})).unwrap();
    assert_eq!(old.model, None);
    example::<AccountsReadParams>(json!({"ids":["a"],"model":"gpt-test"}));
    example::<QuotaReadParams>(json!({"accountId":"a","refreshIfMissing":true}));
    for allowed in [true, false] {
        example::<AccountSummary>(
            json!({"id":"a","enabled":true,"status":"normal","authenticationKind":"oauth","groups":[],"upstreamAccountId":null,"userId":null,"modelAllowed":allowed}),
        );
    }
}
