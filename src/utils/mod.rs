//! Utility modules for common functionality

pub mod retry;

pub use retry::{retry, retry_with_check, RetryConfig, RetryableError};

/// Truncate a string to at most `max_bytes` bytes, ensuring the cut lands on a
/// valid UTF-8 char boundary. Returns the longest prefix that fits.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_str_ascii() {
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_multibyte_boundary() {
        // █ is U+2588, 3 bytes in UTF-8
        let s = "abc█def";
        // "abc" = 3 bytes, "█" = bytes 3..6, "def" = bytes 6..9
        assert_eq!(truncate_str(s, 3), "abc"); // exact boundary before █
        assert_eq!(truncate_str(s, 4), "abc"); // inside █, backs up to 3
        assert_eq!(truncate_str(s, 5), "abc"); // inside █, backs up to 3
        assert_eq!(truncate_str(s, 6), "abc█"); // exact boundary after █
    }

    #[test]
    fn test_truncate_str_emoji() {
        // 🦀 is U+1F980, 4 bytes in UTF-8
        let s = "hi🦀bye";
        // "hi" = 2 bytes, "🦀" = bytes 2..6, "bye" = bytes 6..9
        assert_eq!(truncate_str(s, 2), "hi");
        assert_eq!(truncate_str(s, 3), "hi"); // inside 🦀
        assert_eq!(truncate_str(s, 5), "hi"); // inside 🦀
        assert_eq!(truncate_str(s, 6), "hi🦀");
    }

    #[test]
    fn test_truncate_str_zero() {
        assert_eq!(truncate_str("hello", 0), "");
        assert_eq!(truncate_str("🦀", 0), "");
    }

    #[test]
    fn test_truncate_str_empty() {
        assert_eq!(truncate_str("", 5), "");
        assert_eq!(truncate_str("", 0), "");
    }

    #[test]
    fn test_truncate_str_all_multibyte() {
        // Each char is 3 bytes
        let s = "███"; // 9 bytes
        assert_eq!(truncate_str(s, 1), ""); // inside first █
        assert_eq!(truncate_str(s, 3), "█");
        assert_eq!(truncate_str(s, 7), "██"); // inside third █
        assert_eq!(truncate_str(s, 9), "███");
    }
}
