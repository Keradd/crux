//! Token estimation.
//!
//! Strategy: rough `chars / 4` heuristic, the same shortcut rtk and alex
//! use. It's deliberately wrong-but-cheap; precise tokenization would
//! require shipping a tokenizer per model and is a feature for later.
//!
//! All Layer telemetry uses this estimator so numbers are comparable.

/// Estimate token count for a UTF-8 string.
///
/// Uses `chars().count() / 4` rounded up. Empty input returns 0.
/// Unicode characters count as one (matches BPE-ish behavior on prose).
pub fn estimate(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

/// Estimate from a byte length (when we don't have the full text in
/// memory, e.g., for large files where stat suffices).
pub fn estimate_from_bytes(bytes: u64) -> u64 {
    if bytes == 0 {
        return 0;
    }
    bytes.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate_from_bytes(0), 0);
    }

    #[test]
    fn rounds_up() {
        // 1 char → 1 token, 4 chars → 1 token, 5 chars → 2 tokens
        assert_eq!(estimate("a"), 1);
        assert_eq!(estimate("abcd"), 1);
        assert_eq!(estimate("abcde"), 2);
    }

    #[test]
    fn counts_unicode_codepoints() {
        // 4 bytes for "ñ" but 1 char.
        assert_eq!(estimate("ññññ"), 1);
    }
}
