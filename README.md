# CRAMP: Crab + MCP 🦀🤖

**An [MCP (Model Control Protocol)](https://mcp.z2x.dev/) Toolbox Service for Agentic Rust Development.**
**(Repository: [cupnfish/cramp](https://github.com/cupnfish/cramp))**

CRAMP provides a suite of 7 workflow-driven tools, exposed via the MCP protocol, designed specifically to enable LLM Agents to autonomously understand, diagnose, edit, fix, and test Rust code projects. 
It acts as a bridge between an LLM Agent and the Rust ecosystem tools (`cargo`, `rust-analyzer`).

The name comes from **Cra**b (Rust's mascot, Ferris) + **MCP**.

## Core Concept

LLM Agents often struggle with code modification tasks due to lack of context, inefficient exploration ("hallucinating" file paths, reading irrelevant files), and poor state management. 
CRAMP addresses this by:
1.  Providing a minimal, powerful set of 7 tools.
2.  Enforcing a strict, guided workflow (`Setup` -> `Diagnose` -> `Act` -> `Verify`).
3.  A central diagnostic tool (`check_project_with_context`) that combines `cargo check` errors with LSP-powered auto-fixes (with diff previews!) and API context (docs, signatures) in a single call.
4.  Providing explicit "Next Step" guidance in tool outputs.
5.  Managing state, such as invalidating cached fixes (`fix_id`) whenever the code is modified, forcing the agent to re-evaluate.
6.  Ensuring all interactions use 1-based line/column numbers and project-relative paths.

 👉 **Agent Interaction Rules:** See [`cramp_rule.md`](./cramp_rule.md) for the strict guidelines an LLM Agent MUST follow.
 
 👉 **Tool Design Spec:** See [`mcp.md`](./mcp.md) for the design specification of the 7 tools.

## Key Features

*   **MCP Protocol Implementation**: Exposes tools over Stdio, SSE, and Streamable HTTP transports.
*   **Multi-Project Workspace**: Manage, load, unload, and switch between multiple Rust projects within a single session.
*   **Context-Rich Diagnostics**: Merges `cargo check` output with `rust-analyzer` code actions and API summaries.
*   **LSP Integration**: Leverages `rust-analyzer` for code actions (auto-fixes), go-to-definition, and symbol information. Manages `rust-analyzer` processes per project.
*   **Safe File Access**: Reads/writes files, ensuring paths are strictly within the project boundary.
*   **`.gitignore` Awareness**: `get_file_tree` respects `.gitignore` rules.
*   **State Management**: Caches available fixes and correctly invalidates the cache upon any code modification.
*   **Guided Workflow**: Tool outputs include hints for the agent's most logical next action.
*    **Asynchronous**: Built with `tokio` for managing processes and communication.

## The 7 Tools

The core workflow revolves around these tools:

1.  **`manage_projects`**: (Context) Load, unload, list, and set the active project.
2.  **`check_project_with_context`**: (Diagnose) The **core intelligence hub**. Runs `cargo check` and gathers related LSP auto-fixes (with `fix_id` and `diff`) and API summaries for symbols at the error location.
3.  **`apply_fix`**: (Auto-Action) Applies an auto-fix using a `fix_id` from `check_project_with_context`. Invalidates all cached `fix_id`s.
4.  **`get_file_tree`**: (Explore) Lists project files/directories (respecting `.gitignore`).
5.  **`get_source_code`**: (Explore) Reads a file or a 1-based line range.
6.  **`apply_code_edit`**: (Manual-Action) Replaces a 1-based line range in a file with new code. Invalidates all cached `fix_id`s.
7.  **`test_project`**: (Verify) Runs `cargo test`, optionally filtering by test name.

## Architecture
CRAMP orchestrates communication between the MCP client (Agent) and underlying processes.

```
 LLM Agent <--> MCP Client
     | (Stdio / SSE / HTTP) - transport layer
     V
+---------------------------------+
| CRAMP (crates: cramp, mcp)      | MCP Server / Handler
+---------------------------------+
     | calls tools
     V
+---------------------------------+
| Toolbox (crate: lsp)            | Orchestration, State, Cache (fix_id), 7 Tools Logic
|  - servers: DashMap<RaServer>   | Path validation, 1-based <-> 0-based conversion
|  - fix_cache: DashMap<CachedFix>|
|  - fs_utils, converters         |
+---------------------------------+
     | async LSP calls      | sync blocking calls
     V                      V
+----------------+     +----------------+
| RaServer       | LSP | cargo.rs       | (crate: lsp)
| (crate: lsp)   |<--->| (spawn_blocking)
| (async comms)  | msg |                |
+----------------+     +----------------+
     | stdin/stdout         | stdin/stdout/stderr
     V                      V
[rust-analyzer process]  [cargo check / cargo test]
(per project)             (external process)
```
- **`mcp` crate**: Implements the `ServerHandler` and `#[tool]` macro logic for the MCP protocol.
- **`lsp` crate**:
    - `Toolbox`: The main orchestrator. Holds `DashMap`s for servers and the fix cache. Implements the 7 tool functions. Handles path validation and cache invalidation.
    - `RaServer`: Manages the lifecycle (start, shutdown, kill-on-drop) of a `rust-analyzer` process per project. Handles async LSP request/response/notification communication over stdio using `LspCodec`. Stores LSP diagnostics.
    - `cargo.rs`: Runs `cargo check` and `cargo test` in external processes (using `std::process::Command` within `spawn_blocking`) and parses JSON or text output.
    - `fs_utils.rs`: Handles file tree walking (`ignore` crate), reading, and writing/applying edits.
    - `converters.rs`: Handles conversions between internal models, LSP types, `cargo_metadata` types, and agent-facing models (including 0-based <-> 1-based conversion).
    - `models.rs`: Defines internal (`MyDiagnostic`, `MyApiInfo`) and agent-facing (`RichDiagnostic`, `AvailableFix`) data structures.
- **Concurrency**: Uses `tokio` for async, `dashmap` for concurrent maps, `RwLock` and `Notify` for server state.

## Prerequisites

1.  Rust toolchain installed (`rustc`, `cargo`).
2.  The `rust-analyzer` executable must be installed and available in your system's `PATH`.

## Building

```bash
git clone https://github.com/cupnfish/cramp.git
cd cramp
cargo build --release 
# The binary is in target/release/cramp
```

## Installation

You can install CRAMP using `cargo install` in several ways:

### Install from Git Repository
```bash
# Install the latest version from GitHub
cargo install --git https://github.com/cupnfish/cramp.git --bin cramp
```

### Install from Local Source
```bash
# Clone and install from local source
git clone https://github.com/cupnfish/cramp.git
cd cramp
cargo install --path crates/cramp
```

### Install from Crates.io (when published)
```bash
# Install from crates.io (future release)
cargo install cramp
```

After installation, the `cramp` binary will be available in your cargo bin directory (usually `~/.cargo/bin` on Unix-like systems or `%USERPROFILE%\.cargo\bin` on Windows). Make sure this directory is in your system's `PATH`.

To verify the installation:
```bash
cramp --help
```

## Running

CRAMP can run with different MCP transports and includes built-in documentation features.
Use `--help` for options: `cargo run -- --help` or `target/release/cramp --help`.

### Available Commands

CRAMP supports the following subcommands:

- **`stdio`** - Run MCP server using Standard Input/Output transport
- **`sse`** - Run MCP server using Server-Sent Events over HTTP
- **`stream-http`** - Run MCP server using Streamable HTTP transport
- **`doc <readme|rule>`** - View documentation interactively with syntax highlighting
- **`rule <path>`** - Export cramp_rule.md content to a specified file path

### Command Line Options

**Global Options:**
- `--log-level <LEVEL>`: Set logging level (error, warn, info, debug, trace). Default: `info`
- `--allow-env-log`: Allow RUST_LOG environment variable to override --log-level
- `--request-timeout <SECONDS>`: LSP request timeout in seconds. Default: `210`
- `--shutdown-timeout <SECONDS>`: LSP shutdown timeout in seconds. Default: `130`
- `--initial-wait <SECONDS>`: Wait time for initial indexing in seconds. Default: `125`
- `--sse-keep-alive <SECONDS>`: SSE Keep Alive in seconds for StreamHttp and SSE transports. Default: `300`
- `--stateful-mode <BOOL>`: Enable stateful mode for StreamHttp transport. Default: `true`

**Logging Examples:**
- `cargo run -- --log-level debug stdio`
- `RUST_LOG=cramp=info,lsp=debug cargo run -- --allow-env-log sse`
- `cargo run -- --log-level trace --request-timeout 300 stdio`

### Stdio
Communicates over standard input and output.
```bash
cargo run --release -- stdio
# or 
target/release/cramp stdio
```

### SSE (Server-Sent Events)
Runs an HTTP server. Default: `http://127.0.0.1:8080`.
Endpoints: `/sse` (GET, event stream), `/message` (POST, client messages).
```bash
# Default
cargo run --release -- sse 

# Custom host/port/paths
cargo run --release -- sse --host 0.0.0.0 --port 9000 --sse-path /events --message-path /send

# With custom SSE keep-alive and timeouts
cargo run --release -- sse --sse-keep-alive 600 --request-timeout 300
```

### Streamable HTTP
Runs an HTTP server with the streamable HTTP transport. Default: `http://127.0.0.1:8080`.
Base path: `/mcp`.
```bash
# Default
cargo run --release -- stream-http
 
# Custom host/port/path
cargo run --release -- stream-http --host 0.0.0.0 --port 9001 --path /api/mcp

# With stateful mode disabled and custom timeouts
cargo run --release -- stream-http --stateful-mode false --initial-wait 60
```
The service gracefully shuts down (including all managed `rust-analyzer` processes) on `Ctrl+C`.

### Documentation Commands

CRAMP includes built-in documentation viewing capabilities:

#### View Documentation Interactively
```bash
# View README.md in an interactive pager with syntax highlighting
cargo run --release -- doc readme

# View cramp_rule.md (agent interaction rules) interactively
cargo run --release -- doc rule
```

The interactive pager supports:
- **Navigation**: Use arrow keys, `j`/`k`, Page Up/Down, Home/End, or `g`/`G` to scroll
- **Commands**: Press `:` to enter command mode
  - `:copy` - Copy content to clipboard
  - `:write <filename>` - Write content to a file
- **Exit**: Press `q` to quit
- **Syntax highlighting**: Automatic markdown formatting with colors (when supported)

#### Export Rule Content
```bash
# Write the cramp_rule.md content to a specific file
cargo run --release -- rule /path/to/output/rules.md

# Example: Export rules to current directory
cargo run --release -- rule ./my_cramp_rules.md
```

This is useful for:
- Sharing agent interaction guidelines with team members
- Including rules in your project documentation
- Creating custom rule variations

## Contributing
Contributions are welcome! Please open an issue or submit a pull request.

## License
MIT / Apache 2.0
