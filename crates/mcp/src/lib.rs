use lsp::{ToolResult, Toolbox, ToolboxError};
use rmcp::schemars;
use rmcp::schemars::JsonSchema;
use rmcp::{Error as McpError, RoleServer, ServerHandler, model::*, service::RequestContext, tool};
use serde::Deserialize;
use std::sync::Arc;
use tracing::error;

// Define parameter structures for tools

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

#[derive(Debug, Deserialize, JsonSchema)]
struct GetSymbolInfoRequest {
    #[schemars(description = "Relative path to the file containing the symbol.")]
    file_path: String,
    #[schemars(
        description = "Line number of the symbol, use values from search_workspace_symbols or list_document_symbols."
    )]
    line: u32,
    #[schemars(
        description = "Character position of the symbol, use values from search_workspace_symbols or list_document_symbols."
    )]
    character: u32,
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
        description = "Executes `cargo check` on the active Rust project to detect compilation errors and warnings. Returns a comprehensive JSON list of diagnostics with file paths, line/character positions, severity levels, and detailed error messages. Supports optional filtering by specific file path and result limiting. This tool must be run before `get_code_actions` to populate the diagnostic cache. Invalidates all previously cached fix IDs when executed."
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
        description = "Retrieves available automatic code fixes for a specific compilation diagnostic identified by `list_diagnostics`. Requires the exact file_path and diagnostic_message string as returned by the most recent `list_diagnostics` call. Returns a JSON list of available fix actions, each containing a unique `id` for application, human-readable description of the fix, and a detailed diff preview showing the proposed code changes. Essential for automated code repair workflow."
    )]
    async fn get_code_actions(
        &self,
        #[tool(aggr)] req: GetCodeActionsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| {
            self.toolbox
                .get_code_actions(req.file_path, Some(req.diagnostic_message), None, None)
        })
        .await
    }

    #[tool(
        description = "Applies an automatic code fix using the fix_id obtained from `get_code_actions`. Modifies source files on disk and notifies the internal LSP server of changes. CRITICAL: Applying any fix invalidates ALL cached fix_ids and diagnostic cache for the entire project. You MUST run `list_diagnostics` again after applying any fix to refresh the cache and obtain updated diagnostic information."
    )]
    async fn apply_fix(
        &self,
        #[tool(aggr)] req: ApplyFixRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.apply_fix(req.fix_id)).await
    }

    #[tool(
        description = "Analyzes and lists all high-level code symbols (structs, functions, traits, enums, modules, etc.) found in the specified Rust source file. Returns a detailed JSON list containing symbol names, types, locations with line/character positions, and hierarchical relationships. Essential for code navigation and understanding file structure before making modifications."
    )]
    async fn list_document_symbols(
        &self,
        #[tool(aggr)] req: ListDocumentSymbolsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.list_document_symbols(req.file_path)).await
    }

    #[tool(
        description = "Provides comprehensive information about a specific code symbol including documentation, function signatures, struct/enum definitions, available methods, fields, and implementation details. Requires file_path and precise line/character coordinates from search_workspace_symbols or list_document_symbols. Returns rich markdown-formatted documentation ideal for understanding APIs and code structure before modification."
    )]
    async fn get_symbol_info(
        &self,
        #[tool(aggr)] req: GetSymbolInfoRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| {
            self.toolbox
                .get_symbol_info(req.file_path, req.line, req.character)
        })
        .await
    }

    #[tool(
        description = "Performs a comprehensive search across the entire active Rust project workspace for symbols (functions, structs, traits, etc.) matching the provided query string. Uses fuzzy matching to find relevant symbols even with partial names. Returns a JSON list of matching symbols with their locations (line/character), file paths, and symbol types. Ideal for discovering APIs and understanding codebase structure."
    )]
    async fn search_workspace_symbols(
        &self,
        #[tool(aggr)] req: SearchWorkspaceSymbolsRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.search_workspace_symbols(req.query)).await
    }

    #[tool(
        description = "Executes `cargo test` on the active Rust project to run the test suite and verify code correctness. Supports optional filtering by test name or pattern to run specific tests. Returns raw test output including pass/fail status, test timing, and detailed failure information. CRITICAL: Running tests invalidates ALL cached fix_ids and diagnostic cache. Essential for verification after applying fixes or code changes."
    )]
    async fn test_project(
        &self,
        #[tool(aggr)] req: TestProjectRequest,
    ) -> Result<CallToolResult, McpError> {
        to_mcp_result(|| self.toolbox.test_project(req.test_name)).await
    }
}

const INSTRUCTIONS: &str = r###"
# CRAMP MCP Server - Rust Project Analysis & Repair Tools

This server provides specialized tools for intelligent interaction with Rust projects, enabling automated analysis, diagnosis, and repair of code issues.

## Available Tools

### Diagnostic Tools
- **`list_diagnostics`**: Execute `cargo check` to detect compilation errors and warnings
- **`get_code_actions`**: Retrieve available automatic fixes for specific diagnostics
- **`apply_fix`**: Apply automatic code fixes using fix IDs

### Analysis Tools
- **`list_document_symbols`**: List symbols within a specific file
- **`get_symbol_info`**: Get detailed information about a specific symbol
- **`search_workspace_symbols`**: Search for symbols across the entire project

### Testing Tools
- **`test_project`**: Run the project's test suite

## Core Principles
- **Coordinate System**: Line and character positions are provided by symbol search tools
- **Relative Paths**: All file paths are relative to the active project root directory
- **Cache Management**: Critical state invalidation rules must be followed for correct operation
- **Client I/O Responsibility**: Your environment handles file reading/writing (except `apply_fix`)

## Essential Workflow

### Diagnosis Phase
- **`list_diagnostics`**: Execute `cargo check` to detect compilation errors/warnings
  - Optional parameters: `file_path` (filter by file), `limit` (max results), `force_recheck` (bypass cache)
  - CRITICAL: Must run before `get_code_actions` - populates diagnostic cache
  - CRITICAL: Invalidates ALL previously cached fix IDs when executed

### Investigation & Analysis
- **`list_document_symbols`**: Analyze symbols within specific files (JSON output)
  - Requires: `file_path` (relative to project root)
- **`search_workspace_symbols`**: Find symbols across entire project with fuzzy matching
  - Requires: `query` (search term)
- **`get_symbol_info`**: Get detailed API documentation and signatures (Markdown output)
  - Requires: `file_path` and `line`/`character` coordinates from search_workspace_symbols or list_document_symbols
- **Client-side file reading**: Use your environment to read source code content

### Automated Repair
- **`get_code_actions`**: Retrieve available automatic fixes for specific diagnostics
  - Requires: `file_path` and `diagnostic_message` (exact strings from `list_diagnostics`)
  - Returns fix actions with unique `id`, description, and diff preview
- **`apply_fix`**: Apply automatic fix using fix_id from `get_code_actions`
  - Requires: `fix_id` (from `get_code_actions`)
  - CRITICAL: Invalidates ALL fix IDs and diagnostic cache for entire project
  - CRITICAL: MUST run `list_diagnostics` again after applying any fix

### Manual Repair (when auto-fix unavailable)
- Use investigation tools to understand the issue
- Apply manual code changes via client-side I/O
- CRITICAL: Manual edits make server cache stale - MUST run `list_diagnostics` afterward

### Verification
- **`list_diagnostics`**: Verify compilation errors are resolved
- **`test_project`**: Run test suite to ensure correctness and catch regressions
  - Optional parameter: `test_name` (filter tests by name/pattern)
  - CRITICAL: Running tests invalidates ALL fix IDs and diagnostic cache

## Critical Cache Invalidation Rules

**Operations that invalidate fix IDs and diagnostic cache:**
- Running `list_diagnostics`
- Applying fixes with `apply_fix`
- Running tests with `test_project`
- Any manual client-side file modifications

**After invalidation, you MUST:**
- Re-run `list_diagnostics` to refresh diagnostic cache
- Obtain new fix IDs from `get_code_actions` if needed

## Parameter Details

### list_diagnostics
- `file_path` (optional): Filter diagnostics for specific file
- `limit` (optional): Maximum number of diagnostics to return
- `force_recheck` (optional): Force fresh check instead of using cache

### get_code_actions
- `file_path` (required): Relative file path from diagnostics
- `diagnostic_message` (required): Exact diagnostic message from `list_diagnostics`

### apply_fix
- `fix_id` (required): Unique fix ID from `get_code_actions`

### list_document_symbols
- `file_path` (required): Relative path to file

### get_symbol_info
- `file_path` (required): Relative path to file containing symbol
- `line` (required): Line number from search_workspace_symbols or list_document_symbols
- `character` (required): Character position from search_workspace_symbols or list_document_symbols

### search_workspace_symbols
- `query` (required): Search term for symbol names

### test_project
- `test_name` (optional): Filter tests by name or pattern

## Success Criteria
Task completion requires:
1. `list_diagnostics` reports no compilation errors
2. `test_project` shows all tests passing
3. No regressions introduced
"###;

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
