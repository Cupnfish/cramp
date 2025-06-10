use lsp_types::{CodeAction, TextEdit, WorkspaceEdit};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// --- Internal Common (0-based) ---
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy, Hash, PartialOrd, Ord, Default,
)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

impl Into<lsp_types::Position> for Position {
    fn into(self) -> lsp_types::Position {
        lsp_types::Position {
            line: self.line,
            character: self.character,
        }
    }
}

impl From<lsp_types::Position> for Position {
    fn from(value: lsp_types::Position) -> Self {
        Self {
            line: value.line,
            character: value.character,
        }
    }
}

#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy, Hash, PartialOrd, Ord, Default,
)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Into<lsp_types::Range> for Range {
    fn into(self) -> lsp_types::Range {
        lsp_types::Range {
            start: self.start.into(),
            end: self.end.into(),
        }
    }
}

impl From<lsp_types::Range> for Range {
    fn from(value: lsp_types::Range) -> Self {
        Self {
            start: value.start.into(),
            end: value.end.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord, Default)]
pub struct Location {
    pub path: PathBuf, // Absolute path
    pub range: Range,
}

// --- Internal Diagnostics (0-based) ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy, PartialOrd, Ord, Default)]
pub enum MySeverity {
    Error,
    Warning,
    Information,
    Hint,
    #[default]
    Unknown,
}
impl std::fmt::Display for MySeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{:?}", self).to_lowercase())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
pub struct MyDiagnostic {
    pub location: Location, // 0-based, absolute path
    pub severity: MySeverity,
    pub message: String,
    pub code: Option<String>,
}

// --- Agent Facing: list_diagnostics output ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, PartialOrd, Ord)]
pub struct SimpleDiagnostic {
    pub severity: String,
    pub message: String,
    pub file_path: String, // Relative
    pub line: u32,         // 0-based
    pub character: u32,    // 0-based
}

// --- Internal Code Actions ---
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MyCodeAction {
    pub title: String,
    pub edit: Option<WorkspaceEdit>, // LSP WorkspaceEdit is 0-based
    #[serde(skip)]
    pub original_action: Option<CodeAction>, // Keep if resolution needed
}
// --- Agent Facing: get_code_actions output ---
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionWithId {
    pub id: String,
    pub description: String,
    pub diff: String, // Preview of the first file change
}

// --- Internal Symbols (0-based)---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy, Default)]
pub enum MySymbolKind {
    Module,
    Struct,
    Enum,
    Trait,
    Function,
    Method,
    Field,
    Const,
    Static,
    Type,
    Impl,
    #[default]
    Other,
}
impl std::fmt::Display for MySymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format!("{:?}", self).to_lowercase())
    }
}

// --- Agent Facing: list/search symbols output ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatSymbol {
    pub name: String,
    pub kind: String,      // "struct" | "trait" | "function" etc
    pub file_path: String, // Relative
    pub line: u32,         // 0-based
    pub character: u32,    // 0-based
}

// --- Internal API Extraction (0-based) ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MyField {
    pub name: String,
    pub type_str: Option<String>,
    pub is_public: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MyMethod {
    pub name: String,
    pub signature_str: Option<String>,
    pub docs: Option<String>,
    pub has_self: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MyStructInfo {
    pub name: String,
    pub docs: Option<String>,
    pub location: Option<Location>, // 0-based
    pub fields: Vec<MyField>,
    pub methods: Vec<MyMethod>,
    pub implemented_traits_approx: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MyTraitInfo {
    pub name: String,
    pub docs: Option<String>,
    pub location: Option<Location>, // 0-based
    pub associated_types: Vec<String>,
    pub methods: Vec<MyMethod>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MyApiInfo {
    Struct(MyStructInfo),
    Trait(MyTraitInfo),
    Other {
        name: String,
        kind: MySymbolKind,
        docs: Option<String>,
        location: Option<Location>,
    },
}

// --- Internal Cargo ---
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CargoTestOutput {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

// --- Internal Toolbox Cache / State ---
#[derive(Debug, Clone)]
pub struct CachedFix {
    pub project_name: String,
    pub edit: WorkspaceEdit,
}
// Internal representation of an edit bundle for one file
#[derive(Debug, Clone)]
pub struct FileEdit {
    pub path: PathBuf,
    pub edits: Vec<TextEdit>, // LSP TextEdit is 0-based
}
