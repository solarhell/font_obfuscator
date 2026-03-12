use std::collections::HashSet;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Check if string contains whitespace
pub fn str_has_whitespace(s: &str) -> bool {
    s.contains(' ')
}

/// Check if string contains emoji characters
pub fn str_has_emoji(s: &str) -> bool {
    for c in s.chars() {
        // Check common emoji ranges
        let cp = c as u32;
        if matches!(cp,
            0x1F600..=0x1F64F |  // Emoticons
            0x1F300..=0x1F5FF |  // Misc Symbols and Pictographs
            0x1F680..=0x1F6FF |  // Transport and Map
            0x1F1E0..=0x1F1FF |  // Flags
            0x2600..=0x26FF   |  // Misc symbols
            0x2700..=0x27BF   |  // Dingbats
            0xFE00..=0xFE0F   |  // Variation Selectors
            0x1F900..=0x1F9FF |  // Supplemental Symbols
            0x1FA00..=0x1FA6F |  // Chess Symbols
            0x1FA70..=0x1FAFF |  // Symbols and Pictographs Extended-A
            0x200D            |  // Zero width joiner
            0x20E3            |  // Combining enclosing keycap
            0xE0020..=0xE007F    // Tags
        ) {
            return true;
        }
    }
    false
}

/// Remove duplicate characters while preserving order
pub fn deduplicate_str(s: &str) -> String {
    let mut seen = HashSet::new();
    s.chars().filter(|c| seen.insert(*c)).collect()
}

/// Encode a file as base64 string
pub fn base64_binary(path: &Path) -> std::io::Result<String> {
    let data = std::fs::read(path)?;
    Ok(STANDARD.encode(&data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_whitespace_detected() {
        assert!(str_has_whitespace("hello world"));
        assert!(str_has_whitespace(" leading"));
        assert!(str_has_whitespace("trailing "));
    }

    #[test]
    fn test_whitespace_absent() {
        assert!(!str_has_whitespace("helloworld"));
        assert!(!str_has_whitespace(""));
        assert!(!str_has_whitespace("你好世界"));
        // tabs and newlines are not spaces
        assert!(!str_has_whitespace("hello\tworld"));
        assert!(!str_has_whitespace("hello\nworld"));
    }

    #[test]
    fn test_emoji_detected() {
        assert!(str_has_emoji("hello😀"));
        assert!(str_has_emoji("🚀"));
        assert!(str_has_emoji("text🎉text"));
        assert!(str_has_emoji("☀")); // U+2600
        assert!(str_has_emoji("✂")); // U+2702 Dingbats
    }

    #[test]
    fn test_emoji_absent() {
        assert!(!str_has_emoji("hello"));
        assert!(!str_has_emoji("你好"));
        assert!(!str_has_emoji("0123456789"));
        assert!(!str_has_emoji(""));
        assert!(!str_has_emoji("abc!@#$%^&*()"));
    }

    #[test]
    fn test_deduplicate_ascii() {
        assert_eq!(deduplicate_str("aabbcc"), "abc");
        assert_eq!(deduplicate_str("abcabc"), "abc");
        assert_eq!(deduplicate_str("abcdef"), "abcdef");
    }

    #[test]
    fn test_deduplicate_cjk() {
        assert_eq!(deduplicate_str("真真假假"), "真假");
        assert_eq!(deduplicate_str("你好你好"), "你好");
    }

    #[test]
    fn test_deduplicate_preserves_order() {
        assert_eq!(deduplicate_str("cba"), "cba");
        assert_eq!(deduplicate_str("cbaabc"), "cba");
    }

    #[test]
    fn test_deduplicate_empty() {
        assert_eq!(deduplicate_str(""), "");
    }

    #[test]
    fn test_base64_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"hello world").unwrap();
        drop(f);

        let b64 = base64_binary(&path).unwrap();
        assert_eq!(b64, "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn test_base64_binary_nonexistent() {
        let result = base64_binary(Path::new("/nonexistent/file.bin"));
        assert!(result.is_err());
    }
}
