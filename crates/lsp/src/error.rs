use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::task::JoinError;

// --- Toolbox Layer Errors ---

#[derive(Error, Debug)]
pub enum ToolboxError {
    #[error("No active project is set. Use `manage_projects` first.")]
    NoActiveProject,
    #[error("Project '{0}' not found or not loaded.")]
    ProjectNotFound(String),
    #[error("Project named '{0}' already exists/is loaded.")]
    ProjectAlreadyExists(String),
    #[error("Invalid path: {0:?}. Path must exist and be within the project root.")]
    InvalidPath(PathBuf),
    #[error(
        "Fix ID '{0}' not found or expired. Fixes are invalidated after any code change or re-check."
    )]
    FixNotFound(String),
    #[error(
        "Diagnostic message '{1}' not found for file '{0}'. Ensure `list_diagnostics` has been run and the message/path match exactly."
    )]
    DiagnosticNotFound(String, String),
    #[error("Diagnostic cache is empty for project '{0}'. Run `list_diagnostics` first.")]
    CacheEmpty(String),
    #[error("Failed to apply fix: {0}")]
    ApplyEditFailed(String),
    #[error("Path is not a valid Cargo project (missing Cargo.toml): {0:?}")]
    NotACargoProject(PathBuf),
    #[error("Backend server error: {0}")]
    ServerError(#[from] RaError),
    #[error("File IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization/deserialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Task join error: {0}")]
    TaskJoin(#[from] JoinError),
    #[error("Internal error: {0}")]
    Other(String),
}

#[derive(Deserialize, Clone)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

impl std::fmt::Debug for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseError")
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

// --- Server Layer Errors ---

#[derive(Error, Debug)]
pub enum RaError {
    #[error("LSP request failed: {0:?}")]
    LspRequestError(ResponseError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("LSP protocol error: {0}")]
    Lsp(String),
    #[error("Process management error: {0}")]
    Process(String),
    #[error("Channel communication error: {0}")]
    Channel(String),
    #[error("Oneshot channel receive error: {0}")]
    OneshotRecv(#[from] oneshot::error::RecvError),
    #[error("Timeout ({0:?}) waiting for response ID {1}")]
    Timeout(std::time::Duration, u64),
    #[error("Server is not running or shutting down")]
    ServerNotRunning,
    #[error("Server not yet initialized")]
    ServerNotInitialized,
    #[error("Failed to convert Path to URI: {0:?}")]
    PathToUri(PathBuf),
    #[error("Failed to convert URI to Path: {0}")]
    UriToPath(url::Url),
    #[error("Failed to convert String to URI: {0}")]
    UriParseError(#[from] url::ParseError),
    #[error("Cargo metadata error: {0}")]
    CargoMetadata(#[from] cargo_metadata::Error),
    #[error("Symbol or definition not found at {0:?}:{1}:{2}")]
    NotFound(PathBuf, u32, u32), // 0-based
    #[error("Cannot determine structure type for symbol")]
    UnknownStructureType,
    #[error("Task join error: {0}")]
    TaskJoin(#[from] JoinError),
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, RaError>;
pub type ToolResult<T> = std::result::Result<T, ToolboxError>;
