//! 租约、代次与分组事件的标识：与 Python `uuid.uuid4().hex` 同形（32 位小写十六进制），
//! 测试与一致性对照可以替换为确定的序列。

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub type IdSource = Arc<dyn Fn() -> String + Send + Sync>;

/// 生产用标识：来自系统随机源（`/dev/urandom`），版本位与变体位按 UUID v4 设置。
pub fn random_ids() -> IdSource {
    Arc::new(random_id)
}

/// 确定的序列（`000…001`、`000…002`…），用于测试与对照：Python 侧把 `uuid.uuid4` 替换为相同序列。
pub fn sequential_ids() -> IdSource {
    let counter = AtomicU64::new(0);
    Arc::new(move || format!("{:032x}", counter.fetch_add(1, Ordering::SeqCst) + 1))
}

fn random_id() -> String {
    let mut bytes = [0_u8; 16];
    if !fill_from_system(&mut bytes) {
        fill_from_hasher(&mut bytes);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn fill_from_system(bytes: &mut [u8; 16]) -> bool {
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes))
        .is_ok()
}

/// 系统随机源不可用时的后备：`RandomState` 每个实例带进程随机的密钥，再混入计数器。
fn fill_from_hasher(bytes: &mut [u8; 16]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    for chunk in bytes.chunks_mut(8) {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::SeqCst));
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_have_the_uuid_hex_shape() {
        let source = random_ids();
        let (first, second) = (source(), source());
        assert_ne!(first, second);
        for id in [&first, &second] {
            assert_eq!(id.len(), 32);
            assert!(
                id.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            );
            assert_eq!(id.as_bytes()[12], b'4');
        }
    }

    #[test]
    fn sequential_identifiers_are_stable() {
        let source = sequential_ids();
        assert_eq!(source(), format!("{:032x}", 1));
        assert_eq!(source(), format!("{:032x}", 2));
    }
}
