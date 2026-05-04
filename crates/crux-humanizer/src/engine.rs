use std::sync::OnceLock;

use regex::{Captures, Regex, RegexBuilder};

use crate::rules;
use crate::tokenizer::{join, tokenize, Segment};
use crate::types::{HumanizeOptions, HumanizeResult, Mode, Stats};

#[derive(Debug, Clone)]
pub struct Humanizer {
    mode: Mode,
    options: HumanizeOptions,
}

impl Humanizer {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            options: HumanizeOptions::for_mode(mode),
        }
    }

    pub fn with_options(mode: Mode, options: HumanizeOptions) -> Self {
        Self { mode, options }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn options(&self) -> &HumanizeOptions {
        &self.options
    }

    pub fn rewrite(&self, input: &str) -> HumanizeResult {
        let original_chars = input.chars().count();
        let original_words = word_count(input);

        let mut segments = tokenize(input);
        let mut edits = 0usize;
        let mut leading_is_sentence_start = true;

        for seg in &mut segments {
            match seg {
                Segment::Prose(text) => {
                    let rewritten = self.apply_passes(text, &mut edits);
                    let recased = recase_segment(&rewritten, leading_is_sentence_start);
                    leading_is_sentence_start = ends_with_sentence_terminator(&recased);
                    *text = recased;
                }
                Segment::Verbatim(text) => {
                    leading_is_sentence_start = ends_with_sentence_terminator(text);
                }
            }
        }

        let mut text = join(&segments);

        if self.options.collapse_blanks {
            let (collapsed, n) = collapse_blank_lines(&text);
            edits += n;
            text = collapsed;
        }

        text = trim_per_line_trailing_whitespace(&text);

        let trimmed_start = text.trim_start_matches([' ', '\t']);
        if trimmed_start.len() != text.len() {
            text = trimmed_start.to_string();
        }

        let stats = Stats {
            original_chars,
            rewritten_chars: text.chars().count(),
            original_words,
            rewritten_words: word_count(&text),
            edits_applied: edits,
        };

        HumanizeResult {
            mode: self.mode,
            text,
            stats,
        }
    }

    fn apply_passes(&self, prose: &str, edits: &mut usize) -> String {
        let mut s = prose.to_string();

        if self.options.strip_pleasantries {
            s = replace_count(pleasantry_re(), &s, "", edits);
            s = strip_leading_orphan_punct(&s);
        }

        s = replace_count(strike_re(), &s, "", edits);
        s = strip_leading_orphan_punct(&s);

        for (re, replacement) in phrase_subs() {
            s = replace_count(re, &s, replacement, edits);
        }

        for (re, replacement) in word_subs() {
            s = replace_with_case(re, &s, replacement, edits);
        }

        if mode_strips_fluff(self.mode) {
            s = replace_count(fluff_re(), &s, "", edits);
            s = replace_count(filler_re(), &s, "", edits);
            s = strip_leading_orphan_punct(&s);
        }

        if self.options.contract {
            for (re, replacement) in contractions() {
                s = replace_count(re, &s, replacement, edits);
            }
        }

        if self.options.dedupe_repeats {
            let (collapsed, n) = collapse_adjacent_repeats(&s);
            *edits += n;
            s = collapsed;
        }

        s = cleanup_empty_emphasis(&s);

        tidy_spaces(&s)
    }
}

fn mode_strips_fluff(mode: Mode) -> bool {
    !matches!(mode, Mode::Professional)
}

fn replace_count(re: &Regex, s: &str, replacement: &str, edits: &mut usize) -> String {
    let count = re.find_iter(s).count();
    if count == 0 {
        return s.to_string();
    }
    *edits += count;
    re.replace_all(s, replacement).into_owned()
}

fn replace_with_case(re: &Regex, s: &str, replacement: &str, edits: &mut usize) -> String {
    re.replace_all(s, |caps: &Captures| {
        *edits += 1;
        let matched = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        match_case(matched, replacement)
    })
    .into_owned()
}

fn match_case(original: &str, replacement: &str) -> String {
    if replacement.is_empty() {
        return String::new();
    }
    let first_orig = match original.chars().next() {
        Some(c) => c,
        None => return replacement.to_string(),
    };
    if !first_orig.is_uppercase() {
        return replacement.to_string();
    }
    let mut chars = replacement.chars();
    let first_repl = match chars.next() {
        Some(c) => c,
        None => return replacement.to_string(),
    };
    let mut buf = String::with_capacity(replacement.len());
    for c in first_repl.to_uppercase() {
        buf.push(c);
    }
    buf.extend(chars);
    buf
}

fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

fn collapse_blank_lines(s: &str) -> (String, usize) {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"\n{3,}").expect("blank-collapse regex"));
    let count = re.find_iter(s).count();
    (re.replace_all(s, "\n\n").into_owned(), count)
}

fn trim_per_line_trailing_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut first = true;
    for line in s.split('\n') {
        if !first {
            out.push('\n');
        }
        out.push_str(line.trim_end_matches([' ', '\t']));
        first = false;
    }
    out
}

fn cleanup_empty_emphasis(s: &str) -> String {
    static EMPTY_BOLD_STAR: OnceLock<Regex> = OnceLock::new();
    static EMPTY_BOLD_UND: OnceLock<Regex> = OnceLock::new();
    let star = EMPTY_BOLD_STAR
        .get_or_init(|| Regex::new(r"\*\*[ \t]*\*\*").expect("empty-bold-star regex"));
    let und =
        EMPTY_BOLD_UND.get_or_init(|| Regex::new(r"__[ \t]*__").expect("empty-bold-und regex"));
    let s = star.replace_all(s, "");
    let s = und.replace_all(&s, "");
    s.into_owned()
}

fn tidy_spaces(s: &str) -> String {
    static MULTI: OnceLock<Regex> = OnceLock::new();
    static PUNCT: OnceLock<Regex> = OnceLock::new();
    let multi = MULTI.get_or_init(|| Regex::new(r"[ \t]{2,}").expect("multi-space regex"));
    let punct =
        PUNCT.get_or_init(|| Regex::new(r"[ \t]+([.,;:!?])").expect("space-before-punct regex"));
    let s = multi.replace_all(s, " ");
    let s = punct.replace_all(&s, "$1");
    s.into_owned()
}

fn ends_with_sentence_terminator(s: &str) -> bool {
    s.trim_end_matches([' ', '\t'])
        .chars()
        .last()
        .is_some_and(|c| matches!(c, '.' | '!' | '?' | '\n'))
}

fn recase_segment(s: &str, leading_is_sentence_start: bool) -> String {
    static SENT: OnceLock<Regex> = OnceLock::new();
    static PARA: OnceLock<Regex> = OnceLock::new();
    let sent =
        SENT.get_or_init(|| Regex::new(r"([.!?])([ \t]+|\n+)([a-z])").expect("sentence recase"));
    let para =
        PARA.get_or_init(|| Regex::new(r"(\n[ \t]*\n[ \t]*)([a-z])").expect("paragraph recase"));

    let mut result = sent
        .replace_all(s, |caps: &Captures| {
            let p = &caps[1];
            let ws = &caps[2];
            let letter: String = caps[3].chars().flat_map(char::to_uppercase).collect();
            format!("{p}{ws}{letter}")
        })
        .into_owned();

    result = para
        .replace_all(&result, |caps: &Captures| {
            let pre = &caps[1];
            let letter: String = caps[2].chars().flat_map(char::to_uppercase).collect();
            format!("{pre}{letter}")
        })
        .into_owned();

    if leading_is_sentence_start {
        result = capitalize_first_letter(&result);
    }
    result
}

fn capitalize_first_letter(s: &str) -> String {
    let mut chars = s.char_indices();
    let mut leading_end = 0usize;
    let target = loop {
        match chars.next() {
            Some((_, c)) if c.is_whitespace() => {
                leading_end += c.len_utf8();
                continue;
            }
            Some((i, c)) if c.is_lowercase() => break Some((i, c)),
            _ => break None,
        }
    };

    let Some((i, c)) = target else {
        return s.to_string();
    };
    let upper: String = c.to_uppercase().collect();
    let mut buf = String::with_capacity(s.len());
    buf.push_str(&s[..leading_end]);
    buf.push_str(&upper);
    buf.push_str(&s[i + c.len_utf8()..]);
    buf
}

fn pleasantry_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = rules::PLEASANTRY_OPENERS.join("|");
        let pattern = format!(r"(?im)^[\s>*\-]*(?:{alt})[ \t]*");
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .multi_line(true)
            .build()
            .expect("pleasantry regex compiles")
    })
}

fn strike_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = rules::STRIKE_PHRASES.join("|");
        let pattern = format!(r"(?:{alt})[ \t]*");
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .expect("strike regex compiles")
    })
}

fn phrase_subs() -> &'static [(Regex, String)] {
    static V: OnceLock<Vec<(Regex, String)>> = OnceLock::new();
    V.get_or_init(|| {
        rules::PHRASE_SUBS
            .iter()
            .map(|(p, r)| {
                let escaped = regex::escape(p);
                let re = RegexBuilder::new(&format!(r"\b{escaped}\b"))
                    .case_insensitive(true)
                    .build()
                    .expect("phrase sub regex compiles");
                (re, (*r).to_string())
            })
            .collect()
    })
}

fn word_subs() -> &'static [(Regex, String)] {
    static V: OnceLock<Vec<(Regex, String)>> = OnceLock::new();
    V.get_or_init(|| {
        rules::WORD_SUBS
            .iter()
            .map(|(p, r)| {
                let escaped = regex::escape(p);
                let re = RegexBuilder::new(&format!(r"\b{escaped}\b"))
                    .case_insensitive(true)
                    .build()
                    .expect("word sub regex compiles");
                (re, (*r).to_string())
            })
            .collect()
    })
}

fn fluff_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = rules::FLUFF_ADJECTIVES
            .iter()
            .map(|w| regex::escape(w))
            .collect::<Vec<_>>()
            .join("|");
        let pattern = format!(r"\b(?:{alt})\b[ \t]*");
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .expect("fluff regex compiles")
    })
}

fn filler_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alt = rules::FILLER_WORDS
            .iter()
            .map(|w| regex::escape(w))
            .collect::<Vec<_>>()
            .join("|");
        let pattern = format!(r"\b(?:{alt})\b[ \t]*");
        RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .expect("filler regex compiles")
    })
}

fn strip_leading_orphan_punct(s: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        RegexBuilder::new(r"^[!?.,;:]+[ \t]*")
            .multi_line(true)
            .build()
            .expect("leading orphan punct regex")
    });
    re.replace_all(s, "").into_owned()
}

fn contractions() -> &'static [(Regex, String)] {
    static V: OnceLock<Vec<(Regex, String)>> = OnceLock::new();
    V.get_or_init(|| {
        rules::CONTRACTIONS
            .iter()
            .map(|(p, r)| {
                (
                    Regex::new(p).expect("contraction regex compiles"),
                    (*r).to_string(),
                )
            })
            .collect()
    })
}

fn collapse_adjacent_repeats(s: &str) -> (String, usize) {
    static TOK: OnceLock<Regex> = OnceLock::new();
    let tok = TOK.get_or_init(|| Regex::new(r"\S+").expect("token regex"));

    let mut count = 0usize;
    let mut out = String::with_capacity(s.len());
    let mut cursor = 0usize;
    let mut prev: Option<&str> = None;

    for m in tok.find_iter(s) {
        let token = m.as_str();
        let collapse = prev.is_some_and(|p| {
            p.len() >= 2
                && p.eq_ignore_ascii_case(token)
                && token.chars().all(is_token_char)
                && p.chars().all(is_token_char)
        });
        if collapse {
            count += 1;
            cursor = m.end();
            continue;
        }
        out.push_str(&s[cursor..m.start()]);
        out.push_str(token);
        prev = Some(token);
        cursor = m.end();
    }
    out.push_str(&s[cursor..]);

    (out, count)
}

fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '\''
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(mode: Mode, input: &str) -> String {
        Humanizer::new(mode).rewrite(input).text
    }

    #[test]
    fn strips_in_conclusion_filler() {
        let out = h(Mode::Concise, "In conclusion, the build passed.");
        assert!(!out.to_lowercase().contains("in conclusion"));
        assert!(out.contains("build passed"));
    }

    #[test]
    fn strips_ai_self_reference() {
        let out = h(Mode::Concise, "As an AI language model, I cannot do that.");
        assert!(!out.to_lowercase().contains("as an ai"));
        assert!(out.to_lowercase().contains("cannot"));
    }

    #[test]
    fn strips_pleasantry_opener() {
        let out = h(Mode::Concise, "Certainly! Here is the plan.");
        assert!(!out.to_lowercase().contains("certainly"));
        assert!(out.contains("Here is the plan"));
    }

    #[test]
    fn collapses_wordy_phrase() {
        let out = h(Mode::Concise, "We did this in order to ship faster.");
        assert!(!out.to_lowercase().contains("in order to"));
        assert!(out.contains("to ship faster"));
    }

    #[test]
    fn fluff_adjectives_removed_in_concise() {
        let out = h(
            Mode::Concise,
            "Our robust, comprehensive, cutting-edge platform.",
        );
        let lower = out.to_lowercase();
        assert!(!lower.contains("robust"));
        assert!(!lower.contains("comprehensive"));
        assert!(!lower.contains("cutting-edge"));
    }

    #[test]
    fn fluff_kept_in_professional() {
        let out = h(Mode::Professional, "Our robust platform.");
        assert!(out.to_lowercase().contains("robust"));
    }

    #[test]
    fn fenced_code_block_untouched() {
        let input = "Use\n```rust\nfn utilize() { /* leverage */ }\n```\nthen go.";
        let out = h(Mode::Concise, input);
        assert!(out.contains("fn utilize() { /* leverage */ }"));
    }

    #[test]
    fn inline_code_untouched() {
        let out = h(Mode::Concise, "Run `utilize_xyz()` to start.");
        assert!(out.contains("`utilize_xyz()`"));
        assert!(out.contains("to start"));
    }

    #[test]
    fn function_call_literal_untouched() {
        let out = h(Mode::Concise, "Call utilize(x) explicitly.");
        assert!(out.contains("utilize(x)"));
    }

    #[test]
    fn rust_path_untouched() {
        let out = h(Mode::Concise, "We leverage crux_core::config::Config.");
        assert!(out.contains("crux_core::config::Config"));
        assert!(!out.to_lowercase().contains("leverage "));
    }

    #[test]
    fn url_untouched_with_trailing_period() {
        let out = h(Mode::Concise, "See https://example.com/path?x=1.");
        assert!(out.contains("https://example.com/path?x=1"));
    }

    #[test]
    fn ip_and_hex_untouched() {
        let out = h(Mode::Concise, "Server 127.0.0.1 returned 0xdeadbeef.");
        assert!(out.contains("127.0.0.1"));
        assert!(out.contains("0xdeadbeef"));
    }

    #[test]
    fn path_untouched() {
        let out = h(Mode::Concise, "Edit ./src/main.rs and /etc/hosts.");
        assert!(out.contains("./src/main.rs"));
        assert!(out.contains("/etc/hosts"));
    }

    #[test]
    fn scoped_npm_package_untouched() {
        let out = h(Mode::Concise, "We utilize @crux/humanizer here.");
        assert!(out.contains("@crux/humanizer"));
    }

    #[test]
    fn casual_applies_contractions() {
        let out = h(Mode::Casual, "It is fine and we are ready.");
        assert!(out.contains("It's"));
        assert!(out.contains("we're"));
    }

    #[test]
    fn concise_does_not_contract() {
        let out = h(Mode::Concise, "It is fine.");
        assert!(out.contains("It is fine"));
    }

    #[test]
    fn developer_mode_strips_pleasantry_and_fluff() {
        let out = h(
            Mode::Developer,
            "Sure! The robust pipeline utilizes the cache.",
        );
        let lower = out.to_lowercase();
        assert!(!lower.contains("sure"));
        assert!(!lower.contains("robust"));
        assert!(out.contains("uses"));
    }

    #[test]
    fn social_short_and_contracted() {
        let out = h(Mode::Social, "It is great. We are happy.");
        assert!(out.contains("It's great"));
        assert!(out.contains("We're happy"));
    }

    #[test]
    fn github_readme_keeps_blank_lines() {
        let input = "# Title\n\n\n\nBody paragraph.\n\n\n\nAnother paragraph.";
        let out = h(Mode::GithubReadme, input);
        assert!(out.contains("\n\n\n\n"));
    }

    #[test]
    fn concise_collapses_blank_lines() {
        let input = "First.\n\n\n\nSecond.";
        let out = h(Mode::Concise, input);
        assert!(!out.contains("\n\n\n"));
        assert!(out.contains("First.\n\nSecond."));
    }

    #[test]
    fn collapses_repeated_word() {
        let out = h(Mode::Concise, "Run the build build twice.");
        assert_eq!(out.matches("build").count(), 1);
    }

    #[test]
    fn word_substitution_preserves_capitalization() {
        let out = h(Mode::Concise, "Utilize the cache. Then utilize more.");
        assert!(out.contains("Use the cache"));
        assert!(out.contains("then use more") || out.contains("Then use more"));
    }

    #[test]
    fn stats_count_chars_and_edits() {
        let result =
            Humanizer::new(Mode::Concise).rewrite("In conclusion, we utilize the platform.");
        assert!(result.stats.original_chars > result.stats.rewritten_chars);
        assert!(result.stats.edits_applied >= 2);
    }

    #[test]
    fn empty_input_is_identity() {
        let result = Humanizer::new(Mode::Concise).rewrite("");
        assert!(result.text.is_empty());
        assert_eq!(result.stats.edits_applied, 0);
    }

    #[test]
    fn no_op_input_is_unchanged() {
        let input = "The build passed.";
        let result = Humanizer::new(Mode::Concise).rewrite(input);
        assert_eq!(result.text, input);
        assert_eq!(result.stats.edits_applied, 0);
    }

    #[test]
    fn empty_bold_after_fluff_strip_is_cleaned() {
        let out = h(Mode::Concise, "Our **robust** platform ships fast.");
        assert!(!out.contains("****"));
        assert!(out.contains("platform ships fast"));
    }

    #[test]
    fn strike_phrase_eats_trailing_space() {
        let out = h(Mode::Concise, "In conclusion, we ship.");
        assert_eq!(out, "We ship.");
    }

    #[test]
    fn paragraph_break_recapitalises_next_word() {
        let input = "# Heading\n\nIn conclusion, the build passed.";
        let out = h(Mode::GithubReadme, input);
        assert!(out.contains("# Heading"));
        assert!(out.contains("\n\nThe build passed."));
    }

    #[test]
    fn legitimate_bold_text_preserved() {
        let out = h(Mode::Concise, "Our **fast** platform ships.");
        assert!(out.contains("**fast**"));
    }

    #[test]
    fn mixed_codeblock_and_strike_phrase() {
        let input = concat!(
            "Sure! In conclusion, here is the snippet:\n",
            "```\nlet x = utilize(y);\n```\n",
            "We then leverage it."
        );
        let out = h(Mode::Concise, input);
        assert!(out.contains("let x = utilize(y);"));
        assert!(!out.to_lowercase().contains("sure"));
        assert!(!out.to_lowercase().contains("in conclusion"));
        assert!(out.to_lowercase().contains("use"));
    }

    #[test]
    fn regression_smoke_input_yields_clean_imperative() {
        let input = "I'd be happy to help you with that! In this article, I'll walk you through how to basically just really simplify your code.";
        let out = h(Mode::Concise, input);
        assert_eq!(out, "Walk through how to simplify your code.");
    }

    #[test]
    fn pleasantry_removal_leaves_no_orphan_fragment() {
        let out = h(
            Mode::Concise,
            "I'd be happy to help you with that! Now do it.",
        );
        let lower = out.to_lowercase();
        assert!(!lower.contains("i'd be happy"));
        assert!(!lower.contains("you with that"));
        assert!(!out.starts_with('!'));
        assert!(out.contains("Now do it"));
    }

    #[test]
    fn pleasantry_removal_absolutely_no_orphan() {
        let out = h(Mode::Concise, "Absolutely! Here is the plan.");
        assert!(!out.to_lowercase().contains("absolutely"));
        assert!(!out.starts_with('!'));
        assert!(out.contains("Here is the plan"));
    }

    #[test]
    fn pleasantry_removal_of_course_no_orphan() {
        let out = h(Mode::Concise, "Of course! We ship today.");
        assert!(!out.to_lowercase().contains("of course"));
        assert!(!out.starts_with('!'));
        assert!(out.contains("We ship today"));
    }

    #[test]
    fn pleasantry_removal_sure_i_can_help_no_orphan() {
        let out = h(Mode::Concise, "Sure, I can help! Do this next.");
        let lower = out.to_lowercase();
        assert!(!lower.contains("sure"));
        assert!(!lower.contains("i can help"));
        assert!(!out.starts_with('!'));
        assert!(out.contains("Do this next"));
    }

    #[test]
    fn pleasantry_removal_im_happy_to_help_no_orphan() {
        let out = h(Mode::Concise, "I'm happy to help! Let us proceed.");
        let lower = out.to_lowercase();
        assert!(!lower.contains("happy to help"));
        assert!(!out.starts_with('!'));
        assert!(out.contains("Let us proceed"));
    }

    #[test]
    fn filler_removed_in_concise() {
        let out = h(Mode::Concise, "This is basically just really very simple.");
        let lower = out.to_lowercase();
        assert!(!lower.contains("basically"));
        assert!(!lower.contains(" just "));
        assert!(!lower.contains("really"));
        assert!(!lower.contains(" very "));
        assert!(out.contains("simple"));
    }

    #[test]
    fn filler_actually_simply_clearly_removed_in_concise() {
        let out = h(Mode::Concise, "Clearly, we actually simply need this.");
        let lower = out.to_lowercase();
        assert!(!lower.contains("clearly"));
        assert!(!lower.contains("actually"));
        assert!(!lower.contains("simply"));
        assert!(out.contains("need this"));
    }

    #[test]
    fn filler_kept_in_professional() {
        let out = h(Mode::Professional, "We basically just ship this.");
        let lower = out.to_lowercase();
        assert!(lower.contains("basically") || lower.contains("just"));
    }

    #[test]
    fn walk_through_phrase_becomes_imperative() {
        let out = h(Mode::Concise, "I'll walk you through how to build it.");
        let lower = out.to_lowercase();
        assert!(!lower.contains("i'll"));
        assert!(!lower.contains(" you "));
        assert!(out.contains("Walk through how to"));
    }

    #[test]
    fn filler_does_not_touch_code_or_identifiers() {
        let out = h(
            Mode::Concise,
            "Call really_fast_fn() and `just_do_it` with basically_good.",
        );
        assert!(out.contains("really_fast_fn()"));
        assert!(out.contains("`just_do_it`"));
    }

    #[test]
    fn filler_does_not_touch_urls() {
        let out = h(Mode::Concise, "See https://example.com/really/basic/path.");
        assert!(out.contains("https://example.com/really/basic/path"));
    }
}
