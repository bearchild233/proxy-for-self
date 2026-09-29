//! 单副本网关的凭据静态保护；先初始化主密钥再发布任何运行快照。
use crate::{StoreError, StoreResult};
use aws_lc_rs::{
    aead, hmac,
    rand::{SecureRandom, SystemRandom},
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::OnceLock};
use zeroize::Zeroizing;

const PREFIX: &str = "hubenc1:";
const JSON_FIELD: &str = "_api_hub_vault_v1";
static VAULT: OnceLock<Vault> = OnceLock::new();

struct Vault {
    encryption: aead::LessSafeKey,
    lookup: hmac::Key,
}

fn failure() -> StoreError {
    StoreError::InvalidData {
        entity: "credential vault",
        message: "credential encryption or authentication failed".to_owned(),
    }
}

impl Vault {
    fn new(master: &[u8]) -> StoreResult<Self> {
        if master.len() != 32 {
            return Err(failure());
        }
        let root = hmac::Key::new(hmac::HMAC_SHA256, master);
        let encryption = hmac::sign(&root, b"api-hub/encryption/v1");
        let lookup = hmac::sign(&root, b"api-hub/key-lookup/v1");
        Ok(Self {
            encryption: aead::LessSafeKey::new(
                aead::UnboundKey::new(&aead::AES_256_GCM, encryption.as_ref())
                    .map_err(|_| failure())?,
            ),
            lookup: hmac::Key::new(hmac::HMAC_SHA256, lookup.as_ref()),
        })
    }
    fn seal(&self, aad: &str, bytes: &[u8]) -> StoreResult<String> {
        let mut nonce = [0_u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| failure())?;
        let mut encrypted = Zeroizing::new(bytes.to_vec());
        self.encryption
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad.as_bytes()),
                &mut *encrypted,
            )
            .map_err(|_| failure())?;
        Ok(format!(
            "{PREFIX}{}{}",
            hex::encode(nonce),
            hex::encode(&*encrypted)
        ))
    }
    fn open(&self, aad: &str, value: &str) -> StoreResult<Zeroizing<Vec<u8>>> {
        let value = value.strip_prefix(PREFIX).ok_or_else(failure)?;
        let mut bytes = Zeroizing::new(hex::decode(value).map_err(|_| failure())?);
        if bytes.len() < 28 {
            return Err(failure());
        }
        let nonce: [u8; 12] = bytes[..12].try_into().map_err(|_| failure())?;
        let plain = self
            .encryption
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(aad.as_bytes()),
                &mut bytes[12..],
            )
            .map_err(|_| failure())?;
        Ok(Zeroizing::new(plain.to_vec()))
    }
}

pub(crate) fn initialize(path: &Path) -> StoreResult<()> {
    if !path.is_absolute() || VAULT.get().is_some() {
        return Err(failure());
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| failure())?;
    if !metadata.is_file() || metadata.len() > 4096 {
        return Err(failure());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(failure());
        }
    }
    let encoded = SecretString::from(std::fs::read_to_string(path).map_err(|_| failure())?);
    let master =
        Zeroizing::new(hex::decode(encoded.expose_secret().trim()).map_err(|_| failure())?);
    VAULT.set(Vault::new(&master)?).map_err(|_| failure())
}

pub(crate) fn seal_json(id: &str, value: &Value) -> StoreResult<Value> {
    let Some(vault) = VAULT.get() else {
        return Ok(value.clone());
    };
    let bytes = Zeroizing::new(serde_json::to_vec(value).map_err(|_| failure())?);
    Ok(json!({(JSON_FIELD): vault.seal(&format!("account:{id}"), &bytes)?}))
}
pub(crate) fn open_json(id: &str, value: Value) -> StoreResult<Value> {
    let Some(vault) = VAULT.get() else {
        return if value.get(JSON_FIELD).is_some() {
            Err(failure())
        } else {
            Ok(value)
        };
    };
    let stored = value
        .get(JSON_FIELD)
        .and_then(Value::as_str)
        .ok_or_else(failure)?;
    serde_json::from_slice(&vault.open(&format!("account:{id}"), stored)?).map_err(|_| failure())
}
pub(crate) fn seal_key(id: &str, key: &str) -> StoreResult<String> {
    VAULT.get().map_or_else(
        || Ok(key.to_owned()),
        |vault| vault.seal(&format!("client-key:{id}"), key.as_bytes()),
    )
}
pub(crate) fn open_key(id: &str, key: &str) -> StoreResult<String> {
    let Some(vault) = VAULT.get() else {
        return if key.starts_with(PREFIX) {
            Err(failure())
        } else {
            Ok(key.to_owned())
        };
    };
    String::from_utf8(vault.open(&format!("client-key:{id}"), key)?.to_vec()).map_err(|_| failure())
}
pub(crate) fn key_hash(key: &str) -> String {
    VAULT.get().map_or_else(
        || hex::encode(Sha256::digest(key.as_bytes())),
        |vault| hex::encode(hmac::sign(&vault.lookup, key.as_bytes()).as_ref()),
    )
}
pub(crate) fn key_prefix(key: &str) -> &str {
    &key[..10.min(key.len() / 2)]
}

pub(crate) async fn migrate_existing(pool: &sqlx::PgPool) -> StoreResult<()> {
    let mut transaction = pool.begin().await.map_err(|_| failure())?;
    sqlx::query("select pg_advisory_xact_lock(727487219)")
        .execute(&mut *transaction)
        .await
        .map_err(|_| failure())?;
    let accounts = sqlx::query_as::<_, (String, Value)>(
        "select id, provider_credentials_json from provider_accounts for update",
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|_| failure())?;
    for (id, value) in accounts {
        if value.get(JSON_FIELD).is_some() {
            open_json(&id, value)?;
            continue;
        }
        sqlx::query("update provider_accounts set provider_credentials_json=$2 where id=$1")
            .bind(&id)
            .bind(seal_json(&id, &value)?)
            .execute(&mut *transaction)
            .await
            .map_err(|_| failure())?;
    }
    let keys =
        sqlx::query_as::<_, (String, String)>("select id,key from client_api_keys for update")
            .fetch_all(&mut *transaction)
            .await
            .map_err(|_| failure())?;
    for (id, stored) in keys {
        let key = SecretString::from(if stored.starts_with(PREFIX) {
            open_key(&id, &stored)?
        } else {
            stored
        });
        let plain = key.expose_secret();
        sqlx::query(
            "update client_api_keys set key=$2,key_lookup_hash=$3,key_prefix=$4 where id=$1",
        )
        .bind(&id)
        .bind(seal_key(&id, plain)?)
        .bind(key_hash(plain))
        .bind(key_prefix(plain))
        .execute(&mut *transaction)
        .await
        .map_err(|_| failure())?;
    }
    transaction.commit().await.map_err(|_| failure())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_encryption_is_randomized_and_scope_bound() {
        let vault = Vault::new(&[7; 32]).unwrap();
        let a = vault.seal("account:a", b"test-secret").unwrap();
        let b = vault.seal("account:a", b"test-secret").unwrap();
        assert_ne!(a, b);
        assert_eq!(&*vault.open("account:a", &a).unwrap(), b"test-secret");
        assert!(vault.open("account:b", &a).is_err());
        assert!(vault.open("client-key:a", &a).is_err());
        assert!(Vault::new(&[8; 32]).unwrap().open("account:a", &a).is_err());
    }
    #[test]
    fn tampering_truncation_and_cleartext_are_rejected() {
        let vault = Vault::new(&[7; 32]).unwrap();
        let value = vault.seal("account:a", b"secret").unwrap();
        let mut changed = value.clone();
        let last = changed.pop().unwrap();
        changed.push(if last == '0' { '1' } else { '0' });
        for bad in [
            changed,
            value[..value.len() - 2].to_owned(),
            "secret".to_owned(),
            "hubenc1:00".to_owned(),
        ] {
            assert!(vault.open("account:a", &bad).is_err());
        }
    }
}
