pub const COMMON_MISTAKES: &str = include_str!("../templates/common_mistakes.md");
pub const QUICK_START: &str = include_str!("../templates/quick_start.md");
pub const ARCHITECTURE_MAP: &str = include_str!("../templates/architecture_map.md");
pub const CLAUDEIGNORE: &str = include_str!("../templates/claudeignore");
pub const CRUX_IGNORE: &str = include_str!("../templates/crux_ignore");
pub const COMPLETIONS_README: &str = include_str!("../templates/completions_readme.md");
pub const SESSIONS_README: &str = include_str!("../templates/sessions_readme.md");

pub fn render_claude_md(meta: &ProjectMeta<'_>, profile_body: &str) -> String {
    let today = chrono::Utc::now().format("%Y-%m-%d");
    format!(
        r#"# CLAUDE.md

Quick-start guide for agents working on this project.

## Project

- **Type**: {project_type}
- **Stack**: {stack}
- **Features**: {features}

## Session start (mandatory)

Read these four files at the start of every session:

1. `CLAUDE.md` (this file)
2. `.crux/COMMON_MISTAKES.md`
3. `.crux/QUICK_START.md`
4. `.crux/ARCHITECTURE_MAP.md`

Total budget: ~800 tokens.

## Never auto-load

- `.crux/completions/**`
- `.crux/sessions/**`
- `docs/archive/**`

These are 0-token-cost references — load only when explicitly needed.

## Output rules

{profile_body}

---

Profile: `{profile_name}`
Last updated: {today}
"#,
        project_type = meta.project_type,
        stack = meta.stack,
        features = meta.features,
        profile_name = meta.profile_name,
        profile_body = profile_body.trim(),
        today = today,
    )
}

#[derive(Debug, Clone)]
pub struct ProjectMeta<'a> {
    pub project_type: &'a str,
    pub stack: &'a str,
    pub features: &'a str,
    pub profile_name: &'a str,
}
