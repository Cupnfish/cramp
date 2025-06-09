use lsp::{ToolResult, Toolbox, ToolboxError};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::{Error as McpError, RoleServer, ServerHandler, model::*, service::RequestContext, tool};
use serde::Deserialize;
use std::sync::Arc;
use tracing::error;

// Define parameter structures for tools
#[derive(Debug, Deserialize, JsonSchema)]
struct ManageProjectsRequest {
    #[schemars(
        description = "The path to load a new project, or the name of an existing project to set as active. Omit to just list projects. Activating a project returns a snapshot (tree, initial diagnostics)."
    )]
    project_path_or_name: Option<String>,
    #[schemars(
        description = "The name of a project to remove from the workspace. Removal happens before adding/selecting."
    )]
    remove_project_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListDiagnosticsRequest {
    #[schemars(
        description = "If provided, only returns diagnostics for this specific file path (relative to project root)."
    )]
    file_path: Option<String>,
    #[schemars(description = "Limits the number of diagnostics returned.")]
    limit: Option<u32>,
    #[schemars(description = "If true, forces a fresh check instead of using cached results.")]
    force_recheck: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GetCodeActionsRequest {
    #[schemars(description = "The relative file path of the diagnostic.")]
    file_path: String,
    #[schemars(
        description = "The exact message of the diagnostic, as returned by `list_diagnostics`."
    )]
    diagnostic_message: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ApplyFixRequest {
    #[schemars(description = "The unique ID of the fix, obtained from `get_code_actions`.")]
    fix_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListDocumentSymbolsRequest {
    #[schemars(description = "Relative path to the file within the active project.")]
    file_path: String,
}

// *** USABILITY: Redesign GetSymbolInfoRequest ***
#[derive(Debug, Deserialize, JsonSchema)]
struct GetSymbolInfoRequest {
    #[schemars(description = "Relative path to the file containing the symbol.")]
    file_path: String,
    #[schemars(
        description = "0-based line number of the symbol (optional if symbol_name is provided)."
    )]
    line: Option<u32>,
    #[schemars(
        description = "0-based column number of the symbol (optional if symbol_name is provided)."
    )]
    column: Option<u32>,
    #[schemars(
        description = "Name of the symbol to find within the file (optional if line/column are provided)."
    )]
    symbol_name: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchWorkspaceSymbolsRequest {
    #[schemars(description = "The text query to search for in symbol names across the project.")]
    query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TestProjectRequest {
    #[schemars(description = "Optional name or substring to filter which tests to run.")]
    test_name: Option<String>,
}

/// Maps LSP Toolbox errors to MCP errors
fn map_toolbox_error(e: ToolboxError) -> McpError {
    match e {
        ToolboxError::NoActiveProject
        | ToolboxError::ProjectNotFound(_)
        | ToolboxError::ProjectAlreadyExists(_)
        | ToolboxError::InvalidPath(_)
        | ToolboxError::FixNotFound(_)
        | ToolboxError::DiagnosticNotFound(_, _)
        | ToolboxError::CacheEmpty(_)
        | ToolboxError::NotACargoProject(_) => McpError::invalid_params(e.to_string(), None),
        ToolboxError::ApplyEditFailed(_)
        | ToolboxError::ServerError(_)
        | ToolboxError::Io(_)
        | ToolboxError::Json(_)
        | ToolboxError::TaskJoin(_)
        | ToolboxError::Other(_) => McpError::internal_error(e.to_string(), None),
    }
}

// Helper to convert ToolResult<String> to Mcp Tool Result
async fn to_mcp_result<F, Fut>(f: F) -> Result<CallToolResult, McpError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ToolResult<String>>,
{
    match f().await {
        Ok(s) => Ok(CallToolResult::success(vec![Content::text(s)])),
        Err(e) => {
            error!(error=%e, "Toolbox operation failed");
            Err(map_toolbox_error(e))
        }
    }
}

#[derive(Clone)]
pub struct McpToolboxService {
    pub toolbox: Arc<Toolbox>,
}

impl McpToolboxService {
    pub fn new() -> Self {
        Self {
            toolbox: Arc::new(Toolbox::new()),
        }
    }

    pub fn with_timeouts(request_timeout: u64, shutdown_timeout: u64, initial_wait: u64) -> Self {
        Self {
            toolbox: Arc::new(Toolbox::with_timeouts(
                request_timeout,
                shutdown_timeout,
                initial_wait,
            )),
        }
    }
    pub async fn shutdown(&self) {
        let _ = self.toolbox.shutdown_all().await;
    }
}

#[tool(tool_box)]
impl McpToolboxService {
    #[tool(
        description = "Manages projects: lists, loads from path, sets active by name, or removes by name. All other tools operate on the active project. When a project is activated (loaded or selected), returns a snapshot including file tree and initial diagnostics summary, plus workspace status and next step guidance."
    )]
    async fn manage_projects(
        &self,
        #[tool(aggr)] req: ManageProjectsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| {
            self.toolbox
                .manage_projects(req.project_path_or_name, req.remove_project_name)
        })
        .await
    }

    #[tool(
        description = "Runs `cargo check` on the active project. Returns a JSON list of current errors and warnings, optionally filtered by file and limited. This must be run before `get_code_actions`. All line/column numbers are 0-based."
    )]
    async fn list_diagnostics(
        &self,
        #[tool(aggr)] req: ListDiagnosticsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| {
            self.toolbox
                .list_diagnostics(req.file_path, req.limit, req.force_recheck)
        })
        .await
    }

    #[tool(
        description = "Retrieves available automatic fixes for a specific diagnostic. Requires the exact file_path and message from a diagnostic returned by `list_diagnostics`. Returns a JSON list of actions, each with an `id`, description, and diff preview."
    )]
    async fn get_code_actions(
        &self,
        #[tool(aggr)] req: GetCodeActionsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| {
            self.toolbox
                .get_code_actions(req.file_path, req.diagnostic_message)
        })
        .await
    }

    #[tool(
        description = "Applies an automatic fix identified by `get_code_actions`. Requires the `fix_id`. Applying any fix invalidates all other cached fix_ids and diagnostics for the project; you must run `list_diagnostics` again afterwards."
    )]
    async fn apply_fix(
        &self,
        #[tool(aggr)] req: ApplyFixRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.apply_fix(req.fix_id)).await
    }

    #[tool(
        description = "Lists the file and directory structure of the active project, respecting .gitignore. Returns a text-based tree. Does not read file content."
    )]
    async fn get_file_tree(&self) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.get_file_tree()).await
    }

    #[tool(
        description = "Lists all high-level code symbols (structs, functions, traits, etc.) found in the specified file. Returns a JSON list. All line/column numbers are 0-based."
    )]
    async fn list_document_symbols(
        &self,
        #[tool(aggr)] req: ListDocumentSymbolsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.list_document_symbols(req.file_path)).await
    }

    #[tool(
        description = "Provides detailed information (documentation, signature, definition structure, methods, fields) for the code symbol. Provide file_path AND EITHER line/column (0-based) OR symbol_name. Returns markdown."
    )]
    async fn get_symbol_info(
        &self,
        #[tool(aggr)] req: GetSymbolInfoRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| {
            self.toolbox
                .get_symbol_info(req.file_path, req.line, req.column, req.symbol_name)
        })
        .await
    }

    #[tool(
        description = "Searches the entire active project workspace for symbols matching the query string. Returns a JSON list. All line/column numbers are 0-based."
    )]
    async fn search_workspace_symbols(
        &self,
        #[tool(aggr)] req: SearchWorkspaceSymbolsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.search_workspace_symbols(req.query)).await
    }

    #[tool(
        description = "Runs `cargo test` on the active project. Can optionally filter tests by name. Returns the raw test output and guidance."
    )]
    async fn test_project(
        &self,
        #[tool(aggr)] req: TestProjectRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.test_project(req.test_name)).await
    }
}

const INSTRUCTIONS: &str = r#"
This server provides 9 tools for agentic interaction with Rust projects.
All file paths are relative to the active project root. All line and column numbers are 0-based.
Your environment (the client) is responsible for reading/writing file content, except when using `apply_fix`.
Standard Workflow:
1.  `manage_projects`: Load a project path or select an existing one. Receive initial status, file tree, and diagnostic summary.
2.  `list_diagnostics`: Get full, up-to-date JSON list of errors/warnings. Run this before `get_code_actions`.
3.  Investigation:
    - `get_file_tree`: Review project structure.
    - `list_document_symbols`: Get symbols within one file (JSON).
    - `search_workspace_symbols`: Find symbols across the project (JSON).
    - `get_symbol_info`: Get details (docs, signature, definition) for a symbol. Provide `file_path` AND EITHER `line`/`column` (of usage or definition) OR `symbol_name` (definition must be in `file_path`) (Markdown).
    - Use client-side I/O to read file contents.
4.  Fixing:
    - `get_code_actions`: Get available auto-fixes (JSON with `id` and `diff`) for a specific diagnostic (requires exact `file_path` and `message` from `list_diagnostics`).
    - `apply_fix`: Apply an auto-fix using its `id`. This modifies files on disk and invalidates all cached diagnostics and fixes.
    - If no auto-fix, use client-side I/O to write manual code changes.
5.  Verification:
     - After any fix (auto or manual), run `list_diagnostics` again.
     - `test_project`: Run tests to verify fixes and check for regressions.
Always follow the 'Next Step' guidance provided in each tool's response.
"#;

#[tool(tool_box)]
impl ServerHandler for McpToolboxService {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: server_info(),
            instructions: Some(INSTRUCTIONS.to_string()),
        }
    }

    async fn initialize(
        &self,
        _request: InitializeRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        tracing::info!("Client initialized connection.");
        Ok(InitializeResult {
            protocol_version: ProtocolVersion::V_2024_11_05,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: server_info(),
            instructions: Some(INSTRUCTIONS.to_string()),
        })
    }
}

fn server_info() -> Implementation {
    Implementation {
        name: "cramp".to_string(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}
