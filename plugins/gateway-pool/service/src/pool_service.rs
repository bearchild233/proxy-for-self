//! 池服务（网关目录与路由 Cookie）的访问，经内核 `http.fetch`。
//! 路径、查询参数、请求头、认证方式与旧网关 `transport/probe.rs` 的 `gateway_pool_get` 完全一致：
//! `GET {池地址去掉末尾斜杠}/{path}?<查询>`，`User-Agent: proxy-for-self`，`Authorization: Bearer <凭证>`，
//! 5 秒超时；只接受 200，响应超过大小上限视为失败。出口按设置 `poolDirect` 选直连或该账号出口。

use plugin_sdk::protocol::{Egress, HttpFetchParams, HttpMethod};
use serde_json::{Value, json};

use crate::{caps::Caps, routing::PoolSettings};

/// 取 Cookie 的响应上限（字节）。
const COOKIE_LIMIT: usize = 16384;
/// 取目录的响应上限（字节）。
const CATALOG_LIMIT: usize = 262_144;
/// 单次池服务请求的超时（毫秒）。
const TIMEOUT_MS: u64 = 5000;
/// 目录最多保留的网关数。
const CATALOG_ENTRIES: usize = 512;

/// 与 reqwest `.query()` 相同的 `application/x-www-form-urlencoded` 编码：
/// 字母数字与 `*-._` 原样保留，空格写作 `+`，其余按字节写成大写十六进制。
fn form_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'*' | b'-' | b'.' | b'_' => out.push(char::from(byte)),
            b' ' => out.push('+'),
            b if b.is_ascii_alphanumeric() => out.push(char::from(b)),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 构造请求（便于测试逐项核对）。
pub fn request(
    settings: &PoolSettings,
    account_id: &str,
    path: &str,
    query: &[(&str, &str)],
) -> HttpFetchParams {
    let query = query
        .iter()
        .map(|(name, value)| format!("{}={}", form_encode(name), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    HttpFetchParams {
        url: format!("{}/{path}?{query}", settings.pool_url.trim_end_matches('/')),
        method: HttpMethod::Get,
        headers: vec![
            ("user-agent".to_owned(), "proxy-for-self".to_owned()),
            (
                "authorization".to_owned(),
                format!("Bearer {}", settings.pool_token),
            ),
        ],
        body: None,
        egress: if settings.pool_direct {
            Egress::Direct
        } else {
            Egress::Account {
                account_id: account_id.to_owned(),
            }
        },
        timeout_ms: TIMEOUT_MS,
    }
}

async fn get(caps: &Caps, params: HttpFetchParams, limit: usize) -> Result<Value, ()> {
    let response = caps.fetch(params).await.map_err(|_| ())?;
    if response.status != 200 || response.truncated == Some(true) || response.body.len() > limit {
        return Err(());
    }
    serde_json::from_str(&response.body).map_err(|_| ())
}

/// 取路由 Cookie：`/cookie?account=<成员标识>&gateway=<网关>`。
pub async fn cookie(
    caps: &Caps,
    settings: &PoolSettings,
    account_id: &str,
    identity: &str,
    gateway: &str,
) -> Result<Value, ()> {
    get(
        caps,
        request(
            settings,
            account_id,
            "cookie",
            &[("account", identity), ("gateway", gateway)],
        ),
        COOKIE_LIMIT,
    )
    .await
}

/// 读取池服务目录，只保留可配对的网关；失败返回 None（控制面退回限定列表或上一份目录）。
pub async fn catalog(
    caps: &Caps,
    settings: &PoolSettings,
    account_id: &str,
    identity: &str,
) -> Option<Value> {
    let response = get(
        caps,
        request(settings, account_id, "gateways", &[("account", identity)]),
        CATALOG_LIMIT,
    )
    .await
    .ok()?;
    Some(normalize_catalog(&response))
}

/// 目录筛选：`pair_ready` 为真、网关名形如 `unified-<1~6 位数字>`、有效期有限且在 0..=86400 秒内；
/// 地区只接受两位大写字母；最多 512 项。
pub fn normalize_catalog(response: &Value) -> Value {
    let entries: Vec<Value> = response["gateways"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry["pair_ready"] == true)
        .filter_map(|entry| {
            let gateway = entry["name"].as_str()?;
            let valid = entry["valid_for_s"].as_f64()?;
            let country = entry["datacenter_country"]
                .as_str()
                .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()));
            (gateway.strip_prefix("unified-").is_some_and(|n| {
                !n.is_empty() && n.len() <= 6 && n.bytes().all(|b| b.is_ascii_digit())
            }) && valid.is_finite()
                && (0.0..=86400.0).contains(&valid))
            .then(|| json!({"gateway": gateway, "validForS": valid, "country": country}))
        })
        .take(CATALOG_ENTRIES)
        .collect();
    Value::Array(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_matches_reqwest_query_serialization() {
        assert_eq!(
            form_encode("gwpool-member:acct/user"),
            "gwpool-member%3Aacct%2Fuser"
        );
        assert_eq!(form_encode("a b*c._-"), "a+b*c._-");
        assert_eq!(form_encode("unified-142"), "unified-142");
        assert_eq!(form_encode("中"), "%E4%B8%AD");
    }
}
