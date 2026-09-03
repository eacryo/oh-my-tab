//! 纯逻辑哈希工具:全项目唯一一份 FNV-1a 64 位实现。
//! 历史剪贴板图片去重、代码显示缓存键、非 bundle 应用的图标缓存键共用。
//!
//! Pure-logic hashing helper: the single FNV-1a 64-bit implementation for the whole
//! project. Shared by clipboard image dedup, code-display cache keys, and the icon
//! cache key of non-bundle apps.

/// FNV-1a 64 位哈希(字节流输入,调用方决定语义)。
/// FNV-1a 64-bit hash over a byte slice; callers own the interpretation.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// FNV-1a 64 位哈希的 16 位十六进制形式(字符串键 -> 文件名安全短键)。
/// FNV-1a 64-bit hash as 16-digit hex (string key -> filename-safe short key).
pub(crate) fn fnv1a64_hex(s: &str) -> String {
    format!("{:016x}", fnv1a64(s.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{fnv1a64, fnv1a64_hex};

    #[test]
    fn fnv1a64_known_vectors() {
        // FNV-1a 64 的公开测试向量。
        // Public FNV-1a 64 test vectors.
        assert_eq!(fnv1a64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x85944171f73967e8);
    }

    #[test]
    fn fnv1a64_hex_is_stable_and_distinct() {
        assert_eq!(
            fnv1a64_hex("/Applications/Safari.app/Contents/MacOS/Safari"),
            fnv1a64_hex("/Applications/Safari.app/Contents/MacOS/Safari")
        );
        assert_ne!(fnv1a64_hex(""), fnv1a64_hex("x"));
        assert_eq!(fnv1a64_hex("").len(), 16);
    }
}
