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
        json!({"kernelApi":{"major":0,"minor":1},"kernel":{"version":"test","slot":"a"},"role":"kernel"}),
    );
    example::<HelloParams>(
        json!({"kernelApi":{"major":0,"minor":1},"kernel":{"version":"test","slot":null},"role":"admin"}),
    );
    example::<HelloResult>(
        json!({"plugin":{"id":"echo","version":"0.0.0"},"sdk":{"version":"0.0.0"},"kernelApi":{"major":0,"minMinor":1},"hooks":["route.prepare"],"capabilities":["log"],"ownedGroups":[],"watchedGroups":[]}),
    );
    example::<HealthResult>(json!({"ready":true}));
    example::<HealthResult>(json!({"ready":false,"detail":"test"}));
    example::<DrainResult>(json!({"inFlight":2}));
    example::<EmptyParams>(json!({}));
    assert!(is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 0,
            min_minor: 4
        }
    ));
    // 0.5 内核兼容声明 minMinor 5 的插件；插件声明的新增字段均为可选。
    assert!(is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 0,
            min_minor: 5
        }
    ));
    for min_minor in [6, 7, 8] {
        assert!(is_compatible(
            KernelApi::default(),
            PluginApi {
                major: 0,
                min_minor
            }
        ));
    }
    assert!(!is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 0,
            min_minor: 9
        }
    ));
    assert!(!is_compatible(
        KernelApi::default(),
        PluginApi {
            major: 1,
            min_minor: 4
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
    example::<OwnershipChangedParams>(json!({"ownedGroups":["group"],"watchedGroups":["watch"]}));
    assert_eq!(KERNEL_API, (0, 8));
    assert!(hello.watched_groups.is_empty());
    for min_minor in [0, 1, 2, 3] {
        assert!(!is_compatible(
            KernelApi::default(),
            PluginApi {
                major: 0,
                min_minor
            }
        ));
    }
    let old: OwnershipChangedParams = serde_json::from_value(json!({"ownedGroups":[]})).unwrap();
    assert!(old.watched_groups.is_empty());
}

#[test]
fn hello_role_defaults_to_kernel_and_rejects_unknown_values() {
    // 缺省角色按内核连接处理，序列化时显式写出 kernel。
    let legacy: HelloParams = serde_json::from_value(
        json!({"kernelApi":{"major":0,"minor":4},"kernel":{"version":"old","slot":null}}),
    )
    .unwrap();
    assert_eq!(legacy.role, ConnectionRole::Kernel);
    assert_eq!(
        serde_json::to_value(&legacy).unwrap()["role"],
        json!("kernel")
    );
    let admin: HelloParams = serde_json::from_value(
        json!({"kernelApi":{"major":0,"minor":5},"kernel":{"version":"v","slot":null},"role":"admin"}),
    )
    .unwrap();
    assert_eq!(admin.role, ConnectionRole::Admin);
    // 未知角色不能退化为内核连接，必须让握手失败。
    assert!(
        serde_json::from_value::<HelloParams>(
            json!({"kernelApi":{"major":0,"minor":5},"kernel":{"version":"v","slot":null},"role":"root"}),
        )
        .is_err()
    );
}
