use kernel_protocol::*;
use serde_json::{Value, json};

fn example<T: serde::Serialize + serde::de::DeserializeOwned>(wire: Value) {
    let mut extended = wire.clone();
    extended["futureField"] = json!(true);
    let value: T = serde_json::from_value(extended).unwrap();
    assert_eq!(serde_json::to_value(value).unwrap(), wire);
}

#[test]
fn every_hook_and_action_matches_wire_contract() {
    example::<RouteSelectParams>(
        json!({"requestId":"r","model":"m","accounts":["a"],"ordinaryAvailable":false,"pinned":"a"}),
    );
    example::<RouteSelectResult>(
        json!({"preferred":["a"],"live":["a"],"blocked":[],"pinnedDecision":"stay"}),
    );
    example::<RouteSelectResult>(
        json!({"preferred":[],"live":[],"blocked":[{"accountId":"a","reason":{"code":"test","message":"test"}}],"pinnedDecision":"escape"}),
    );
    example::<RoutePrepareParams>(
        json!({"requestId":"r","attempt":1,"model":"m","deadlineUnixMs":7,"replaySafe":true,"accountId":"a"}),
    );
    example::<RoutePrepareResult>(
        json!({"action":"use","accountId":"a","leaseId":"l","upstream":{"cookie":null,"turnState":"s"}}),
    );
    example::<RoutePrepareResult>(
        json!({"action":"wait","retryAfterMs":7,"reason":{"code":"test","message":"test"}}),
    );
    example::<RoutePrepareResult>(
        json!({"action":"fail","reason":{"code":"test","message":"test"}}),
    );
    example::<ResponseHeadParams>(
        json!({"requestId":"r","leaseId":"l","attempt":1,"status":200,"errorKind":null,"stateReturned":true,"turnState":"s","setCookies":["test"],"durationMs":7,"replaySafe":true}),
    );
    example::<ResponseHeadParams>(
        json!({"requestId":"r","leaseId":"l","attempt":1,"status":null,"errorKind":"connect","stateReturned":false,"turnState":null,"setCookies":[],"durationMs":7,"replaySafe":false}),
    );
    for action in ["accept", "switch"] {
        example::<ResponseHeadResult>(json!({"action":action}));
    }
    example::<ResponseHeadResult>(json!({"action":"retry","delayMs":7}));
    example::<ResponseHeadResult>(
        json!({"action":"fail","reason":{"code":"test","message":"test"}}),
    );
    for outcome in ["completed", "failed", "cancelled", "interrupted"] {
        example::<RequestFinishParams>(
            json!({"requestId":"r","leaseId":"l","outcome":outcome,"durationMs":7,"firstByteMs":null,"usage":null}),
        );
    }
    example::<RequestFinishParams>(
        json!({"requestId":"r","leaseId":"l","outcome":"completed","durationMs":7,"firstByteMs":null,"usage":{"tokens":2}}),
    );
    example::<AdminCallParams>(
        json!({"method":"echo","params":{"text":"test"},"actor":{"role":"admin"}}),
    );
}

#[test]
fn response_observe_wire_contract() {
    let wire = serde_json::json!({"requestId":"r","accountId":"a","attempt":1,"model":"m","canSwitch":true,"requestHadTurnState":false,"status":null,"errorKind":"connect","stateReturned":false,"turnState":null,"setCookies":[],"replaySafe":true});
    let params: kernel_protocol::ResponseObserveParams =
        serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(params).unwrap(), wire);
    for wire in [
        serde_json::json!({"action":"accept"}),
        serde_json::json!({"action":"switchAccount","reason":{"code":"test","message":"fixture"}}),
    ] {
        let result: kernel_protocol::ResponseObserveResult =
            serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(result).unwrap(), wire);
    }
}
