use anyhow::{Context, Result};
use axum::Router;
use clap::{Args, Parser, Subcommand, ValueEnum};
use mcp::McpToolboxService;
use rmcp::{
    ServiceExt,
    transport::{
        StreamableHttpServerConfig,
        sse_server::{SseServer, SseServerConfig},
        stdio,
        streamable_http_server::{StreamableHttpService, session::local::LocalSessionManager},
    },
};

use std::{
    io::{self, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};
// Keep crossterm/scopeguard for doc/rule pager only
use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::{Print, ResetColor, Stylize},
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use scopeguard::defer;

use tokio_util::sync::CancellationToken;
use tracing::{error, info, instrument, level_filters::LevelFilter, warn};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

// Default constants
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_SSE_PATH: &str = "/sse";
const DEFAULT_MESSAGE_PATH: &str = "/message";
const DEFAULT_STREAM_HTTP_PATH: &str = "/mcp";
const DEFAULT_REQUEST_TIMEOUT: u64 = 210;
const DEFAULT_SHUTDOWN_TIMEOUT: u64 = 130;
const DEFAULT_INITIAL_WAIT: u64 = 125;
const DEFAULT_SSE_KEEP_ALIVE: u64 = 300;

// --- CLI Structure ---

#[derive(Parser, Debug)]
#[command(name = "cramp")]
#[command(version, about = "CRAMP: CLI Runner and Controller for MCP Toolbox Service", long_about = None)]
struct Cli {
    /// The subcommand to execute
    #[command(subcommand)]
    command: Command,

    /// Set the logging level [default: info]
    #[arg(short, long, value_enum, default_value_t = LogLevel::Info, global = true)]
    log_level: LogLevel,

    /// Allow overriding log level via RUST_LOG environment variable
    #[arg(long, default_value_t = false, global = true)]
    allow_env_log: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// View documentation (README or cramp_rule)
    Doc {
        /// Type of documentation to view
        #[arg(value_enum)]
        doc_type: DocType,
    },
    /// Rule management commands
    #[command(subcommand)]
    Rule(RuleCommand),
    #[command(flatten)]
    TransportCommand(TransportCommand),
}

#[derive(Args, Debug, Clone)]
struct ServeArgs {
    /// Timeout duration (in seconds) for individual requests
    #[arg(long, default_value_t = DEFAULT_REQUEST_TIMEOUT)]
    pub request_timeout: u64,

    /// Timeout duration (in seconds) for graceful shutdown
    #[arg(long, default_value_t = DEFAULT_SHUTDOWN_TIMEOUT)]
    pub shutdown_timeout: u64,

    /// Initial waiting period (in seconds) before service starts processing requests
    #[arg(long, default_value_t = DEFAULT_INITIAL_WAIT)]
    pub initial_wait: u64,
}

#[derive(Subcommand, Debug, Clone)]
enum RuleCommand {
    /// Write rule content to custom file path
    Export {
        /// Path where the rule content will be written
        path: PathBuf,
    },
    /// Write rule content to Trae IDE rules directory
    Trae,
    /// Write rule content to Cursor IDE rules directory
    Cursor,
    /// Write rule content to VS Code workspace settings
    Vscode,
    /// Write rule content to Zed IDE settings
    Zed,
}

#[derive(Subcommand, Debug, Clone)]
enum TransportCommand {
    /// Run with stdio transport
    Stdio {
        /// Path to the active project
        project_path: PathBuf,
        #[command(flatten)]
        args: ServeArgs,
    },
    /// Run with SSE transport
    Sse {
        /// Path to the active project
        project_path: PathBuf,
        /// Host address to bind the SSE server to
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        /// Port number to bind the SSE server to
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Path for SSE event stream endpoint
        #[arg(long, default_value = DEFAULT_SSE_PATH)]
        sse_path: String,
        /// Path for message posting endpoint
        #[arg(long, default_value = DEFAULT_MESSAGE_PATH)]
        message_path: String,
        /// SSE keep-alive interval in seconds
        #[arg(long, default_value_t = DEFAULT_SSE_KEEP_ALIVE)]
        sse_keep_alive: u64,
        #[command(flatten)]
        args: ServeArgs,
    },
    /// Run with StreamHttp transport
    StreamHttp {
        /// Path to the active project
        project_path: PathBuf,
        /// Host address to bind the HTTP server to
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        /// Port number to bind the HTTP server to
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Base path for streamable HTTP endpoints
        #[arg(long, default_value = DEFAULT_STREAM_HTTP_PATH)]
        path: String,
        /// SSE keep-alive interval in seconds
        #[arg(long, default_value_t = DEFAULT_SSE_KEEP_ALIVE)]
        sse_keep_alive: u64,
        /// Whether to run in stateful mode (maintain session state)
        #[arg(long, default_value_t = true)]
        stateful_mode: bool,
        #[command(flatten)]
        args: ServeArgs,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}
impl From<LogLevel> for LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => LevelFilter::ERROR,
            LogLevel::Warn => LevelFilter::WARN,
            LogLevel::Info => LevelFilter::INFO,
            LogLevel::Debug => LevelFilter::DEBUG,
            LogLevel::Trace => LevelFilter::TRACE,
        }
    }
}
impl From<LogLevel> for tracing_subscriber::filter::Directive {
    fn from(level: LogLevel) -> Self {
        LevelFilter::from(level).into()
    }
}
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum DocType {
    Readme,
    Rule,
}

// --- Logging ---
fn init_logging(level: LogLevel, allow_env: bool) {
    let filter = if allow_env && std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else {
        EnvFilter::builder()
            .with_default_directive(level.into())
            .from_env_lossy()
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::Layer::new()
                .with_writer(std::io::stderr)
                .with_ansi(true),
        )
        .init();
}

// --- Main ---
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.log_level, cli.allow_env_log);

    match cli.command {
        Command::TransportCommand(transport) => run_server(transport).await,
        Command::Doc { doc_type } => show_documentation(doc_type),
        Command::Rule(rule_cmd) => handle_rule_command(rule_cmd),
    }
}

// --- Server Runner ---
async fn run_server(transport: TransportCommand) -> Result<()> {
    info!("Starting CRAMP MCP Toolbox Service...");

    let (project_path, args) = match &transport {
        TransportCommand::Stdio { project_path, args } => (project_path.clone(), args.clone()),
        TransportCommand::Sse {
            project_path, args, ..
        } => (project_path.clone(), args.clone()),
        TransportCommand::StreamHttp {
            project_path, args, ..
        } => (project_path.clone(), args.clone()),
    };

    let final_args = args;

    let service = Arc::new(McpToolboxService::with_timeouts(
        final_args.request_timeout,
        final_args.shutdown_timeout,
        final_args.initial_wait,
    ));

    // Initialize the service with the project path
    info!("Initializing service with project path: {:?}", project_path);
    let canonical_path = project_path
        .canonicalize()
        .map_err(|e| warn!("Failed to canonicalize path: {}", e))
        .ok();
    let project_name = canonical_path
        .as_ref()
        .and_then(|p| {
            p.file_name()
                .and_then(|name| name.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| {
            warn!("Could not determine project name from path, using default");
            "project".to_string()
        });
    if let Err(e) = service
        .toolbox
        .initialize_project(project_name, canonical_path.unwrap_or(project_path))
        .await
    {
        error!("Failed to initialize project: {}", e);
        return Err(e.into());
    }

    // Wait for initial indexing to complete before starting MCP service
    info!("Waiting for rust-analyzer indexing to complete...");
    if let Err(e) = service.toolbox.wait_for_indexing_complete().await {
        error!("Failed to wait for indexing completion: {}", e);
        return Err(e.into());
    }

    info!("Project initialized successfully");

    let shutdown_token = CancellationToken::new();

    let ctrl_c_token = shutdown_token.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            warn!("Ctrl-C signal received, initiating graceful shutdown...");
        } else {
            error!("Failed to listen for Ctrl-C signal.");
        }
        ctrl_c_token.cancel();
    });

    let server_result = match transport {
        TransportCommand::Stdio { .. } => run_stdio(service.clone(), shutdown_token.clone()).await,
        TransportCommand::Sse {
            host,
            port,
            sse_path,
            message_path,
            sse_keep_alive,
            ..
        } => {
            run_http_server(
                service.clone(),
                shutdown_token.clone(),
                &host,
                port,
                |s| {
                    create_sse_router(
                        s,
                        shutdown_token.clone(),
                        &sse_path,
                        &message_path,
                        sse_keep_alive,
                    )
                },
                "SSE",
            )
            .await
        }
        TransportCommand::StreamHttp {
            host,
            port,
            path,
            sse_keep_alive,
            stateful_mode,
            ..
        } => {
            run_http_server(
                service.clone(),
                shutdown_token.clone(),
                &host,
                port,
                |s| create_stream_http_router(s, &path, sse_keep_alive, stateful_mode),
                "Streamable HTTP",
            )
            .await
        }
    };

    if let Err(e) = &server_result {
        error!("Server error: {:?}", e);
        shutdown_token.cancel();
    }

    info!("Tasks stopped, cleaning up LSP resources...");
    service.shutdown().await;
    info!("Cleanup complete. Exiting.");
    server_result
}

// --- Transport Runners ---

#[instrument(skip_all)]
async fn run_stdio(service: Arc<McpToolboxService>, token: CancellationToken) -> Result<()> {
    info!("Transport: Standard I/O. Connect client to stdin/stdout.");

    let stdio_future = async {
        let running_service = (*service)
            .clone()
            .serve_with_ct(stdio(), token.clone())
            .await
            .context("Failed to start stdio service")?;
        let reason = running_service.waiting().await?;
        info!(?reason, "Stdio service finished");
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
         res = stdio_future => res.context("Stdio error"),
          _ = token.cancelled() => { info!("Stdio shutdown requested."); Ok(())}
    }
}

// Generic HTTP Server Runner
#[instrument(skip_all)]
async fn run_http_server<F>(
    service: Arc<McpToolboxService>,
    token: CancellationToken,
    host: &str,
    port: u16,
    router_factory: F,
    transport_name: &'static str,
) -> Result<()>
where
    F: FnOnce(Arc<McpToolboxService>) -> Router,
{
    let addr_str = format!("{}:{}", host, port);
    let addr = SocketAddr::from_str(&addr_str)?;
    info!("Transport: {}", transport_name);
    info!("Listening on: http://{}", addr);
    info!("Press Ctrl+C to exit.");

    let app = router_factory(service);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to address {}", addr))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(token.clone().cancelled_owned())
        .await
        .context(format!("Axum {} server error", transport_name))?;

    info!("{} server finished", transport_name);
    Ok(())
}

// Router Factories
fn create_sse_router(
    service: Arc<McpToolboxService>,
    token: CancellationToken,
    sse_path: &str,
    message_path: &str,
    keep_alive: u64,
) -> Router {
    info!("SSE endpoint path: {}", sse_path);
    info!("Message endpoint path: {}", message_path);
    let config = SseServerConfig {
        bind: "0.0.0.0:0".parse().unwrap(), // Bind address handled by run_http_server
        sse_path: sse_path.to_string(),
        post_path: message_path.to_string(),
        ct: token.clone(),
        sse_keep_alive: Some(Duration::from_secs(keep_alive)),
    };
    let (sse_server, sse_router) = SseServer::new(config);
    sse_server.with_service(move || (*service).clone());
    sse_router
}

fn create_stream_http_router(
    service: Arc<McpToolboxService>,
    path: &str,
    keep_alive: u64,
    stateful: bool,
) -> Router {
    info!("Streamable HTTP base path: {}", path);
    info!("Stateful mode: {}", stateful);
    let http_service = StreamableHttpService::new(
        move || (*service).clone(),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig {
            sse_keep_alive: Some(Duration::from_secs(keep_alive)),
            stateful_mode: stateful,
        },
    );
    Router::new().nest_service(path, http_service)
}

// +----------------------------------------------------------------++
// | --- Doc / Rule Functions (Kept from original) ------------------ |
// +-----------------------------------------------------------------+
fn show_documentation(doc_type: DocType) -> Result<()> {
    info!("Displaying documentation: {:?}", doc_type);
    let (content, display_name, file_name) = match doc_type {
        DocType::Readme => (assets::get_readme(), "README", "README.md"),
        DocType::Rule => (assets::get_rule(), "CRAMP Rules", "cramp_rule.md"),
    };
    use std::io::IsTerminal;
    let use_colors = std::io::stderr().is_terminal();
    let formatted_content = if use_colors {
        format_markdown_with_colors(content, display_name)
    } else {
        format_markdown_plain(content, display_name)
    };
    run_interactive_pager(&formatted_content, content, file_name)?;
    Ok(())
}

fn format_markdown_with_colors(content: &str, display_name: &str) -> String {
    let mut output = String::new();
    output.push_str(&format!("{}\n", display_name.bold().cyan()));
    output.push_str(&format!("{}\n", "=".repeat(display_name.len()).blue()));
    for line in content.lines() {
        if line.starts_with("# ") {
            output.push_str(&format!("{}\n", line.green().bold()));
        } else if line.starts_with("## ") {
            output.push_str(&format!("{}\n", line.yellow().bold()));
        } else if line.starts_with("- ") {
            output.push_str(&format!("{}{}\n", " • ".cyan(), &line[2..]));
        } else {
            output.push_str(&format!("{}\n", line));
        }
    }
    output
}
fn format_markdown_plain(content: &str, display_name: &str) -> String {
    format!(
        "{}\n{}\n{}",
        display_name,
        "=".repeat(display_name.len()),
        content
    )
}

fn run_interactive_pager(
    formatted_content: &str,
    raw_content: &str,
    file_name: &str,
) -> Result<()> {
    use crossterm::event::poll;
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, Hide)?;
    defer! {
         let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
         let _ = disable_raw_mode();
    }
    let lines: Vec<&str> = formatted_content.lines().collect();
    let mut scroll_offset = 0;
    let mut command_mode = false;
    let mut command_input = String::new();
    let mut status_message = String::new();
    let mut status_expiry = std::time::Instant::now();
    loop {
        let (cols, terminal_height) = crossterm::terminal::size()?;
        let display_height = (terminal_height as usize).saturating_sub(1);
        let max_scroll = lines.len().saturating_sub(display_height);
        scroll_offset = scroll_offset.min(max_scroll);
        execute!(io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
        for i in 0..display_height {
            if let Some(line) = lines.get(scroll_offset + i) {
                execute!(io::stdout(), Print(line), Print("\r\n"))?;
            } else {
                execute!(io::stdout(), Print("~\r\n"))?;
            }
        }
        let status = if command_mode {
            format!(":{}", command_input)
        } else if !status_message.is_empty() && std::time::Instant::now() < status_expiry {
            status_message.clone()
        } else {
            let percentage = if max_scroll == 0 {
                100
            } else {
                (scroll_offset * 100) / max_scroll.max(1)
            };
            format!(
                "{}% | q:quit | ↑↓/jk:scroll | :command (copy, write <file>)",
                percentage
            )
        };
        let status_line = format!("{:<width$}", status, width = cols as usize);
        execute!(
            io::stdout(),
            MoveTo(0, terminal_height - 1),
            Print(status_line.on_blue()),
            ResetColor
        )?;
        if command_mode {
            execute!(
                io::stdout(),
                Show,
                MoveTo((command_input.len() + 1) as u16, terminal_height - 1)
            )?;
        } else {
            execute!(io::stdout(), Hide)?;
        }
        io::stdout().flush()?;
        if !poll(Duration::from_millis(100))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            status_message.clear();
            if command_mode {
                match key.code {
                    KeyCode::Enter => {
                        match execute_pager_command(&command_input, raw_content, file_name) {
                            Ok(msg) => status_message = msg,
                            Err(e) => status_message = format!("Error: {}", e),
                        }
                        status_expiry = std::time::Instant::now() + Duration::from_secs(2);
                        command_mode = false;
                        command_input.clear();
                    }
                    KeyCode::Esc => {
                        command_mode = false;
                        command_input.clear();
                    }
                    KeyCode::Backspace => {
                        command_input.pop();
                    }
                    KeyCode::Char(c) => command_input.push(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char(':') => {
                        command_mode = true;
                        command_input.clear();
                    }
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        scroll_offset = (scroll_offset + 1).min(max_scroll)
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        scroll_offset = scroll_offset.saturating_sub(1)
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        scroll_offset = (scroll_offset + display_height).min(max_scroll)
                    }
                    KeyCode::PageUp => scroll_offset = scroll_offset.saturating_sub(display_height),
                    KeyCode::Home | KeyCode::Char('g') => scroll_offset = 0,
                    KeyCode::End | KeyCode::Char('G') => scroll_offset = max_scroll,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
fn execute_pager_command(command: &str, content: &str, _file_name: &str) -> Result<String> {
    let command = command.trim();
    if command == "copy" {
        copy_to_clipboard_impl(content)?;
        return Ok("Content copied to clipboard!".to_string());
    } else if let Some(filename) = command.strip_prefix("write ") {
        let filename = filename.trim();
        if filename.is_empty() {
            return Err(anyhow::anyhow!("Filename cannot be empty"));
        }
        std::fs::write(filename, content)?;
        return Ok(format!("Content written to '{}'", filename));
    }
    Err(anyhow::anyhow!(
        "Unknown command: '{}'. Use: copy, write <file>",
        command
    ))
}
fn copy_to_clipboard_impl(content: &str) -> Result<()> {
    use arboard::Clipboard;
    Clipboard::new()?.set_text(content)?;
    Ok(())
}
fn handle_rule_command(rule_cmd: RuleCommand) -> Result<()> {
    match rule_cmd {
        RuleCommand::Export { path } => write_rule_to_custom_path(&path),
        RuleCommand::Trae => write_rule_to_ide_path(".trae/rules/project_rules.md", "Trae"),
        RuleCommand::Cursor => write_rule_to_ide_path(".cursorrules", "Cursor"),
        RuleCommand::Vscode => write_rule_to_vscode(),
        RuleCommand::Zed => write_rule_to_ide_path(".zed/rules.md", "Zed"),
    }
}

fn write_rule_to_custom_path(path: &Path) -> Result<()> {
    use std::fs;
    let rule_content = assets::get_rule();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Failed to create dir {:?}", parent))?;
    }
    fs::write(path, rule_content).with_context(|| format!("Failed to write to {:?}", path))?;
    info!("Rule content written to: {:?}", path);
    Ok(())
}

fn write_rule_to_ide_path(target_path_str: &str, ide_name: &str) -> Result<()> {
    use std::fs;
    use std::path::Path;

    let rule_content = assets::get_rule();
    let target_path = Path::new(target_path_str);

    // Create directory if it doesn't exist
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Failed to create dir {:?}", parent))?;
    }

    // Check if file exists and has different content
    if target_path.exists() {
        let existing_content = fs::read_to_string(target_path)
            .with_context(|| format!("Failed to read existing file {:?}", target_path))?;

        if existing_content != rule_content {
            // Create backup with timestamp
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let backup_path = format!("{}.backup.{}", target_path_str, timestamp);

            fs::copy(target_path, &backup_path)
                .with_context(|| format!("Failed to create backup at {}", backup_path))?;

            info!(
                "Existing {} rules file backed up to: {}",
                ide_name, backup_path
            );
        } else {
            info!(
                "{} rules file content is identical, no changes needed.",
                ide_name
            );
            return Ok(());
        }
    }

    // Write the new content
    fs::write(target_path, rule_content)
        .with_context(|| format!("Failed to write to {:?}", target_path))?;

    info!(
        "Rule content written to {} rules file: {:?}",
        ide_name, target_path
    );
    Ok(())
}

fn write_rule_to_vscode() -> Result<()> {
    use std::fs;
    use std::path::Path;

    let rule_content = assets::get_rule();
    let vscode_dir = Path::new(".vscode");
    let settings_path = vscode_dir.join("settings.json");

    // Create .vscode directory if it doesn't exist
    fs::create_dir_all(vscode_dir).with_context(|| "Failed to create .vscode directory")?;

    // Read existing settings or create new ones
    let mut settings = if settings_path.exists() {
        let content = fs::read_to_string(&settings_path)
            .with_context(|| "Failed to read existing VS Code settings")?;
        serde_json::from_str::<serde_json::Value>(&content)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Add or update the cramp rules in settings
    if let Some(obj) = settings.as_object_mut() {
        obj.insert(
            "cramp.rules".to_string(),
            serde_json::Value::String(rule_content.to_string()),
        );
    }

    // Write back to settings.json
    let settings_str = serde_json::to_string_pretty(&settings)
        .with_context(|| "Failed to serialize VS Code settings")?;

    fs::write(&settings_path, settings_str).with_context(|| "Failed to write VS Code settings")?;

    info!(
        "Rule content written to VS Code settings: {:?}",
        settings_path
    );
    Ok(())
}
