use kernel_protocol::*;
use serde_json::{Value, json};

#[test]
fn four_message_forms_roundtrip_and_ignore_unknown_fields() {
    for wire in [
        json!({"kind":"request","id":7,"method":"route.prepare","params":{}}),
        json!({"kind":"response","id":7,"result":{}}),
        json!({"kind":"response","id":7,"error":{"code":"not_supported","message":"..."}}),
        json!({"kind":"notify","method":"request.finish","params":{}}),
        json!({"kind":"response","id":7,"result":null}),
    ] {
        let message: Message = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&message).unwrap(), wire);
        assert_eq!(
            decode_frame::<Message>(&encode_frame(&message).unwrap()).unwrap(),
            message
        );
        let mut extended = wire;
        extended["futureField"] = json!(true);
        assert_eq!(
            serde_json::from_value::<Message>(extended).unwrap(),
            message
        );
    }
}

fn example<T: serde::Serialize + serde::de::DeserializeOwned>(wire: Value) {
    let value: T = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(serde_json::to_value(value).unwrap(), wire);
}

#[test]
fn handshake_lifecycle_examples_and_compatibility() {
    example::<HelloParams>(
        json!({"kernelApi":{"major":0,"minor":1},"kernel":{"version":"test","slot":"a"}}),
    );
    example::<HelloParams>(
        json!({"kernelApi":{"major":0,"minor":1},"kernel":{"version":"test","slot":null}}),
    );
    example::<HelloResult>(
        json!({"plugin":{"id":"echo","version":"0.0.0"},"sdk":{"version":"0.0.0"},"kernelApi":{"major":0,"minMinor":1},"hooks":["route.prepare"],"capabilities":["log"],"ownedGroups":[]}),
    );
    example::<HealthResult>(json!({"ready":true}));
    example::<HealthResult>(json!({"ready":false,"detail":"test"}));
    example::<DrainResult>(json!({"inFlight":2}));
    example::<EmptyParams>(json!({}));
    assert!(is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 0,
            min_minor: 1
        }
    ));
    assert!(is_compatible(
        KernelApi { major: 0, minor: 2 },
        PluginApi {
            major: 0,
            min_minor: 1
        }
    ));
    assert!(!is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 1,
            min_minor: 0
        }
    ));
    assert!(!is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 0,
            min_minor: 3
        }
    ));
    assert_eq!(
        [
            NOT_SUPPORTED,
            INVALID_PARAMS,
            UNAVAILABLE,
            TIMEOUT,
            FORBIDDEN,
            INTERNAL
        ],
        [
            "not_supported",
            "invalid_params",
            "unavailable",
            "timeout",
            "forbidden",
            "internal"
        ]
    );
}

#[test]
fn old_hello_defaults_ownership_and_new_notification_roundtrips() {
    let hello: HelloResult = serde_json::from_value(json!({
        "plugin":{"id":"old","version":"0.1"},"sdk":{"version":"0.1"},
        "kernelApi":{"major":0,"minMinor":1},"hooks":[],"capabilities":[]
    }))
    .unwrap();
    assert!(hello.owned_groups.is_empty());
    example::<OwnershipChangedParams>(json!({"ownedGroups":["group"]}));
    assert_eq!(KERNEL_API, (0, 2));
}
