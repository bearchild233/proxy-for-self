//! 设置校验：合并已有设置与本次提交，按固定顺序逐项检查范围与格式。

use serde_json::{Value, json};

use crate::{
    error::Failure,
    policy,
    util::{Object, num_field, str_field},
};

/// 合并已有设置与本次提交，并逐项校验。返回的对象含 `poolToken`（若有），由调用方决定是否对外暴露。
pub fn prepare(previous: &Object, data: &Object) -> Result<Object, Failure> {
    let defaults = policy::defaults();
    if data
        .keys()
        .any(|key| !defaults.contains_key(key) && key != "poolToken")
    {
        return Failure::invalid("网关池设置字段无效");
    }
    let mut settings = previous.clone();
    for (key, value) in data {
        settings.insert(key.clone(), value.clone());
    }
    for (name, maximum) in [
        ("windowSeconds", 3600.0),
        ("initialRestSeconds", 86400.0),
        ("maximumRestSeconds", 86400.0),
        ("probeIntervalMs", 1000.0),
        ("recoverySeconds", 3600.0),
        ("recoveryPauseSeconds", 86400.0),
    ] {
        let value = policy::finite_seconds(settings.get(name), name, maximum)?;
        settings.insert(name.to_owned(), json!(value));
    }
    let interval = settings
        .get("probeIntervalMs")
        .and_then(Value::as_f64)
        .unwrap_or(f64::NAN);
    if interval.fract() != 0.0 {
        return Failure::invalid("候选间隔必须是整数毫秒");
    }
    settings.insert("probeIntervalMs".to_owned(), json!(interval as i64));
    let initial = num_field(&settings, "initialRestSeconds").map(|value| value.as_f64());
    let maximum = num_field(&settings, "maximumRestSeconds").map(|value| value.as_f64());
    let mode = str_field(&settings, "mode");
    if initial > maximum || !matches!(mode, Some("lazy" | "continuous")) {
        return Failure::invalid("静置范围或模式无效");
    }
    if !matches!(
        str_field(&settings, "backoffMode"),
        Some("additive" | "multiplicative")
    ) {
        return Failure::invalid("退避方式无效");
    }
    // 加性步长：60–86400 的整数秒（JSON 里 3600 与 3600.0 都接受，存为整数）。
    let step = settings
        .get("backoffStepSeconds")
        .and_then(Value::as_f64)
        .filter(|step| step.is_finite() && step.fract() == 0.0 && (60.0..=86400.0).contains(step));
    let Some(step) = step else {
        return Failure::invalid("退避步长必须是 60 到 86400 的整数秒");
    };
    settings.insert("backoffStepSeconds".to_owned(), json!(step as i64));
    for (field, valid) in [
        ("groupIds", group_list as fn(&Value) -> bool),
        ("gateways", gateway_list as fn(&Value) -> bool),
    ] {
        if !settings.get(field).is_some_and(valid) {
            return Failure::invalid(&format!("{field}无效"));
        }
    }
    if !matches!(settings.get("poolDirect"), Some(Value::Bool(_))) {
        return Failure::invalid("池服务直连设置无效");
    }
    if !matches!(settings.get("dynamicGroups"), Some(Value::Bool(_))) {
        return Failure::invalid("动态分组开关无效");
    }
    for field in ["normalGroupId", "degradedGroupId"] {
        let valid =
            str_field(&settings, field).is_some_and(|value| value.is_empty() || group_id(value));
        if !valid {
            return Failure::invalid(&format!("{field}无效"));
        }
    }
    let dynamic = matches!(settings.get("dynamicGroups"), Some(Value::Bool(true)));
    if dynamic {
        let normal = str_field(&settings, "normalGroupId").unwrap_or_default();
        let degraded = str_field(&settings, "degradedGroupId").unwrap_or_default();
        if normal.is_empty() || degraded.is_empty() || normal == degraded {
            return Failure::invalid("动态分组需要指定不同的正常分组与降智分组");
        }
        let in_scope = settings
            .get("groupIds")
            .and_then(Value::as_array)
            .is_some_and(|groups| groups.iter().any(|group| group.as_str() == Some(degraded)));
        if !in_scope {
            return Failure::invalid("降智分组必须在网关池分组范围内");
        }
    }
    if !str_field(&settings, "probeModel").is_some_and(model_id) {
        return Failure::invalid("打票模型无效");
    }
    if !account_mapping(settings.get("poolAccountIds")) {
        return Failure::invalid("池账号映射无效");
    }
    if !str_field(&settings, "poolUrl").is_some_and(https_root) {
        return Failure::invalid("池服务地址必须是 HTTPS 根地址");
    }
    match settings.get("poolToken") {
        None => {}
        Some(Value::String(token))
            if token.chars().count() <= 1024 && token.chars().all(|c| ('!'..='~').contains(&c)) => {
        }
        Some(_) => return Failure::invalid("池服务凭证无效"),
    }
    Ok(settings)
}

/// 候选集合与池服务地址是否变化；变化时历史中断并清空队列剩余网关。
pub fn candidates_changed(previous: &Object, settings: &Object) -> bool {
    ["gateways", "groupIds", "poolAccountIds", "poolUrl"]
        .iter()
        .any(|key| previous.get(*key) != settings.get(*key))
}

fn within(text: &str, min: usize, max: usize, allowed: impl Fn(char) -> bool) -> bool {
    let length = text.chars().count();
    (min..=max).contains(&length) && text.chars().all(allowed)
}

/// `[A-Za-z0-9_-]{1,128}`
fn group_id(text: &str) -> bool {
    within(text, 1, 128, |c| {
        c.is_ascii_alphanumeric() || c == '_' || c == '-'
    })
}

/// `[A-Za-z0-9:_-]{1,128}`：账号与映射键。
pub fn account_id(text: &str) -> bool {
    within(text, 1, 128, |c| {
        c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-'
    })
}

/// `unified-[0-9]{1,6}`
pub fn gateway_id(text: &str) -> bool {
    text.strip_prefix("unified-").is_some_and(|digits| {
        (1..=6).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
    })
}

/// `gpt-[0-9a-z.-]{1,60}`
fn model_id(text: &str) -> bool {
    text.strip_prefix("gpt-").is_some_and(|rest| {
        within(rest, 1, 60, |c| {
            c.is_ascii_digit() || c.is_ascii_lowercase() || c == '.' || c == '-'
        })
    })
}

/// 映射值 `[A-Za-z0-9:_.\/-]{1,160}`
fn member_identity(text: &str) -> bool {
    within(text, 1, 160, |c| {
        c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '/' | '-')
    })
}

fn group_list(value: &Value) -> bool {
    identifier_list(value, 512, group_id)
}

fn gateway_list(value: &Value) -> bool {
    identifier_list(value, 512, gateway_id)
}

/// 列表：不超过 `maximum` 项，每项是通过校验的字符串，且没有重复。
pub fn identifier_list(value: &Value, maximum: usize, valid: impl Fn(&str) -> bool) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    if items.len() > maximum {
        return false;
    }
    let mut seen = std::collections::BTreeSet::new();
    items.iter().all(|item| {
        item.as_str()
            .is_some_and(|text| valid(text) && seen.insert(text.to_owned()))
    })
}

fn account_mapping(value: Option<&Value>) -> bool {
    let Some(Value::Object(mapping)) = value else {
        return false;
    };
    mapping.len() <= 512
        && mapping.iter().all(|(key, identity)| {
            account_id(key) && identity.as_str().is_some_and(member_identity)
        })
}

/// 与 `urlsplit` 的判定一致：https、非空主机名、无用户信息、无查询与片段、路径为空或 `/`。
pub fn https_root(url: &str) -> bool {
    let Some(prefix) = url.get(..6) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case("https:") {
        return false;
    }
    let Some(rest) = url[6..].strip_prefix("//") else {
        return false;
    };
    let (before_fragment, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (before_query, query) = before_fragment
        .split_once('?')
        .unwrap_or((before_fragment, ""));
    let (authority, path) = before_query
        .find('/')
        .map_or((before_query, ""), |index| before_query.split_at(index));
    if !fragment.is_empty() || !query.is_empty() || !(path.is_empty() || path == "/") {
        return false;
    }
    let host_port = match authority.rfind('@') {
        Some(index) => {
            let userinfo = &authority[..index];
            let (username, password) = userinfo.split_once(':').unwrap_or((userinfo, ""));
            if !username.is_empty() || !password.is_empty() {
                return false;
            }
            &authority[index + 1..]
        }
        None => authority,
    };
    let hostname = match host_port.strip_prefix('[') {
        Some(inner) => inner.split(']').next().unwrap_or_default(),
        None => host_port.split(':').next().unwrap_or_default(),
    };
    !hostname.is_empty()
}
