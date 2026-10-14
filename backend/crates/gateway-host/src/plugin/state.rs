use super::{registry::PluginRegistration, transport::Peer};
use kernel_protocol::PluginApi;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::SystemTime,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disabled,
    Connecting,
    Available,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct PluginSnapshot {
    pub id: String,
    pub state: ConnectionState,
    pub version: Option<String>,
    pub kernel_api: Option<PluginApi>,
    pub granted_capabilities: Vec<String>,
    pub owned_groups: Vec<String>,
    pub watched_groups: Vec<String>,
    pub peer_uid: Option<u32>,
    pub last_error: Option<String>,
    pub last_error_at: Option<SystemTime>,
}

#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    pub plugins: Vec<PluginSnapshot>,
    pub cache_error: Option<String>,
    pub registry_error: Option<String>,
    pub registry_error_at: Option<SystemTime>,
}

pub(super) struct Entry {
    pub registration: PluginRegistration,
    pub snapshot: PluginSnapshot,
    pub peer: Option<Arc<Peer>>,
}

impl Entry {
    pub fn new(registration: PluginRegistration, owned_groups: Vec<String>) -> Self {
        Self {
            snapshot: PluginSnapshot {
                id: registration.id.clone(),
                state: if registration.enabled {
                    ConnectionState::Connecting
                } else {
                    ConnectionState::Disabled
                },
                version: None,
                kernel_api: None,
                granted_capabilities: vec![],
                owned_groups,
                watched_groups: vec![],
                peer_uid: None,
                last_error: None,
                last_error_at: None,
            },
            registration,
            peer: None,
        }
    }
    pub fn error(&mut self, error: impl ToString) {
        let error = error.to_string();
        tracing::warn!(
            plugin_id = self.registration.id,
            error,
            "plugin runtime error"
        );
        self.snapshot.last_error = Some(error);
        self.snapshot.last_error_at = Some(SystemTime::now());
    }
}

pub(super) struct State {
    pub entries: BTreeMap<String, Entry>,
    pub cache_error: Option<String>,
    pub registry_error: Option<String>,
    pub registry_error_at: Option<SystemTime>,
    pub cache: PathBuf,
}

impl State {
    pub fn registry_error(&mut self, error: String) {
        tracing::error!(error, "plugin registry rejected; keeping previous registry");
        self.registry_error = Some(error);
        self.registry_error_at = Some(SystemTime::now());
    }

    pub fn ownership(&mut self, id: &str, groups: Vec<String>, watched: Vec<String>) {
        let others: Vec<_> = self
            .entries
            .iter()
            .filter(|(other, _)| other.as_str() != id)
            .collect();
        let owned: BTreeSet<_> = others
            .iter()
            .flat_map(|(_, entry)| entry.snapshot.owned_groups.iter().cloned())
            .collect();
        let observed: BTreeSet<_> = others
            .iter()
            .flat_map(|(_, entry)| entry.snapshot.watched_groups.iter().cloned())
            .collect();
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        let mut accepted = BTreeSet::new();
        for group in groups {
            if owned.contains(&group) || observed.contains(&group) {
                entry.error(format!("group ownership conflict: {group}"));
            } else {
                accepted.insert(group);
            }
        }
        entry.snapshot.owned_groups = accepted.into_iter().collect();
        let mut accepted = BTreeSet::new();
        for group in watched {
            if owned.contains(&group) {
                entry.error(format!("group ownership conflict: {group}"));
            } else {
                accepted.insert(group);
            }
        }
        entry.snapshot.watched_groups = accepted.into_iter().collect();
        let groups = entry.snapshot.owned_groups.clone();
        let watched = entry.snapshot.watched_groups.clone();
        if let Err(error) = update_cache(&self.cache, |cache| {
            cache.plugins.insert(id.into(), groups);
            cache.watched_groups.insert(id.into(), watched);
        }) {
            self.entries
                .get_mut(id)
                .expect("entry")
                .error(format!("ownership cache: {error}"));
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
pub(super) struct OwnershipCache {
    pub plugins: BTreeMap<String, Vec<String>>,
    #[serde(default, rename = "watchedGroups")]
    pub watched_groups: BTreeMap<String, Vec<String>>,
}

pub(super) fn read_cache(path: &Path) -> io::Result<OwnershipCache> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(OwnershipCache::default()),
        Err(e) => Err(e),
    }
}

pub(super) fn update_cache(
    path: &Path,
    update: impl FnOnce(&mut OwnershipCache),
) -> io::Result<()> {
    // A/B 槽共用缓存：锁住独立锁文件后读改写，不能用过期的进程快照覆盖另一个槽。
    let lock_path = path.with_extension("json.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o640)
        .open(lock_path)?;
    lock.lock()?;
    let mut cache = read_cache(path)?;
    update(&mut cache);
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o640)
        .open(&temporary)?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(0o640))?;
        file.write_all(&serde_json::to_vec(&cache)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}
