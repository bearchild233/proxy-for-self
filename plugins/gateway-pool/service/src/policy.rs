//! 设置默认值、判定、退役、静置、轮换与候选排序，逐项对应 `plugins/gateway-pool/worker/policy.py`。

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

/// `classify`：只有 200、已配对、网关一致且未标记失败的访问才有判定；返回 state 判 29，否则判 21。
/// `status` 与 Python 的 `status != 200` 一致：数字 200 与 200.0 相等，布尔与字符串都不等于 200。
pub fn classify(
    status: Option<&Value>,
    state_returned: bool,
    paired: bool,
    gateway_matches: bool,
    failed: bool,
) -> Option<&'static str> {
    let ok = matches!(status, Some(Value::Number(number)) if number.as_f64() == Some(200.0));
    if !ok || !paired || !gateway_matches || failed {
        return None;
    }
    Some(if state_returned { "29" } else { "21" })
}

/// `recover`：只学习本轮最后一档；连续两轮同档成功才提高以后起点。
pub fn recover(pair: &mut Object, waited: Option<Num>) {
    let Some(waited) = waited else {
        return;
    };
    let previous = num_field(pair, "lastRecoveredRestSeconds");
    let same = previous.is_some_and(|previous| previous.as_f64() == waited.as_f64());
    let streak = if same {
        num_field(pair, "recoveryStreak")
            .unwrap_or(Num::Int(0))
            .plus(Num::Int(1))
    } else {
        Num::Int(1)
    };
    if previous.is_some() && !same {
        set(pair, "learnedRestSeconds", Value::Null);
    }
    set(pair, "lastRecoveredRestSeconds", waited.to_json());
    set(pair, "recoveryStreak", streak.to_json());
    if streak.ge(Num::Int(2)) {
        set(pair, "learnedRestSeconds", waited.to_json());
    }
}

/// Cookie 到期前预留的准备时间；到期与窗口到期都触发轮换。
pub const COOKIE_LEAD_SECONDS: i64 = 60;

/// `rotation_due`：窗口用满或 Cookie 即将到期，且有需求（或处于持续模式）时需要轮换。
pub fn rotation_due(pair: &Object, settings: &Object, now: Num, demand: bool) -> bool {
    let window = num_field(settings, "windowSeconds").unwrap_or(Num::Int(0));
    let window_over =
        num_field(pair, "firstContactAt").is_some_and(|first| now.ge(first.plus(window)));
    let cookie_ending = num_field(pair, "cookieExpiresAt")
        .is_some_and(|expires| now.ge(expires.minus(Num::Int(COOKIE_LEAD_SECONDS))));
    str_field(pair, "state") == Some("ready")
        && (window_over || cookie_ending)
        && (demand || str_field(settings, "mode") == Some("continuous"))
}

/// `candidate_order` 的排序键：Cookie 剩余不足一个窗口的靠后，其次最久未访问的优先，最后按网关名。
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateKey {
    short: bool,
    last_contact: f64,
    gateway: String,
}

impl PartialOrd for CandidateKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(
            self.short
                .cmp(&other.short)
                .then(self.last_contact.total_cmp(&other.last_contact))
                .then_with(|| self.gateway.cmp(&other.gateway)),
        )
    }
}

pub fn candidate_order(pair: &Object, entry: Option<&Object>, settings: &Object) -> CandidateKey {
    let window = num_field(settings, "windowSeconds").unwrap_or(Num::Int(0));
    let short = entry.is_some_and(|entry| {
        num_field(entry, "validForS")
            .unwrap_or(Num::Int(0))
            .lt(window.plus(Num::Int(COOKIE_LEAD_SECONDS)))
    });
    // `pair.get('lastContactAt') or 0`：缺失、空值与零都按 0 排序。
    let last_contact = num_field(pair, "lastContactAt").map_or(0.0, Num::as_f64);
    CandidateKey {
        short,
        last_contact,
        gateway: str_field(pair, "gateway").unwrap_or_default().to_owned(),
    }
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

    /// 对应 `test_two_equal_recoveries_learn_and_different_recovery_resets`。
    #[test]
    fn two_equal_recoveries_learn_and_different_recovery_resets() {
        let settings = object(json!({"maximumRestSeconds": 86400, "initialRestSeconds": 3600}));
        let mut pair = Object::new();
        let learned = |pair: &Object| num_field(pair, "learnedRestSeconds");
        recover(&mut pair, Some(Num::Int(7200)));
        assert_eq!(initial(&settings, learned(&pair)).to_json(), json!(3600));
        recover(&mut pair, Some(Num::Int(7200)));
        assert_eq!(initial(&settings, learned(&pair)).to_json(), json!(7200));
        recover(&mut pair, Some(Num::Int(14400)));
        assert_eq!(initial(&settings, learned(&pair)).to_json(), json!(3600));
        recover(&mut pair, Some(Num::Int(14400)));
        assert_eq!(initial(&settings, learned(&pair)).to_json(), json!(14400));
        // 没有等待时长（首次探测）不学习。
        let mut untouched = Object::new();
        recover(&mut untouched, None);
        assert!(untouched.is_empty());
    }

    #[test]
    fn classification_requires_a_paired_matching_200() {
        let ok = json!(200);
        assert_eq!(classify(Some(&ok), false, true, true, false), Some("21"));
        assert_eq!(classify(Some(&ok), true, true, true, false), Some("29"));
        assert_eq!(
            classify(Some(&json!(200.0)), false, true, true, false),
            Some("21")
        );
        assert_eq!(classify(Some(&json!(429)), false, true, true, false), None);
        assert_eq!(
            classify(Some(&json!("200")), false, true, true, false),
            None
        );
        assert_eq!(classify(Some(&json!(true)), false, true, true, false), None);
        assert_eq!(classify(None, false, true, true, false), None);
        assert_eq!(classify(Some(&ok), false, false, true, false), None);
        assert_eq!(classify(Some(&ok), false, true, false, false), None);
        assert_eq!(classify(Some(&ok), false, true, true, true), None);
    }

    #[test]
    fn rotation_is_due_by_window_or_cookie_only_with_demand_or_continuous_mode() {
        let lazy = object(json!({"windowSeconds": 180.0, "mode": "lazy"}));
        let continuous = object(json!({"windowSeconds": 180.0, "mode": "continuous"}));
        let pair = object(json!({"state": "ready", "firstContactAt": 1000.0}));
        assert!(!rotation_due(&pair, &lazy, Num::Float(1179.0), true));
        assert!(rotation_due(&pair, &lazy, Num::Float(1180.0), true));
        assert!(!rotation_due(&pair, &lazy, Num::Float(1180.0), false));
        assert!(rotation_due(&pair, &continuous, Num::Float(1180.0), false));
        let cookie = object(json!({"state": "ready", "cookieExpiresAt": 1100.0}));
        assert!(!rotation_due(&cookie, &lazy, Num::Float(1039.0), true));
        assert!(rotation_due(&cookie, &lazy, Num::Float(1040.0), true));
        let preparing = object(json!({"state": "preparing", "firstContactAt": 1.0}));
        assert!(!rotation_due(&preparing, &lazy, Num::Float(9999.0), true));
    }

    #[test]
    fn candidates_prefer_long_cookies_then_the_longest_idle_gateway() {
        let settings = object(json!({"windowSeconds": 180.0}));
        let stale = object(json!({"gateway": "unified-1", "lastContactAt": 500.0}));
        let fresh = object(json!({"gateway": "unified-2", "lastContactAt": 900.0}));
        let short = object(json!({"validForS": 100}));
        let long = object(json!({"validForS": 2000}));
        let key = |pair: &Object, entry: Option<&Object>| candidate_order(pair, entry, &settings);
        assert!(key(&stale, Some(&long)) < key(&fresh, Some(&long)));
        assert!(key(&fresh, Some(&long)) < key(&stale, Some(&short)));
        assert!(key(&stale, None) < key(&fresh, None));
        let tie_a = object(json!({"gateway": "unified-1"}));
        let tie_b = object(json!({"gateway": "unified-2"}));
        assert!(key(&tie_a, None) < key(&tie_b, None));
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
