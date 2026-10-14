//! 设置默认值与网关组合退役规则，逐项对应 `plugins/gateway-pool/worker/policy.py`。
//! 轮换、恢复学习等运行规则在后续步骤迁入；这里只包含设置变更副作用所需的部分。

use serde_json::{Value, json};

use crate::{
    error::Failure,
    num::Num,
    util::{Object, num_field, set, str_field, truthy},
};

/// `DEFAULTS`：字段名与取值必须与 policy.py 完全一致。
pub fn defaults() -> Object {
    let Value::Object(object) = json!({
        "mode": "lazy",
        "windowSeconds": 180,
        "initialRestSeconds": 7200,
        "maximumRestSeconds": 86400,
        "probeIntervalMs": 50,
        "groupIds": [],
        "gateways": [],
        "poolAccountIds": {},
        "poolUrl": "https://pool.0102400.xyz",
        "probeModel": "gpt-6.1-sol",
        "poolDirect": true,
        "dynamicGroups": false,
        "normalGroupId": "",
        "degradedGroupId": "",
        "recoverySeconds": 480,
        "recoveryPauseSeconds": 3600
    }) else {
        return Object::new();
    };
    object
}

/// `finite_seconds`：有限数值，且 1 <= value <= maximum；返回浮点数（与 Python `float(value)` 相同）。
pub fn finite_seconds(value: Option<&Value>, name: &str, maximum: f64) -> Result<f64, Failure> {
    match value.and_then(Value::as_f64) {
        Some(number) if number.is_finite() && (1.0..=maximum).contains(&number) => Ok(number),
        _ => Failure::invalid(&format!("{name}无效")),
    }
}

/// `initial`：下一次静置起点，取学习值（若有）与初始静置中的较小者，但不超过最大静置。
pub fn initial(settings: &Object, learned: Option<Num>) -> Num {
    let maximum = num_field(settings, "maximumRestSeconds").unwrap_or(Num::Int(0));
    let start = match learned {
        Some(value) if value.truthy() => value,
        _ => num_field(settings, "initialRestSeconds").unwrap_or(Num::Int(0)),
    };
    maximum.min(start)
}

/// `retire`：把组合转入 draining 并计算下一次静置时长。
pub fn retire(pair: &mut Object, settings: &Object, degraded: bool, failed: bool) {
    let state = str_field(pair, "state").unwrap_or_default().to_owned();
    if matches!(state.as_str(), "cooling" | "draining") {
        return;
    }
    let waited = if state == "preparing" {
        num_field(pair, "probeRestSeconds")
    } else {
        None
    };
    let rest = match waited {
        Some(waited) if degraded || failed => {
            let maximum = num_field(settings, "maximumRestSeconds").unwrap_or(Num::Int(0));
            maximum.min(waited.times(Num::Int(2)))
        }
        _ => initial(settings, num_field(pair, "learnedRestSeconds")),
    };
    set(pair, "restSeconds", rest.to_json());
    set(pair, "state", json!("draining"));
    let reason = if degraded {
        "29"
    } else if failed {
        "probe_failed"
    } else {
        "rotation"
    };
    set(pair, "retiredReason", json!(reason));
}

/// `settle`：本地结束且无在途请求后才进入静置；从未联系过的组合回到 unknown。
pub fn settle(pair: &mut Object) {
    if str_field(pair, "state") != Some("draining") || truthy(pair.get("inFlight")) {
        return;
    }
    match num_field(pair, "lastContactAt") {
        None => {
            set(pair, "state", json!("unknown"));
            set(pair, "restUntil", Value::Null);
        }
        Some(last) => {
            let rest = num_field(pair, "restSeconds").unwrap_or(Num::Int(0));
            set(pair, "state", json!("cooling"));
            set(pair, "restUntil", last.plus(rest).to_json());
        }
    }
}

/// 组合是否仍在服务窗口里（ready 或 preparing 之外的组合不参与退役）。
pub fn is_serving(pair: &Object) -> bool {
    matches!(str_field(pair, "state"), Some("ready" | "preparing"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn object(value: Value) -> Object {
        match value {
            Value::Object(object) => object,
            _ => Object::new(),
        }
    }

    #[test]
    fn degraded_retirement_doubles_last_tier_up_to_the_maximum() {
        let settings = object(json!({"maximumRestSeconds": 86400.0, "initialRestSeconds": 7200.0}));
        let mut pair = object(json!({"state": "preparing", "probeRestSeconds": 57600.0}));
        retire(&mut pair, &settings, true, false);
        assert_eq!(pair["restSeconds"], json!(86400.0));
        assert_eq!(pair["retiredReason"], json!("29"));
        assert_eq!(pair["state"], json!("draining"));
    }

    #[test]
    fn learned_rest_applies_only_below_the_maximum() {
        let settings = object(json!({"maximumRestSeconds": 8, "initialRestSeconds": 1}));
        assert_eq!(initial(&settings, None).to_json(), json!(1));
        assert_eq!(initial(&settings, Some(Num::Int(4))).to_json(), json!(4));
        assert_eq!(initial(&settings, Some(Num::Int(20))).to_json(), json!(8));
    }

    #[test]
    fn settle_moves_drained_pairs_to_unknown_or_cooling() {
        let mut never = object(json!({"state": "draining", "restSeconds": 3600}));
        settle(&mut never);
        assert_eq!(never["state"], json!("unknown"));
        assert_eq!(never["restUntil"], Value::Null);

        let mut contacted = object(
            json!({"state": "draining", "restSeconds": 3600, "lastContactAt": 10000.0, "inFlight": 0}),
        );
        settle(&mut contacted);
        assert_eq!(contacted["state"], json!("cooling"));
        assert_eq!(contacted["restUntil"], json!(13600.0));

        let mut busy = object(
            json!({"state": "draining", "restSeconds": 3600, "lastContactAt": 10000.0, "inFlight": 1}),
        );
        settle(&mut busy);
        assert_eq!(busy["state"], json!("draining"));
    }

    #[test]
    fn finite_seconds_rejects_booleans_and_out_of_range_values() {
        assert_eq!(
            finite_seconds(Some(&json!(180)), "windowSeconds", 3600.0).ok(),
            Some(180.0)
        );
        assert!(finite_seconds(Some(&json!(true)), "windowSeconds", 3600.0).is_err());
        assert!(finite_seconds(Some(&json!(0)), "windowSeconds", 3600.0).is_err());
        assert!(finite_seconds(Some(&json!(3601)), "windowSeconds", 3600.0).is_err());
        assert!(finite_seconds(None, "windowSeconds", 3600.0).is_err());
    }
}
