//! Explicit account egress. Credentials never appear in Debug or ordinary admin projections.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::fmt;
use url::Url;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct OutboundProxy(Url);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "invalid outbound proxy; expected HTTP(S), SOCKS5(H), VLESS or Hysteria2 with valid connection parameters"
)]
pub struct InvalidOutboundProxy;

impl OutboundProxy {
    pub fn parse(value: &str) -> Result<Self, InvalidOutboundProxy> {
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(InvalidOutboundProxy);
        }
        let mut url = Url::parse(value).map_err(|_| InvalidOutboundProxy)?;
        if matches!(url.scheme(), "vless" | "hysteria2" | "hy2") {
            if url.scheme() == "hy2" {
                url.set_scheme("hysteria2")
                    .map_err(|_| InvalidOutboundProxy)?;
            }
            if url.port().is_none() && url.scheme() == "hysteria2" {
                url.set_port(Some(443)).map_err(|_| InvalidOutboundProxy)?;
            }
            if url.host_str().is_none()
                || url.port().is_none_or(|port| port == 0)
                || url.username().is_empty()
                || !matches!(url.path(), "" | "/")
            {
                return Err(InvalidOutboundProxy);
            }
            validate_tunnel(&url)?;
            // 分享链接备注不属于连接身份，避免备注变化产生重复连接池。
            url.set_fragment(None);
            return Ok(Self(url));
        }
        if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
            || url.host_str().is_none()
            || url.port_or_known_default().is_none_or(|port| port == 0)
            || !matches!(url.path(), "" | "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(InvalidOutboundProxy);
        }
        Ok(Self(url))
    }

    #[must_use]
    pub fn expose_url(&self) -> &str {
        self.0.as_str()
    }

    /// 新协议通过本机受管桥接进程连接；桥接不可用时连接失败，绝不回退直连。
    /// 用户名是节点 URI 的 base64url 编码，仍属于秘密；仅发送到回环地址。
    #[must_use]
    pub fn transport_url(&self) -> String {
        if matches!(self.0.scheme(), "vless" | "hysteria2") {
            format!(
                "http://{}:node@127.0.0.1:18323",
                URL_SAFE_NO_PAD.encode(self.expose_url())
            )
        } else {
            self.expose_url().to_owned()
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> String {
        let mut url = self.0.clone();
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        url.to_string()
    }
}

fn validate_tunnel(url: &Url) -> Result<(), InvalidOutboundProxy> {
    let mut query = std::collections::BTreeMap::new();
    for (key, value) in url.query_pairs() {
        if value.chars().any(char::is_control) || query.insert(key, value).is_some() {
            return Err(InvalidOutboundProxy);
        }
    }
    let value = |key: &str, fallback: &str| {
        query
            .get(key)
            .map_or_else(|| fallback.to_owned(), |v| v.to_string())
    };
    let allowed: &[&str] = if url.scheme() == "vless" {
        if uuid::Uuid::parse_str(url.username()).is_err()
            || url.password().is_some()
            || !matches!(value("type", "tcp").as_str(), "tcp" | "ws")
            || !matches!(
                value("security", "tls").as_str(),
                "none" | "tls" | "reality"
            )
            || value("encryption", "none") != "none"
            || !matches!(value("flow", "").as_str(), "" | "xtls-rprx-vision")
            || (value("type", "tcp") == "ws"
                && (value("security", "tls") == "reality" || !value("flow", "").is_empty()))
            || (value("flow", "") == "xtls-rprx-vision" && value("security", "tls") == "none")
            || (value("security", "tls") == "reality"
                && (value("pbk", "").is_empty() || value("sni", "").is_empty()))
        {
            return Err(InvalidOutboundProxy);
        }
        &[
            "type",
            "security",
            "encryption",
            "flow",
            "sni",
            "fp",
            "pbk",
            "sid",
            "path",
            "host",
            "alpn",
            "insecure",
            "allowInsecure",
            "udp",
        ]
    } else {
        if !matches!(value("obfs", "").as_str(), "" | "salamander")
            || (value("obfs", "") == "salamander" && value("obfs-password", "").is_empty())
        {
            return Err(InvalidOutboundProxy);
        }
        &["sni", "insecure", "obfs", "obfs-password", "alpn"]
    };
    if query.keys().any(|key| !allowed.contains(&key.as_ref()))
        || ["insecure", "allowInsecure", "udp"]
            .iter()
            .any(|key| !matches!(value(key, "0").as_str(), "0" | "1" | "false" | "true"))
    {
        return Err(InvalidOutboundProxy);
    }
    Ok(())
}

impl fmt::Debug for OutboundProxy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OutboundProxy(<redacted>)")
    }
}
