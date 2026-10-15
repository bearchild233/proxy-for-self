//! 设置默认值、判定、退役、静置退避、备用票、轮换与候选排序。016 起退避与备用票规则与 Python 版有意不同。

use serde_json::{Value, json};

use crate::{
    error::Failure,
    num::Num,
    util::{Object, num_field, set, str_field, truthy},
};

/// 设置默认值。
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
        "watchScope": "all",
        "unwatchedGroupIds": [],
        "recoverySeconds": 480,
        "recoveryPauseSeconds": 3600,
        "backoffMode": "additive",
        "backoffStepSeconds": 3600
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

/// 检测失败（401、402、429、超时、网络错误、缺材料、Cookie host 不符）的固定静置时长：不升档、不改起始档。
pub const FAILURE_REST_SECONDS: i64 = 300;

/// 备用票自拿到起超过这么久仍未启用就丢弃（S0 约 60 分钟过期，留出余量）。
pub const STANDBY_LIFETIME_SECONDS: i64 = 3000;

fn multiplicative(settings: &Object) -> bool {
    str_field(settings, "backoffMode") == Some("multiplicative")
}

fn step_seconds(settings: &Object) -> Num {
    num_field(settings, "backoffStepSeconds").unwrap_or(Num::Int(3600))
}

/// 升一档：加性加一个步长，乘性翻倍，不超过最长静置。
pub fn escalate(settings: &Object, tier: Num) -> Num {
    let maximum = num_field(settings, "maximumRestSeconds").unwrap_or(Num::Int(0));
    let next = if multiplicative(settings) {
        tier.times(Num::Int(2))
    } else {
        tier.plus(step_seconds(settings))
    };
    maximum.min(next)
}

/// 降一档：加性减一个步长，乘性减半，不低于初始静置。
pub fn lower(settings: &Object, tier: Num) -> Num {
    let initial = num_field(settings, "initialRestSeconds").unwrap_or(Num::Int(0));
    let next = if multiplicative(settings) {
        tier.over(Num::Int(2))
    } else {
        tier.minus(step_seconds(settings))
    };
    initial.max(next)
}

/// 起始档：组合上一次静置后恢复时记录的起点（`learnedRestSeconds`）；旧数据没有它时取最近一次恢复的档，
/// 从未恢复过用初始静置。始终夹在初始静置与最长静置之间。
pub fn start_tier(settings: &Object, pair: &Object) -> Num {
    let initial = num_field(settings, "initialRestSeconds").unwrap_or(Num::Int(0));
    let maximum = num_field(settings, "maximumRestSeconds").unwrap_or(Num::Int(0));
    let base = num_field(pair, "learnedRestSeconds")
        .filter(|value| value.truthy())
        .or_else(|| num_field(pair, "lastRecoveredRestSeconds").filter(|value| value.truthy()))
        .unwrap_or(initial);
    maximum.min(initial.max(base))
}

/// 组合当前所处的档：静置时记录的档，旧数据没有时取上一次静置时长。
pub fn tier_of(pair: &Object) -> Option<Num> {
    num_field(pair, "tierSeconds").or_else(|| num_field(pair, "restSeconds"))
}

/// 退役原因决定下一次静置怎么算。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// 29：复测中的组合升一档，服务中的组合从起始档开始。
    Degraded,
    /// 检测失败：固定短静置，档位不变。
    Failed,
    /// 健康轮换、备用票丢弃、账号退出调度等：从起始档开始。附带记入历史的原因码。
    Planned(&'static str),
}

/// `retire`：把组合转入 draining 并计算下一次静置时长与档位。
pub fn retire(pair: &mut Object, settings: &Object, cause: Cause) {
    let state = str_field(pair, "state").unwrap_or_default().to_owned();
    if matches!(state.as_str(), "cooling" | "draining") {
        return;
    }
    let start = start_tier(settings, pair);
    let (rest, tier, reason) = match cause {
        Cause::Degraded => {
            // 只有复测（preparing）才有“刚等过的档”可升；服务中途 29 从起始档重新开始。
            let waited = if state == "preparing" {
                num_field(pair, "probeTierSeconds")
            } else {
                None
            };
            let tier = waited.map_or(start, |waited| escalate(settings, waited));
            (tier, Some(tier), "29")
        }
        Cause::Failed => (Num::Int(FAILURE_REST_SECONDS), None, "probe_failed"),
        Cause::Planned(reason) => (start, Some(start), reason),
    };
    set(pair, "restSeconds", rest.to_json());
    if let Some(tier) = tier {
        set(pair, "tierSeconds", tier.to_json());
    }
    set(pair, "startTierSeconds", start.to_json());
    set(pair, "state", json!("draining"));
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

/// 组合是否仍在服务窗口里（ready、preparing、standby 之外的组合不参与退役）。
pub fn is_serving(pair: &Object) -> bool {
    matches!(
        str_field(pair, "state"),
        Some("ready" | "preparing" | "standby")
    )
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

/// `recover`：复测为 21 时记录恢复的档。同一组合连续两次在同一档恢复，下次起始档降一档；
/// 否则下次从该档开始。不再因为档不同而清空起点。
pub fn recover(pair: &mut Object, settings: &Object, waited: Option<Num>) {
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
    set(pair, "lastRecoveredRestSeconds", waited.to_json());
    set(pair, "recoveryStreak", streak.to_json());
    let next = if streak.ge(Num::Int(2)) {
        lower(settings, waited)
    } else {
        waited
    };
    set(pair, "learnedRestSeconds", next.to_json());
}

/// Cookie 到期前预留的准备时间；到期与窗口到期都触发轮换。
pub const COOKIE_LEAD_SECONDS: i64 = 60;

/// 窗口计时起点：备用票转正的窗口从第一次业务访问算起（尚无业务访问时为空），其余从首次接触算起。
pub fn window_start(pair: &Object) -> Option<Num> {
    if truthy(pair.get("fromStandby")) {
        num_field(pair, "windowStartedAt")
    } else {
        num_field(pair, "firstContactAt")
    }
}

/// 服务中组合的两类到期：窗口用满（只需准备备用票）与 Cookie 即将到期（需要立刻换）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rotation {
    pub window_over: bool,
    pub cookie_ending: bool,
}

pub fn rotation(pair: &Object, settings: &Object, now: Num) -> Rotation {
    if str_field(pair, "state") != Some("ready") {
        return Rotation::default();
    }
    let window = num_field(settings, "windowSeconds").unwrap_or(Num::Int(0));
    Rotation {
        window_over: window_start(pair).is_some_and(|start| now.ge(start.plus(window))),
        cookie_ending: num_field(pair, "cookieExpiresAt")
            .is_some_and(|expires| now.ge(expires.minus(Num::Int(COOKIE_LEAD_SECONDS)))),
    }
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

    fn additive() -> Object {
        object(json!({
            "maximumRestSeconds": 86400, "initialRestSeconds": 7200,
            "backoffMode": "additive", "backoffStepSeconds": 3600
        }))
    }

    fn multiplicative_settings() -> Object {
        object(json!({
            "maximumRestSeconds": 86400, "initialRestSeconds": 7200,
            "backoffMode": "multiplicative", "backoffStepSeconds": 3600
        }))
    }

    #[test]
    fn escalation_adds_a_step_or_doubles_and_stops_at_the_maximum() {
        let add = additive();
        assert_eq!(escalate(&add, Num::Int(7200)).to_json(), json!(10800));
        assert_eq!(escalate(&add, Num::Int(84000)).to_json(), json!(86400));
        assert_eq!(escalate(&add, Num::Int(86400)).to_json(), json!(86400));
        let mult = multiplicative_settings();
        assert_eq!(escalate(&mult, Num::Int(7200)).to_json(), json!(14400));
        assert_eq!(escalate(&mult, Num::Int(57600)).to_json(), json!(86400));
    }

    #[test]
    fn lowering_subtracts_a_step_or_halves_and_stops_at_the_initial_rest() {
        let add = additive();
        assert_eq!(lower(&add, Num::Int(14400)).to_json(), json!(10800));
        assert_eq!(lower(&add, Num::Int(7200)).to_json(), json!(7200));
        assert_eq!(lower(&add, Num::Int(9000)).to_json(), json!(7200));
        let mult = multiplicative_settings();
        assert_eq!(lower(&mult, Num::Int(28800)).as_f64(), 14400.0);
        assert_eq!(lower(&mult, Num::Int(10000)).as_f64(), 7200.0);
    }

    #[test]
    fn retirement_starts_from_the_start_tier_and_a_failed_probe_keeps_the_tier() {
        let settings = additive();
        // 服务中遇到 29：从起始档开始（旧档不升）。
        let mut serving = object(json!({"state": "ready", "learnedRestSeconds": 10800}));
        retire(&mut serving, &settings, Cause::Degraded);
        assert_eq!(serving["restSeconds"], json!(10800));
        assert_eq!(serving["retiredReason"], json!("29"));
        // 复测 29：在刚等过的档上升一档。
        let mut probing = object(json!({
            "state": "preparing", "probeTierSeconds": 10800, "learnedRestSeconds": 7200
        }));
        retire(&mut probing, &settings, Cause::Degraded);
        assert_eq!(probing["restSeconds"], json!(14400));
        assert_eq!(probing["tierSeconds"], json!(14400));
        assert_eq!(probing["startTierSeconds"], json!(7200));
        // 检测失败：固定 300 秒，不升档、不写档。
        let mut failed = object(json!({
            "state": "preparing", "probeTierSeconds": 10800, "tierSeconds": 10800
        }));
        retire(&mut failed, &settings, Cause::Failed);
        assert_eq!(failed["restSeconds"], json!(300));
        assert_eq!(failed["tierSeconds"], json!(10800));
        assert_eq!(failed["retiredReason"], json!("probe_failed"));
        // 备用票丢弃与健康轮换同样从起始档开始。
        let mut spare = object(json!({"state": "standby"}));
        retire(&mut spare, &settings, Cause::Planned("standby_expired"));
        assert_eq!(spare["restSeconds"], json!(7200));
        assert_eq!(spare["retiredReason"], json!("standby_expired"));
        // 从未静置过的组合复测 29：没有“刚等过的档”，从起始档开始。
        let mut fresh = object(json!({"state": "preparing"}));
        retire(&mut fresh, &settings, Cause::Degraded);
        assert_eq!(fresh["restSeconds"], json!(7200));
        // 已经在静置或排空的组合不再重新计算。
        let mut cooling = object(json!({"state": "cooling", "restSeconds": 5}));
        retire(&mut cooling, &settings, Cause::Degraded);
        assert_eq!(cooling["restSeconds"], json!(5));
    }

    #[test]
    fn start_tier_prefers_the_learned_tier_then_the_last_recovered_tier() {
        let settings = additive();
        assert_eq!(start_tier(&settings, &Object::new()).to_json(), json!(7200));
        let learned =
            object(json!({"learnedRestSeconds": 14400, "lastRecoveredRestSeconds": 3600}));
        assert_eq!(start_tier(&settings, &learned).to_json(), json!(14400));
        let recovered = object(json!({"lastRecoveredRestSeconds": 18000}));
        assert_eq!(start_tier(&settings, &recovered).to_json(), json!(18000));
        // 夹在初始静置与最长静置之间。
        let low = object(json!({"learnedRestSeconds": 60}));
        assert_eq!(start_tier(&settings, &low).to_json(), json!(7200));
        let high = object(json!({"learnedRestSeconds": 200000}));
        assert_eq!(start_tier(&settings, &high).to_json(), json!(86400));
    }

    /// 同一档连续两次恢复后下次起始档降一档，不同档不清空起点。
    #[test]
    fn two_equal_recoveries_lower_the_start_tier_and_a_different_tier_is_remembered() {
        let settings = additive();
        let mut pair = Object::new();
        recover(&mut pair, &settings, Some(Num::Int(14400)));
        assert_eq!(start_tier(&settings, &pair).to_json(), json!(14400));
        recover(&mut pair, &settings, Some(Num::Int(14400)));
        assert_eq!(start_tier(&settings, &pair).to_json(), json!(10800));
        // 降档后在较低的档恢复：记为新的恢复档。
        recover(&mut pair, &settings, Some(Num::Int(10800)));
        assert_eq!(start_tier(&settings, &pair).to_json(), json!(10800));
        recover(&mut pair, &settings, Some(Num::Int(10800)));
        assert_eq!(start_tier(&settings, &pair).to_json(), json!(7200));
        // 下限是初始静置。
        recover(&mut pair, &settings, Some(Num::Int(7200)));
        recover(&mut pair, &settings, Some(Num::Int(7200)));
        assert_eq!(start_tier(&settings, &pair).to_json(), json!(7200));
        // 乘性：减半。
        let mult = multiplicative_settings();
        let mut doubled = Object::new();
        recover(&mut doubled, &mult, Some(Num::Int(28800)));
        recover(&mut doubled, &mult, Some(Num::Int(28800)));
        assert_eq!(start_tier(&mult, &doubled).as_f64(), 14400.0);
        // 没有等待时长（首次探测）不记录。
        let mut untouched = Object::new();
        recover(&mut untouched, &settings, None);
        assert!(untouched.is_empty());
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
    fn rotation_tells_a_used_up_window_from_an_ending_cookie() {
        let settings = object(json!({"windowSeconds": 180.0, "mode": "lazy"}));
        let pair = object(json!({"state": "ready", "firstContactAt": 1000.0}));
        let at = |now: f64| rotation(&pair, &settings, Num::Float(now));
        assert_eq!(at(1179.0), Rotation::default());
        assert_eq!(
            at(1180.0),
            Rotation {
                window_over: true,
                cookie_ending: false
            }
        );
        let cookie = object(json!({"state": "ready", "cookieExpiresAt": 1100.0}));
        assert!(!rotation(&cookie, &settings, Num::Float(1039.0)).cookie_ending);
        assert!(rotation(&cookie, &settings, Num::Float(1040.0)).cookie_ending);
        // 只有服务中的组合谈得上到期。
        let preparing = object(json!({"state": "preparing", "firstContactAt": 1.0}));
        assert_eq!(
            rotation(&preparing, &settings, Num::Float(9999.0)),
            Rotation::default()
        );
        // 备用票转正的窗口从第一次业务访问算起；还没有业务访问时窗口没有开始。
        let promoted =
            object(json!({"state": "ready", "firstContactAt": 1.0, "fromStandby": true}));
        assert!(!rotation(&promoted, &settings, Num::Float(9999.0)).window_over);
        let served = object(json!({
            "state": "ready", "firstContactAt": 1.0, "fromStandby": true, "windowStartedAt": 5000.0
        }));
        assert!(!rotation(&served, &settings, Num::Float(5179.0)).window_over);
        assert!(rotation(&served, &settings, Num::Float(5180.0)).window_over);
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
