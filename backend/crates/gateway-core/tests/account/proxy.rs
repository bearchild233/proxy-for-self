use gateway_core::account::OutboundProxy;

#[test]
fn proxy_endpoints_support_explicit_schemes_and_redact_credentials() {
    for scheme in ["http", "https", "socks5", "socks5h"] {
        let proxy =
            OutboundProxy::parse(&format!("{scheme}://user:p%40ss%3Aword@[::1]:1080")).unwrap();
        assert!(proxy.expose_url().contains("p%40ss%3Aword"));
        assert!(!proxy.endpoint().contains("user"));
        assert!(!proxy.endpoint().contains("p%40ss"));
        assert!(!format!("{proxy:?}").contains("user"));
        assert!(proxy.endpoint().contains("[::1]:1080"));
    }
}

#[test]
fn invalid_proxy_never_silently_becomes_direct() {
    for url in [
        "",
        "localhost:1080",
        "file:///etc/passwd",
        "ftp://host:21",
        "http://host:0",
        "socks5://host",
        "http://host:80/path",
        "http://host:80?secret=1",
        "http://host:80#fragment",
        "http://user:secret@host:80\n",
    ] {
        let error = OutboundProxy::parse(url).unwrap_err();
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn tunnel_connections_use_local_bridge_and_redact_all_node_secrets() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    for source in [
        "vless://bf000d23-0752-40b4-affe-68f7707a9661@node.example:443?security=reality&sni=example.com&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=aa#note",
        "hy2://private-password@node.example?obfs=salamander&obfs-password=secret",
    ] {
        let proxy = OutboundProxy::parse(source).unwrap();
        let local = url::Url::parse(&proxy.transport_url()).unwrap();
        assert_eq!(local.host_str(), Some("127.0.0.1"));
        assert_eq!(local.port(), Some(18323));
        assert_eq!(
            URL_SAFE_NO_PAD.decode(local.username()).unwrap(),
            proxy.expose_url().as_bytes()
        );
        let public = proxy.endpoint();
        assert!(!public.contains('@'));
        assert!(!public.contains('?'));
        assert!(!public.contains('#'));
        assert!(!format!("{proxy:?}").contains("secret"));
    }
    let normal = OutboundProxy::parse("socks5h://user:pass@localhost:1080").unwrap();
    assert_eq!(normal.transport_url(), normal.expose_url());
}

#[test]
fn unsupported_tunnel_options_fail_without_changing_semantics() {
    for source in [
        "vless://not-a-uuid@host:443",
        "vless://bf000d23-0752-40b4-affe-68f7707a9661@host:443?type=grpc",
        "vless://bf000d23-0752-40b4-affe-68f7707a9661@host:443?security=reality",
        "vless://bf000d23-0752-40b4-affe-68f7707a9661@host:443?type=ws&flow=xtls-rprx-vision",
        "hysteria2://pass@host:443?insecure=maybe",
        "hysteria2://pass@host:443?obfs=salamander",
        "hysteria2://pass@host:443?mport=1234",
        "hysteria2://pass@host:443?sni=a&sni=b",
    ] {
        assert!(OutboundProxy::parse(source).is_err());
    }
}

#[test]
fn vless_udp_flags_accept_boolean_values_without_changing_tcp_transport() {
    let prefix = "vless://bf000d23-0752-40b4-affe-68f7707a9661@node.example:443?";
    for flag in ["true", "false", "1", "0"] {
        let proxy = OutboundProxy::parse(&format!("{prefix}udp={flag}")).unwrap();
        assert!(proxy.transport_url().ends_with(":node@127.0.0.1:18323"));
    }
    for flag in ["maybe", "", "true&udp=false"] {
        assert!(OutboundProxy::parse(&format!("{prefix}udp={flag}")).is_err());
    }
}
