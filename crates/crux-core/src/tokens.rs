pub fn estimate(text: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

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
        assert_eq!(estimate("a"), 1);
        assert_eq!(estimate("abcd"), 1);
        assert_eq!(estimate("abcde"), 2);
    }

    #[test]
    fn counts_unicode_codepoints() {
        assert_eq!(estimate("ññññ"), 1);
    }
}
