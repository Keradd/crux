#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleId {
    DecorativeBanner,
    LongModuleDoc,
    GoalSection,
    PublicSurfaceSection,
    PatternAdaptedFrom,
    LayerLabel,
    MarketingPhrase,
}

impl RuleId {
    pub fn as_str(&self) -> &'static str {
        match self {
            RuleId::DecorativeBanner => "decorative-banner",
            RuleId::LongModuleDoc => "long-module-doc",
            RuleId::GoalSection => "goal-section",
            RuleId::PublicSurfaceSection => "public-surface-section",
            RuleId::PatternAdaptedFrom => "pattern-adapted-from",
            RuleId::LayerLabel => "layer-label",
            RuleId::MarketingPhrase => "marketing-phrase",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HygieneRule {
    pub id: RuleId,
    pub label: &'static str,
    pub reason: &'static str,
}

pub const RULES: &[HygieneRule] = &[
    HygieneRule {
        id: RuleId::DecorativeBanner,
        label: "decorative-banner",
        reason: "decorative separator comment wastes context",
    },
    HygieneRule {
        id: RuleId::LongModuleDoc,
        label: "long-module-doc",
        reason: "module doc comment exceeds threshold",
    },
    HygieneRule {
        id: RuleId::GoalSection,
        label: "goal-section",
        reason: "`Goal:` block belongs in README, not source",
    },
    HygieneRule {
        id: RuleId::PublicSurfaceSection,
        label: "public-surface-section",
        reason: "`Public surface:` repeats `pub use` items",
    },
    HygieneRule {
        id: RuleId::PatternAdaptedFrom,
        label: "pattern-adapted-from",
        reason: "`Pattern adapted from` reference belongs in commit message",
    },
    HygieneRule {
        id: RuleId::LayerLabel,
        label: "layer-label",
        reason: "`Layer N` label is documented in ARCHITECTURE.md",
    },
    HygieneRule {
        id: RuleId::MarketingPhrase,
        label: "marketing-phrase",
        reason: "marketing phrase has no technical content",
    },
];

pub const MARKETING_PHRASES: &[&str] = &[
    "revolutionary",
    "cutting-edge",
    "cutting edge",
    "seamlessly",
    "seamless integration",
    "robust and scalable",
    "highly scalable",
    "state-of-the-art",
    "state of the art",
    "groundbreaking",
    "best-in-class",
    "best in class",
    "world-class",
    "world class",
    "next-generation",
    "next generation",
    "game-changing",
    "game changing",
    "synergistic",
    "paradigm shift",
    "paradigm-shifting",
];

pub const SECTION_HEADERS_GOAL: &[&str] = &["Goal:", "Goals:", "## Goal", "## Goals"];

pub const SECTION_HEADERS_PUBLIC_SURFACE: &[&str] = &[
    "Public surface:",
    "Public Surface:",
    "Public API:",
    "## Public surface",
    "## Public Surface",
];

pub const PATTERN_ADAPTED_FROM_NEEDLES: &[&str] = &[
    "Pattern adapted from",
    "pattern adapted from",
    "Adapted from pattern",
    "Inspired by pattern",
];

pub fn is_decorative_banner(content: &str, min_run: usize) -> bool {
    let trimmed = content.trim();
    if trimmed.chars().count() < min_run {
        return false;
    }
    let first = match trimmed.chars().next() {
        Some(c) => c,
        None => return false,
    };
    if !is_banner_char(first) {
        return false;
    }
    trimmed.chars().all(|c| c == first || c == ' ' || c == '\t')
}

fn is_banner_char(c: char) -> bool {
    matches!(
        c,
        '─' | '═'
            | '-'
            | '='
            | '*'
            | '#'
            | '~'
            | '_'
            | '·'
            | '━'
            | '┄'
            | '┈'
            | '╌'
            | '╍'
            | '┉'
            | '⎯'
    )
}

pub fn is_goal_header(content: &str) -> bool {
    let t = content.trim_start_matches(['#', ' ', '\t']).trim();
    let lower = t.to_ascii_lowercase();
    lower.starts_with("goal:") || lower.starts_with("goals:") || lower == "goal" || lower == "goals"
}

pub fn is_public_surface_header(content: &str) -> bool {
    let t = content.trim_start_matches(['#', ' ', '\t']).trim();
    let lower = t.to_ascii_lowercase();
    lower.starts_with("public surface:")
        || lower.starts_with("public surface")
        || lower.starts_with("public api:")
}

pub fn has_pattern_adapted_from(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    PATTERN_ADAPTED_FROM_NEEDLES
        .iter()
        .any(|n| lower.contains(&n.to_ascii_lowercase()))
}

pub fn has_layer_label(content: &str) -> bool {
    let bytes = content.as_bytes();
    let lower: Vec<u8> = bytes.iter().map(|b| b.to_ascii_lowercase()).collect();
    let needle = b"layer ";
    let n = needle.len();
    if lower.len() < n + 1 {
        return false;
    }
    let mut i = 0usize;
    while i + n < lower.len() {
        if &lower[i..i + n] != needle {
            i += 1;
            continue;
        }
        let prev_ok = i == 0 || !is_word_byte(bytes[i - 1]);
        if !prev_ok {
            i += 1;
            continue;
        }
        let mut k = i + n;
        let first = lower[k];
        if first.is_ascii_digit() {
            while k < lower.len() && lower[k].is_ascii_digit() {
                k += 1;
            }
        } else if first == b'x' || first == b'n' {
            k += 1;
        } else {
            i += 1;
            continue;
        }
        let after_ok = k >= bytes.len() || !is_word_byte(bytes[k]);
        if after_ok {
            return true;
        }
        i = k;
    }
    false
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub fn find_marketing_phrase(content: &str) -> Option<&'static str> {
    let lower = content.to_ascii_lowercase();
    for p in MARKETING_PHRASES {
        if lower.contains(*p) {
            return Some(*p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_detected_for_box_drawing() {
        assert!(is_decorative_banner("─────────────────────────", 10));
        assert!(is_decorative_banner("=========================", 10));
        assert!(is_decorative_banner("-------------------------", 10));
    }

    #[test]
    fn banner_rejects_short_runs() {
        assert!(!is_decorative_banner("---", 10));
    }

    #[test]
    fn banner_rejects_words() {
        assert!(!is_decorative_banner("Helpers", 10));
        assert!(!is_decorative_banner("== End ==", 10));
    }

    #[test]
    fn marketing_phrase_hits() {
        assert!(find_marketing_phrase("This robust and scalable platform").is_some());
        assert!(find_marketing_phrase("Revolutionary new feature").is_some());
        assert!(find_marketing_phrase("a normal sentence").is_none());
    }

    #[test]
    fn layer_label_word_boundary() {
        assert!(has_layer_label("Layer 1: read cache"));
        assert!(has_layer_label("layer 12 hygiene"));
        assert!(has_layer_label("Layer X is the new layer"));
        assert!(!has_layer_label("multilayered cake"));
        assert!(!has_layer_label("layered architecture"));
    }

    #[test]
    fn goal_header_matches() {
        assert!(is_goal_header("Goal: ship the feature"));
        assert!(is_goal_header("## Goal"));
        assert!(!is_goal_header("the goal is unclear"));
    }

    #[test]
    fn public_surface_header_matches() {
        assert!(is_public_surface_header("Public surface:"));
        assert!(is_public_surface_header("## Public Surface"));
        assert!(!is_public_surface_header("the public api is small"));
    }

    #[test]
    fn pattern_adapted_from_matches() {
        assert!(has_pattern_adapted_from("Pattern adapted from foo crate"));
        assert!(!has_pattern_adapted_from("just a normal comment"));
    }
}
