//! 池 Cookie 工具：逐项对应旧网关 `gateway_pool.rs` 的 `claims`、`cookie_gateway`、`cookie_expiry`、
//! `routing_cookie`、`merge_cookie`。路由 Cookie `__oailb` 的第二段是 base64url 编码的 JSON，
//! 其中 `host` 指出网关、`exp` 是到期时间（Unix 秒）。

use std::collections::HashMap;

use serde_json::Value;

/// 合并后 Cookie 的长度上限（与旧实现一致）。
const COOKIE_LIMIT: usize = 16384;

/// base64url（无填充）解码；长度不合法或含非法字符时返回 None。
fn decode_base64url(input: &str) -> Option<Vec<u8>> {
    if input.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0_u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn claims(cookie: &str) -> Option<Value> {
    let token = cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix("__oailb="))?;
    let bytes = decode_base64url(token.split('.').nth(1)?.trim_end_matches('='))?;
    serde_json::from_slice(&bytes).ok()
}

/// Cookie 指向的网关名（`unified-<数字>`）。
pub fn cookie_gateway(cookie: &str) -> Option<String> {
    let value = claims(cookie)?;
    value["host"]
        .as_str()?
        .split('.')
        .find(|segment| {
            segment
                .strip_prefix("unified-")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// Cookie 的到期时间（Unix 秒）；已过期或缺少 `exp` 时返回 None。
pub fn cookie_expiry(cookie: &str, now: f64) -> Option<f64> {
    let expiry = claims(cookie)?["exp"].as_u64()? as f64;
    (expiry > now).then_some(expiry)
}

/// 池服务 `/cookie` 的响应转成路由 Cookie：优先用现成的 `cookie`，否则由两个 Cookie 值拼成。
pub fn routing_cookie(value: &Value) -> Option<String> {
    if let Some(cookie) = value["cookie"].as_str() {
        return merge_cookie(cookie, &[]);
    }
    let values = value.get("cookies").unwrap_or(value);
    let oailb = values["__oailb"]
        .as_str()
        .or_else(|| values["oailb"].as_str())?;
    let cflb = values["__cflb"]
        .as_str()
        .or_else(|| values["cflb"].as_str())?;
    merge_cookie(&format!("__oailb={oailb}; __cflb={cflb}"), &[])
}

/// 把响应里的 Set-Cookie 并入原 Cookie：只保留 `__oailb` 与 `__cflb`，后出现的覆盖先出现的；
/// 缺任何一个、超长或不是合法头值时返回 None。
pub fn merge_cookie(original: &str, updates: &[String]) -> Option<String> {
    let mut values: HashMap<&str, &str> = HashMap::new();
    for part in original.split(';').chain(
        updates
            .iter()
            .map(|update| update.split(';').next().unwrap_or_default()),
    ) {
        let Some((name, value)) = part.trim().split_once('=') else {
            continue;
        };
        if matches!(name, "__oailb" | "__cflb") {
            values.insert(name, value);
        }
    }
    let cookie = format!(
        "__oailb={}; __cflb={}",
        values.get("__oailb")?,
        values.get("__cflb")?
    );
    // 与 `HeaderValue::from_str` 的判定一致：控制字符与 DEL 不允许（制表符除外）。
    let valid = cookie.bytes().all(|b| (b >= 32 && b != 127) || b == b'\t');
    (cookie.len() <= COOKIE_LIMIT && valid).then_some(cookie)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成带 host 与 exp 的路由 Cookie（测试用，签名段不校验）。
    fn cookie(host: &str, exp: u64) -> String {
        let payload = format!("{{\"host\":\"{host}\",\"exp\":{exp}}}");
        format!(
            "__oailb=h.{}.s; __cflb=demo",
            encode_base64url(payload.as_bytes())
        )
    }

    fn encode_base64url(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let value = chunk
                .iter()
                .enumerate()
                .fold(0_u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..=chunk.len() {
                out.push(char::from(TABLE[((value >> (18 - 6 * i)) & 63) as usize]));
            }
        }
        out
    }

    #[test]
    fn gateway_and_expiry_come_from_the_routing_claims() {
        let header = cookie("unified-142.example.invalid", 2_000_000_000);
        assert_eq!(cookie_gateway(&header).as_deref(), Some("unified-142"));
        assert_eq!(cookie_expiry(&header, 1_000.0), Some(2_000_000_000.0));
        assert_eq!(cookie_expiry(&header, 3_000_000_000.0), None);
        assert_eq!(cookie_gateway("__oailb=bad; __cflb=x"), None);
        let other = cookie("edge.example.invalid", 2_000_000_000);
        assert_eq!(cookie_gateway(&other), None);
    }

    #[test]
    fn merge_keeps_only_routing_cookies_and_lets_updates_win() {
        let merged = merge_cookie(
            "__oailb=a; __cflb=b; other=1",
            &["__cflb=c; Path=/; HttpOnly".into(), "session=zzz".into()],
        );
        assert_eq!(merged.as_deref(), Some("__oailb=a; __cflb=c"));
        assert_eq!(merge_cookie("__oailb=a", &[]), None);
        assert_eq!(merge_cookie("__oailb=a\u{1}; __cflb=b", &[]), None);
        let long = format!("__oailb={}; __cflb=b", "x".repeat(COOKIE_LIMIT));
        assert_eq!(merge_cookie(&long, &[]), None);
    }

    #[test]
    fn pool_cookie_response_accepts_both_shapes() {
        let direct = serde_json::json!({"cookie": "__oailb=a; __cflb=b"});
        assert_eq!(
            routing_cookie(&direct).as_deref(),
            Some("__oailb=a; __cflb=b")
        );
        let pair = serde_json::json!({"cookies": {"oailb": "a", "__cflb": "b"}});
        assert_eq!(
            routing_cookie(&pair).as_deref(),
            Some("__oailb=a; __cflb=b")
        );
        assert_eq!(routing_cookie(&serde_json::json!({"cookies": {}})), None);
    }
}
