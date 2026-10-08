use provider_openai::transport::profile::cli_release::parse_cli_release;
use serde_json::json;

#[test]
fn cli_latest_requires_matching_stable_platform_dependencies() {
    let mut manifest =
        json!({"name":"@openai/codex", "version":"0.155.0", "optionalDependencies": {}});
    for target in [
        "darwin-arm64",
        "darwin-x64",
        "linux-arm64",
        "linux-x64",
        "win32-arm64",
        "win32-x64",
    ] {
        manifest["optionalDependencies"][format!("@openai/codex-{target}")] =
            json!(format!("npm:@openai/codex@0.155.0-{target}"));
    }
    assert_eq!(
        parse_cli_release(&serde_json::to_vec(&manifest).unwrap()).unwrap(),
        "0.155.0"
    );
    let mut missing = manifest.clone();
    missing["optionalDependencies"]
        .as_object_mut()
        .unwrap()
        .remove("@openai/codex-linux-arm64");
    assert!(parse_cli_release(&serde_json::to_vec(&missing).unwrap()).is_err());
    for version in ["0.155.0-alpha.1", "0.155.0+custom", "invalid"] {
        manifest["version"] = json!(version);
        assert!(parse_cli_release(&serde_json::to_vec(&manifest).unwrap()).is_err());
    }
}

#[tokio::test]
async fn locked_cli_auto_update_restores_verified_cache_without_changing_identity() {
    use chrono::Utc;
    use gateway_core::{
        account::OpaqueProviderData, provider_ports::ProviderArtifactProfile, routing::ProviderKind,
    };
    use provider_openai::transport::profile::{
        CodexWireProfile, CodexWireProfileState, cli_release::CliReleaseService,
        selection::ClientKind,
    };
    use std::sync::Arc;

    for (automatic, cached, expected) in [
        (true, "0.162.0", "0.162.0"),
        (false, "0.162.0", "0.161.0"),
        (true, "0.160.0", "0.161.0"),
        (true, "0.163.0-alpha.1", "0.161.0"),
    ] {
        let base = CodexWireProfile {
            client_kind: ClientKind::Cli,
            originator: "codex_exec".to_owned(),
            codex_version: "0.161.0".to_owned(),
            terminal: "dumb (codex_exec; 0.161.0)".to_owned(),
            ..Default::default()
        };
        let state = CodexWireProfileState::new(base.clone())
            .with_locked_identity(true)
            .with_cli_auto_update(automatic);
        let cache = Arc::new(super::ArtifactProfiles::default());
        let minor = cached.split('.').nth(1).unwrap().parse::<u64>().unwrap();
        *cache.profile.lock().unwrap() = Some(ProviderArtifactProfile::new(
            ProviderKind::new("openai").unwrap(),
            "cli-linux-x64".to_owned(),
            1 + minor * 1000,
            Utc::now().into(),
            OpaqueProviderData::new(json!({"version":cached}).as_object().unwrap().clone()),
        ));
        CliReleaseService::new(
            ProviderKind::new("openai").unwrap(),
            state.clone(),
            cache.clone(),
        )
        .unwrap()
        .restore()
        .await;
        let current = state.snapshot();
        assert_eq!(current.codex_version, expected);
        assert_eq!(current.terminal, format!("dumb (codex_exec; {expected})"));
        assert_eq!(current.originator, base.originator);
        assert_eq!(current.os_type, base.os_type);
        assert_eq!(current.arch, base.arch);
        // 后续读取不到缓存时保留已核验版本。
        *cache.profile.lock().unwrap() = None;
        CliReleaseService::new(ProviderKind::new("openai").unwrap(), state.clone(), cache)
            .unwrap()
            .restore()
            .await;
        assert_eq!(state.snapshot(), current);
    }
}
