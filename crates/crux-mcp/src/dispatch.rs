use serde_json::Value;

use crate::protocol::CallToolResult;
use crate::tools;

pub struct AppContext {
    pub conn: rusqlite::Connection,
    pub config: crux_core::Config,
    pub project_root: Option<std::path::PathBuf>,
    pub global_config_path: std::path::PathBuf,
    pub project_config_path: Option<std::path::PathBuf>,
}

pub fn call(ctx: &AppContext, name: &str, arguments: &Value) -> CallToolResult {
    let result = match tools::REGISTRY.iter().find(|t| t.name() == name) {
        Some(tool) => tool.call(ctx, arguments),
        None => Err(format!("unknown tool: {name}")),
    };
    tools::digest::record_l11_event(ctx, name, arguments, &result);
    match result {
        Ok(text) => CallToolResult::text(text),
        Err(msg) => CallToolResult::error(msg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crux_l5_ast::{
        ConfidenceTier, EdgeKind, GraphStore, NodeKind, ParseResult, ParsedEdge, ParsedNode,
    };
    use crux_l8_memory::NewObservation;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    fn make_appcontext(project: PathBuf) -> AppContext {
        let conn = crux_core::db::open_in_memory().unwrap();
        AppContext {
            conn,
            config: crux_core::Config::default(),
            project_root: Some(project),
            global_config_path: PathBuf::from("/tmp/crux-test/global.toml"),
            project_config_path: None,
        }
    }

    fn seed_graph(ctx: &AppContext, project: &str) {
        let store = GraphStore::new(&ctx.conn);
        let result = ParseResult {
            nodes: vec![
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "compute_delta".to_string(),
                    qualified_name: "demo::delta::compute_delta".to_string(),
                    line_start: 1,
                    line_end: 1,
                    parent_qn: Some("demo::delta".to_string()),
                    signature: Some("pub fn compute_delta()".to_string()),
                    is_test: false,
                },
                ParsedNode {
                    kind: NodeKind::Function,
                    name: "caller".to_string(),
                    qualified_name: "demo::main::caller".to_string(),
                    line_start: 1,
                    line_end: 1,
                    parent_qn: Some("demo::main".to_string()),
                    signature: Some("pub fn caller()".to_string()),
                    is_test: false,
                },
            ],
            edges: vec![ParsedEdge {
                kind: EdgeKind::Calls,
                source_qn: "demo::main::caller".to_string(),
                target_qn: "compute_delta".to_string(),
                line: 1,
                confidence: 0.6,
                tier: ConfidenceTier::Inferred,
            }],
        };
        store
            .write(project, "src/lib.rs", "rust", "deadbeef", &result)
            .unwrap();
    }

    #[test]
    fn dispatch_call_records_l11_event_for_normal_tools() {
        use crux_l11_digest::DigestEngine;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_graph(&ctx, &project.display().to_string());

        let _ = call(
            &ctx,
            "crux_find_symbol",
            &json!({"name": "compute_delta", "exact": true, "session_id": "mcps"}),
        );
        let engine = DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
        let pending = engine.list_pending_events("mcps", 50).unwrap();
        assert_eq!(pending.len(), 1, "expected 1 recorded event");
        assert_eq!(pending[0].tool_name, "mcp__crux__crux_find_symbol");
        assert_eq!(pending[0].target.as_deref(), Some("compute_delta"));
    }

    #[test]
    fn dispatch_call_skips_l11_event_for_digest_tools() {
        use crux_l11_digest::DigestEngine;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project);
        let _ = call(&ctx, "crux_digest", &json!({"session_id": "mcps"}));
        let engine = DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
        let pending = engine.list_pending_events("mcps", 50).unwrap();
        assert_eq!(pending.len(), 0, "digest tool must not self-record");
    }

    #[test]
    fn digest_dispatcher_renders_summary() {
        use crux_l11_digest::{DigestEngine, TurnEvent};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        let engine = DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
        let project_s = project.display().to_string();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s.clone(),
                "Read",
                Some("src/login.rs".into()),
                "Read src/login.rs",
            ))
            .unwrap();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s.clone(),
                "Read",
                Some("src/login.rs".into()),
                "Read src/login.rs",
            ))
            .unwrap();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s,
                "Bash",
                Some("cargo test".into()),
                "Bash cargo test",
            ))
            .unwrap();

        let out = tools::digest::digest(&ctx, &json!({"session_id": "s1"})).unwrap();
        assert!(out.contains("Files read"), "missing reads bucket: {out}");
        assert!(out.contains("src/login.rs ×2"));
        assert!(out.contains("Commands"));
        assert!(out.contains("cargo ×1"));
    }

    #[test]
    fn digest_dispatcher_disabled_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let mut ctx = make_appcontext(project);
        ctx.config.layers.l11_digest = false;
        let err = tools::digest::digest(&ctx, &json!({"session_id": "s1"})).unwrap_err();
        assert!(err.contains("L11"));
    }

    #[test]
    fn compact_dispatcher_returns_digest_payload() {
        use crux_l11_digest::{DigestEngine, TurnEvent};
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        let engine = DigestEngine::new(&ctx.conn, ctx.config.layer.l11.clone());
        let project_s = project.display().to_string();
        engine
            .record(&TurnEvent::new(
                "s1",
                project_s,
                "Edit",
                Some("src/login.rs".into()),
                "Edit src/login.rs",
            ))
            .unwrap();

        let out = tools::digest::compact(&ctx, &json!({"session_id": "s1"})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["session_id"], "s1");
        assert_eq!(v["event_count"], 1);
        assert!(v["summary"].as_str().unwrap().contains("Files edited"));
    }

    #[test]
    fn find_symbol_dispatcher_returns_matches() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_graph(&ctx, &project.display().to_string());
        let out =
            tools::symbols::find_symbol(&ctx, &json!({"name": "compute_delta", "exact": true}))
                .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "compute_delta");
    }

    #[test]
    fn query_graph_dispatcher_callers_resolve_via_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_graph(&ctx, &project.display().to_string());
        let out = tools::graph::query_graph(
            &ctx,
            &json!({
                "qualified_name": "demo::delta::compute_delta",
                "direction": "callers"
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["qualified_name"], "demo::main::caller");
    }

    #[test]
    fn impact_dispatcher_walks_callers() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_graph(&ctx, &project.display().to_string());
        let out = tools::graph::impact(
            &ctx,
            &json!({
                "qualified_name": "demo::delta::compute_delta",
                "depth": 3,
                "max": 50
            }),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert!(arr.iter().any(|n| n["name"] == "caller"));
    }

    #[test]
    fn query_graph_rejects_bad_direction() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project);
        let err = tools::graph::query_graph(
            &ctx,
            &json!({"qualified_name": "x", "direction": "siblings"}),
        )
        .unwrap_err();
        assert!(err.contains("unknown direction"));
    }

    #[test]
    fn execute_dispatcher_runs_bash_echo() {
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_appcontext(dir.path().to_path_buf());
        let out =
            tools::execute::execute(&ctx, &json!({"runtime": "bash", "code": "echo from-mcp"}))
                .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exit_code"], 0);
        assert!(v["stdout"].as_str().unwrap().contains("from-mcp"));
        assert_eq!(v["timed_out"], false);
    }

    #[test]
    fn execute_dispatcher_rejects_unknown_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_appcontext(dir.path().to_path_buf());
        let err = tools::execute::execute(&ctx, &json!({"runtime": "ruby", "code": "puts 1"}))
            .unwrap_err();
        assert!(err.contains("unknown runtime"));
    }

    #[test]
    fn execute_dispatcher_rejects_when_l7_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut ctx = make_appcontext(dir.path().to_path_buf());
        ctx.config.layers.l7_sandbox = false;
        let err = tools::execute::execute(&ctx, &json!({"runtime": "bash", "code": "echo x"}))
            .unwrap_err();
        assert!(
            err.contains("L7 sandbox is disabled") && err.contains("l7_sandbox = true"),
            "expected helpful re-enable hint, got: {err}"
        );
    }

    #[test]
    fn execute_dispatcher_runs_by_default_without_explicit_config() {
        if std::process::Command::new("bash")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_appcontext(dir.path().to_path_buf());
        assert!(ctx.config.layers.l7_sandbox, "default must be on");
        let out = tools::execute::execute(
            &ctx,
            &json!({"runtime": "bash", "code": "echo default-on-works"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exit_code"], 0);
        assert!(v["stdout"].as_str().unwrap().contains("default-on-works"));
    }

    fn seed_indexed_code_chunk(ctx: &AppContext, project: &std::path::Path) {
        seed_indexed_code_chunk_with_body(ctx, project, "pub fn compute_delta() -> i32 { 42 }");
    }

    fn seed_indexed_code_chunk_with_body(ctx: &AppContext, project: &std::path::Path, body: &str) {
        let embedder = crux_l6_search::build_embedder(&ctx.config.layer.l6).unwrap();
        let indexer = crux_l6_search::Indexer::new(&ctx.conn);
        let chunk = crux_l6_search::Chunk {
            project_root: project.display().to_string(),
            source_id: None,
            file_path: "src/lib.rs".into(),
            language: Some("rust".into()),
            content_type: crux_l6_search::ContentType::Code,
            title: Some("compute_delta".into()),
            content: body.into(),
            line_start: 1,
            line_end: body.lines().count().max(1) as u32,
        };
        indexer.index_chunks(&[chunk], embedder.as_ref()).unwrap();
    }

    #[test]
    fn search_dispatcher_returns_results() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_indexed_code_chunk(&ctx, &project);

        let out =
            tools::search::search(&ctx, &json!({"query": "compute delta", "limit": 5})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert!(!arr.is_empty(), "should return at least one hit");
        assert_eq!(arr[0]["title"], "compute_delta");
    }

    #[test]
    fn search_dispatcher_shows_snippet_in_compact_view() {
        let body = "pub fn compute_delta() -> i32 { 42 }";
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_indexed_code_chunk_with_body(&ctx, &project, body);

        let out = tools::search::search(
            &ctx,
            &json!({"query": "compute_delta", "limit": 5, "view": "compact"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        let snippet = arr[0]["snippet"].as_str().unwrap();
        assert!(
            snippet.contains("compute_delta"),
            "snippet should mention the matched query term"
        );
    }

    #[test]
    fn search_dispatcher_shows_full_source_in_full_view() {
        let body = "pub fn compute_delta() -> i32 { 42 }";
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_indexed_code_chunk_with_body(&ctx, &project, body);

        let out = tools::search::search(
            &ctx,
            &json!({"query": "compute_delta", "limit": 5, "view": "full"}),
        )
        .unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr[0]["snippet"], body);
    }

    #[test]
    fn search_dispatcher_rounds_score() {
        let body = "pub fn compute_delta() -> i32 { 42 }";
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        seed_indexed_code_chunk_with_body(&ctx, &project, body);

        let out =
            tools::search::search(&ctx, &json!({"query": "compute_delta", "limit": 5})).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let arr = v.as_array().unwrap();
        let score = arr[0]["score"].as_f64().unwrap();
        let places = (score * 10_000.0).fract();
        assert!(
            places < 0.0001,
            "score {} should have at most 4 decimal places",
            score
        );
    }

    #[test]
    fn read_dispatcher_returns_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("readme.md");
        std::fs::write(&src, "hello world\nline two\n").unwrap();
        let ctx = make_appcontext(project);
        let out = tools::read::read(&ctx, &json!({"file_path": "readme.md"})).unwrap();
        assert!(out.contains("hello world"), "expected file content");
    }

    #[test]
    fn read_dispatcher_returns_blocked_when_file_outside_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project);
        let err = tools::read::read(&ctx, &json!({"file_path": "/etc/passwd"})).unwrap_err();
        assert!(err.contains("escapes project") || err.contains("cannot be resolved"));
    }

    #[test]
    fn read_dispatcher_honours_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("lines.txt");
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&src, &content).unwrap();
        let ctx = make_appcontext(project);
        let out = tools::read::read(
            &ctx,
            &json!({"file_path": "lines.txt", "offset": 10, "limit": 3}),
        )
        .unwrap();
        assert!(out.contains("line 10"), "should start at line 10");
        assert!(out.contains("line 12"), "should include line 12");
        assert!(!out.contains("line 13"), "should NOT include line 13");
    }

    #[test]
    fn read_dispatcher_omits_line_numbers_without_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("plain.txt");
        std::fs::write(&src, "a\nb\nc\n").unwrap();
        let ctx = make_appcontext(project);
        let out = tools::read::read(
            &ctx,
            &json!({"file_path": "plain.txt", "offset": 1, "limit": 2}),
        )
        .unwrap();
        assert!(!out.contains("    1"), "plain reads omit line numbers");
    }

    #[test]
    fn read_dispatcher_shows_line_numbers_with_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "pub fn greet() {}\nfn hidden() {}\n").unwrap();
        let ctx = make_appcontext(project.clone());
        seed_graph(&ctx, &project.display().to_string());
        let out =
            tools::read::read(&ctx, &json!({"symbol": "demo::delta::compute_delta"})).unwrap();
        assert!(
            out.contains("    1"),
            "symbol read annotates with line numbers"
        );
    }

    #[test]
    fn read_dispatcher_includes_memory_footer_when_available() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, "pub fn greet() -> &str { \"hi\" }\n").unwrap();
        let mut ctx = make_appcontext(project.clone());
        ctx.config.layer.l8.auto_surface = true;
        ctx.config.layer.l8.auto_surface_limit = 5;
        let mem = crux_l8_memory::MemoryEngine::new(&ctx.conn).unwrap();
        mem.remember(NewObservation {
            project_root: project.display().to_string(),
            session_id: None,
            agent_id: None,
            kind: crux_l8_memory::ObservationKind::Reference,
            title: "uses greet".into(),
            content: "the agent likes greet".into(),
            why: None,
            how_to_apply: None,
            symbol: Some("greet".into()),
            file_path: Some("src/lib.rs".into()),
            tags: vec![],
            importance: 5,
            private: false,
        })
        .ok();
        let out = tools::read::read(&ctx, &json!({"file_path": "src/lib.rs"})).unwrap();
        assert!(
            out.contains("[crux:l8]"),
            "expected memory footer, got: {out}"
        );
    }

    #[test]
    fn read_dispatcher_outlines_large_file_without_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let big = "pub fn a() {}\n".repeat(1001);
        std::fs::write(&src, big).unwrap();
        let ctx = make_appcontext(project.clone());
        seed_graph(&ctx, &project.display().to_string());
        let out = tools::read::read(&ctx, &json!({"file_path": "src/lib.rs"})).unwrap();
        assert!(out.contains("[crux:l4+l5]"), "expected outline, got: {out}");
    }

    #[test]
    fn read_dispatcher_outline_threshold_respected() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("small.rs");
        std::fs::write(&src, "pub fn small() {}\n").unwrap();
        let ctx = make_appcontext(project.clone());
        let out = tools::read::read(&ctx, &json!({"file_path": "small.rs"})).unwrap();
        assert!(
            !out.contains("[crux:l4+l5]"),
            "small file should not trigger outline"
        );
    }

    #[test]
    fn read_dispatcher_outline_disabled_when_set_to_zero() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let src = project.join("src/lib.rs");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        let big = "pub fn a() {}\n".repeat(1001);
        std::fs::write(&src, big).unwrap();
        let mut ctx = make_appcontext(project.clone());
        ctx.config.layer.l4.outline_above_lines = 0;
        let out = tools::read::read(&ctx, &json!({"file_path": "src/lib.rs"})).unwrap();
        assert!(
            !out.contains("[crux:l4+l5]"),
            "outline should be disabled when threshold is 0"
        );
    }

    #[test]
    fn bash_filter_dispatcher_filters_output() {
        let ctx = make_appcontext(PathBuf::from("/tmp/nonexistent"));
        let out =
            tools::bash::bash_filter(&ctx, &json!({"command": "ls", "output": "src/\ntests/\n"}))
                .unwrap();
        assert!(!out.is_empty(), "bash_filter should not crash on any input");
    }

    #[test]
    fn audit_dispatcher_returns_pretty_json() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project);
        let out = tools::audit::audit(&ctx).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("layers").is_some(), "audit must include layers");
    }

    #[test]
    fn remember_dispatcher_creates_observation() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project);
        let out = tools::memory::remember(
            &ctx,
            &json!({
                "kind": "reference",
                "title": "test obs",
                "content": "some content"
            }),
        )
        .unwrap();
        assert!(
            out.starts_with("remembered #"),
            "expected remember output: {out}"
        );
    }

    fn insert_observation(ctx: &AppContext, project: &str, symbol: &str) -> i64 {
        use crux_l8_memory::{MemoryEngine, NewObservation, ObservationKind};
        let mem = MemoryEngine::new(&ctx.conn).unwrap();
        let obs = NewObservation {
            project_root: project.to_string(),
            session_id: None,
            agent_id: None,
            kind: ObservationKind::Reference,
            title: format!("pref {symbol}"),
            content: format!("the agent likes {symbol}"),
            why: None,
            how_to_apply: None,
            symbol: Some(symbol.to_string()),
            file_path: Some("src/lib.rs".to_string()),
            tags: vec![],
            importance: 5,
            private: false,
        };
        mem.remember(obs).unwrap()
    }

    #[test]
    fn recall_dispatcher_finds_matching_observations() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project.clone());
        let project_s = project.display().to_string();
        insert_observation(&ctx, &project_s, "greet");
        let out = tools::memory::recall(&ctx, &json!({"query": "greet", "limit": 5})).unwrap();
        assert!(
            out.contains("greet"),
            "recall should find inserted observation: {out}"
        );
    }

    #[test]
    fn recall_dispatcher_shows_empty_when_no_matches() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let ctx = make_appcontext(project);
        let out =
            tools::memory::recall(&ctx, &json!({"query": "nonexistent", "limit": 5})).unwrap();
        assert_eq!(out, "(no observations found)");
    }
}
