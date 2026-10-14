use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginRuntimeConfig {
    pub registry_path: Option<PathBuf>,
    pub ownership_cache_path: Option<PathBuf>,
}

impl PluginRuntimeConfig {
    pub fn resolve_and_validate(&mut self, directory: &Path) -> Result<(), ConfigError> {
        // 两个路径构成完整运行时配置，不推断缺失路径。
        match (&mut self.registry_path, &mut self.ownership_cache_path) {
            (None, None) => Ok(()),
            (Some(registry), Some(cache)) => {
                for path in [registry, cache] {
                    if path.as_os_str().is_empty() {
                        return Err(ConfigError::InvalidField("plugins"));
                    }
                    if path.is_relative() {
                        *path = directory.join(&*path);
                    }
                }
                Ok(())
            }
            _ => Err(ConfigError::InvalidField("plugins")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistration {
    pub id: String,
    pub enabled: bool,
    pub socket: PathBuf,
    pub grants: Vec<String>,
    #[serde(default)]
    pub uid: Option<u32>,
    #[serde(default, rename = "fetchHosts")]
    pub fetch_hosts: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginRegistry {
    pub plugins: Vec<PluginRegistration>,
}

impl PluginRegistry {
    pub fn load(path: &Path) -> Result<Self, String> {
        let registry: Self =
            serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let mut ids = BTreeSet::new();
        for plugin in &registry.plugins {
            if plugin.id.is_empty() || !ids.insert(&plugin.id) {
                return Err("empty or duplicate plugin ID".into());
            }
            if !plugin.socket.is_absolute() {
                return Err("plugin socket must be absolute".into());
            }
            if plugin.grants.iter().any(|grant| {
                !matches!(
                    grant.as_str(),
                    "log"
                        | "upstream.probe"
                        | "http.fetch"
                        | "accounts.read"
                        | "groups.move"
                        | "quota.read"
                )
            }) {
                return Err("unknown plugin capability".into());
            }
        }
        Ok(registry)
    }
}
