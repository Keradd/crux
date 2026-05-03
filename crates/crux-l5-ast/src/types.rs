//! Shared types for the AST graph.

use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum NodeKind {
    File,
    Module,
    Class,
    Function,
    Method,
    Type,
    Test,
    Constant,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::File => "File",
            NodeKind::Module => "Module",
            NodeKind::Class => "Class",
            NodeKind::Function => "Function",
            NodeKind::Method => "Method",
            NodeKind::Type => "Type",
            NodeKind::Test => "Test",
            NodeKind::Constant => "Constant",
        }
    }
}

impl FromStr for NodeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "File" => Ok(Self::File),
            "Module" => Ok(Self::Module),
            "Class" => Ok(Self::Class),
            "Function" => Ok(Self::Function),
            "Method" => Ok(Self::Method),
            "Type" => Ok(Self::Type),
            "Test" => Ok(Self::Test),
            "Constant" => Ok(Self::Constant),
            other => Err(format!("unknown NodeKind '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeKind {
    Calls,
    ImportsFrom,
    Inherits,
    Implements,
    Contains,
    TestedBy,
    DependsOn,
    References,
    /// L5.13e: `export default <expr>` in JS/TS. `source_qn` is
    /// `{module_qn}:file`; `target_qn` is the local FQN (or a bare
    /// identifier the file-local resolver upgrades to a FQN).
    ExportsDefault,
}

impl EdgeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Calls => "CALLS",
            EdgeKind::ImportsFrom => "IMPORTS_FROM",
            EdgeKind::Inherits => "INHERITS",
            EdgeKind::Implements => "IMPLEMENTS",
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::TestedBy => "TESTED_BY",
            EdgeKind::DependsOn => "DEPENDS_ON",
            EdgeKind::References => "REFERENCES",
            EdgeKind::ExportsDefault => "EXPORTS_DEFAULT",
        }
    }
}

impl FromStr for EdgeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "CALLS" => Ok(Self::Calls),
            "IMPORTS_FROM" => Ok(Self::ImportsFrom),
            "INHERITS" => Ok(Self::Inherits),
            "IMPLEMENTS" => Ok(Self::Implements),
            "CONTAINS" => Ok(Self::Contains),
            "TESTED_BY" => Ok(Self::TestedBy),
            "DEPENDS_ON" => Ok(Self::DependsOn),
            "REFERENCES" => Ok(Self::References),
            "EXPORTS_DEFAULT" => Ok(Self::ExportsDefault),
            other => Err(format!("unknown EdgeKind '{other}'")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfidenceTier {
    Extracted,
    Resolved,
    Inferred,
}

impl ConfidenceTier {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfidenceTier::Extracted => "EXTRACTED",
            ConfidenceTier::Resolved => "RESOLVED",
            ConfidenceTier::Inferred => "INFERRED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Language {
    Rust,
    Python,
    TypeScript,
    JavaScript,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "ts" | "tsx" => Self::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Self::JavaScript,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::TypeScript => "typescript",
            Language::JavaScript => "javascript",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: i64,
    pub project_root: String,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub language: Option<String>,
    pub parent_qn: Option<String>,
    pub signature: Option<String>,
    pub is_test: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedNode {
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub line_start: u32,
    pub line_end: u32,
    pub parent_qn: Option<String>,
    pub signature: Option<String>,
    pub is_test: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedEdge {
    pub kind: EdgeKind,
    pub source_qn: String,
    pub target_qn: String,
    pub line: u32,
    pub confidence: f64,
    pub tier: ConfidenceTier,
}

#[derive(Debug, Clone, Default)]
pub struct ParseResult {
    pub nodes: Vec<ParsedNode>,
    pub edges: Vec<ParsedEdge>,
}

#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    /// Files actually parsed + written this run.
    pub files_scanned: u64,
    /// Files dropped for non-Merkle reasons (too big, unknown language,
    /// IO error, parser panic).
    pub files_skipped: u64,
    /// Files whose content hash matched the previous snapshot and were
    /// not re-parsed this run.
    pub files_unchanged: u64,
    /// Files that existed in the previous snapshot but are gone from
    /// disk; their nodes + edges were purged.
    pub files_removed: u64,
    /// L5.12.5: files whose phase-1 signature payload was served from
    /// `ast_file_signatures` (bincode) instead of being re-parsed.
    /// Tracks how many file parses the signature cache saved this run.
    pub files_signature_cached: u64,
    pub nodes_upserted: u64,
    pub edges_upserted: u64,
}
