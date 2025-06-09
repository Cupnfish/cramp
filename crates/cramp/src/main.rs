use anyhow::Context;
use axum::Router;
use clap::{Parser, Subcommand, ValueEnum};
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
use std::{net::SocketAddr, path::{Path, PathBuf}, str::FromStr, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use tracing::{Level, error, info, warn};
use tracing_subscriber::{
    EnvFilter, filter::LevelFilter, layer::SubscriberExt, util::SubscriberInitExt,
};

// Default constants
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8080;
const DEFAULT_SSE_PATH: &str = "/sse";
const DEFAULT_MESSAGE_PATH: &str = "/message";
const DEFAULT_STREAM_HTTP_PATH: &str = "/mcp";

#[derive(Parser, Debug)]
#[command(name = "cramp")]
#[command(version, about = "CRAMP: CLI Runner for MCP Toolbox Service", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: TransportCommand,

    /// Set the logging level
    #[arg(short, long, value_enum, default_value_t = LogLevel::Info, global = true)]
    log_level: LogLevel,

    /// Override log level with RUST_LOG environment variable if set
    #[arg(
        long,
        default_value_t = false,
        global = true,
        help = "Allow RUST_LOG env var to override --log-level"
    )]
    allow_env_log: bool,

    /// LSP request timeout in seconds
    #[arg(
        long,
        default_value_t = 210,
        global = true,
        help = "Timeout for LSP requests in seconds"
    )]
    request_timeout: u64,

    /// LSP shutdown timeout in seconds
    #[arg(
        long,
        default_value_t = 130,
        global = true,
        help = "Timeout for LSP shutdown in seconds"
    )]
    shutdown_timeout: u64,

    /// Initial indexing wait time in seconds
    #[arg(
        long,
        default_value_t = 125,
        global = true,
        help = "Wait time for initial indexing in seconds"
    )]
    initial_wait: u64,

    /// SSE Keep Alive in seconds for StreamHttp and SSE transports
    #[arg(
        long,
        default_value_t = 300,
        global = true,
        help = "SSE Keep Alive in seconds"
    )]
    sse_keep_alive: u64,

    /// Enable stateful mode for StreamHttp transport
    #[arg(
        long,
        default_value_t = true,
        global = true,
        help = "Enable stateful mode for StreamHttp"
    )]
    stateful_mode: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LogLevel {
    /// Only errors
    Error,
    /// Errors and warnings
    Warn,
    /// Errors, warnings, and informational messages (default)
    Info,
    /// Everything up to debug messages
    Debug,
    /// Maximum verbosity
    Trace,
}
// Convert LogLevel to tracing LevelFilter
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
// Convert LogLevel to tracing Level
impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Level::ERROR,
            LogLevel::Warn => Level::WARN,
            LogLevel::Info => Level::INFO,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Trace => Level::TRACE,
        }
    }
}

#[derive(Subcommand, Debug, Clone)]
enum TransportCommand {
    /// Run server using Standard Input/Output
    Stdio,
    /// Run server using Server-Sent Events (SSE) over HTTP
    Sse {
        #[arg(long, default_value = DEFAULT_HOST, help = "Host address to bind to")]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT, help = "Port to listen on")]
        port: u16,
        #[arg(long, default_value = DEFAULT_SSE_PATH, help = "Path for the SSE connection endpoint")]
        sse_path: String,
        #[arg(long, default_value = DEFAULT_MESSAGE_PATH, help = "Path for the POST message endpoint")]
        message_path: String,
    },
    /// Run server using Streamable HTTP transport
    StreamHttp {
        #[arg(long, default_value = DEFAULT_HOST, help = "Host address to bind to")]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT, help = "Port to listen on")]
        port: u16,
        #[arg(long, default_value = DEFAULT_STREAM_HTTP_PATH, help = "Base path for streamable HTTP service")]
        path: String,
    },
    /// View documentation (README or cramp_rule) in interactive mode
    Doc {
        /// Document to view: 'readme' or 'rule'
        #[arg(value_enum, help = "Document to view")]
        doc_type: DocType,
    },
    /// Write rule content to specified file path
    Rule {
        /// Path where to write the rule content
        #[arg(help = "File path to write the rule content")]
        path: PathBuf,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum DocType {
    /// View README.md
    Readme,
    /// View cramp_rule.md
    Rule,
}

/// Initialize tracing subscriber
fn init_logging(level: LogLevel, allow_env: bool) {
    let filter = if allow_env && std::env::var("RUST_LOG").is_ok() {
        EnvFilter::from_default_env()
    } else {
        // Set default level, but still allow RUST_LOG=none or specific module overrides
        let level_filter: LevelFilter = level.into();
        EnvFilter::builder()
            .with_default_directive(level_filter.into())
            .from_env_lossy()
    };

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr)) // Log to stderr
        .init();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    init_logging(cli.log_level, cli.allow_env_log);

    info!("Starting CRAMP MCP Toolbox Service...");

    // Create a single service instance wrapped in Arc with timeout configuration
    let service = Arc::new(McpToolboxService::with_timeouts(
        cli.request_timeout,
        cli.shutdown_timeout,
        cli.initial_wait,
    ));
    // Create a token to signal shutdown
    let shutdown_token = CancellationToken::new();

    // --- Handle Ctrl+C ---
    let ctrl_c_token = shutdown_token.clone();
    tokio::spawn(async move {
        match tokio::signal::ctrl_c().await {
            Ok(_) => {
                warn!("Ctrl-C signal received, initiating graceful shutdown...");
                ctrl_c_token.cancel();
            }
            Err(e) => {
                error!("Failed to listen for Ctrl-C signal: {}", e);
                // Still try to shut down if signal listener fails
                ctrl_c_token.cancel();
            }
        }
    });
    // --- End Ctrl+C Handler ---

    let server_result = match cli.command {
        TransportCommand::Stdio => run_stdio(service.clone(), shutdown_token.clone()).await,
        TransportCommand::Sse {
            host,
            port,
            sse_path,
            message_path,
        } => {
            run_sse(
                service.clone(),
                shutdown_token.clone(),
                &host,
                port,
                &sse_path,
                &message_path,
                cli.sse_keep_alive,
            )
            .await
        }
        TransportCommand::StreamHttp { host, port, path } => {
            run_stream_http(
                service.clone(),
                shutdown_token.clone(),
                &host,
                port,
                &path,
                cli.sse_keep_alive,
                cli.stateful_mode,
            )
            .await
        }
        TransportCommand::Doc { doc_type } => {
            show_documentation(doc_type)?;
            return Ok(());
        }
        TransportCommand::Rule { path } => {
            write_rule_to_file(&path)?;
            return Ok(());
        }
    };

    // Wait for the server task to actually finish before cleaning up
    if let Err(e) = server_result {
        error!("Server error: {:?}", e);
        // Ensure token is cancelled even if server failed to start or run
        shutdown_token.cancel();
    }

    // --- Cleanup: Shutdown the underlying LSP servers ---
    // This MUST happen after the server loop (axum::serve or waiting()) has terminated
    info!("Server stopped, cleaning up LSP resources...");
    service.shutdown().await;
    info!("Cleanup complete. Exiting.");
    // --- End Cleanup ---

    Ok(())
}

// --- Transport Runners ---

async fn run_stdio(
    service: Arc<McpToolboxService>,
    token: CancellationToken,
) -> anyhow::Result<()> {
    info!("Transport: Standard I/O");
    info!("Connect client to stdin/stdout. Press Ctrl+C to exit.");
    // The token passed here will cause `waiting()` to return when cancelled
    let running_service = (*service)
        .clone()
        .serve_with_ct(stdio(), token)
        .await
        .context("Failed to start stdio service")?;

    // waiting() returns when the token is cancelled or the stream closes
    let reason = running_service.waiting().await?;
    info!(?reason, "Stdio service finished");
    Ok(())
}

async fn run_sse(
    service: Arc<McpToolboxService>,
    token: CancellationToken,
    host: &str,
    port: u16,
    sse_path: &str,
    message_path: &str,
    sse_keep_alive_secs: u64,
) -> anyhow::Result<()> {
    let addr_str = format!("{}:{}", host, port);
    let addr = SocketAddr::from_str(&addr_str)
        .with_context(|| format!("Invalid address format: {}", addr_str))?;

    info!("Transport: Server-Sent Events (SSE)");
    info!("Listening on: http://{}", addr);
    info!("SSE endpoint: http://{}{}", addr, sse_path);
    info!("Message endpoint: http://{}{}", addr, message_path);
    info!("Press Ctrl+C to exit.");

    let config = SseServerConfig {
        bind: addr,
        sse_path: sse_path.to_string(),
        post_path: message_path.to_string(),
        // Pass the token to the SSE server for its own internal shutdown logic
        ct: token.clone(),
        sse_keep_alive: Some(Duration::from_secs(sse_keep_alive_secs)),
    };

    let (sse_server, sse_router) = SseServer::new(config);

    // Provide a factory closure that clones the Arc and dereferences it
    sse_server.with_service(move || (*service).clone());

    let app = Router::new().merge(sse_router);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to address {}", addr))?;

    // Axum graceful shutdown just waits for the token
    axum::serve(listener, app)
        .with_graceful_shutdown(token.cancelled_owned())
        .await
        .context("Axum SSE server error")?;

    info!("SSE server finished");
    Ok(())
}

async fn run_stream_http(
    service: Arc<McpToolboxService>,
    token: CancellationToken,
    host: &str,
    port: u16,
    path: &str,
    sse_keep_alive_secs: u64,
    stateful_mode: bool,
) -> anyhow::Result<()> {
    let addr_str = format!("{}:{}", host, port);
    let addr = SocketAddr::from_str(&addr_str)
        .with_context(|| format!("Invalid address format: {}", addr_str))?;

    info!("Transport: Streamable HTTP");
    info!("Listening on: http://{}", addr);
    info!("Service endpoint: http://{}{}", addr, path);
    info!("Press Ctrl+C to exit.");

    // Provide a factory closure that clones the Arc
    let http_service = StreamableHttpService::new(
        move || (*service).clone(),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig {
            sse_keep_alive: Some(Duration::from_secs(sse_keep_alive_secs)),
            stateful_mode: stateful_mode,
        },
    );

    let router = axum::Router::new().nest_service(path, http_service);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind to address {}", addr))?;

    // Axum graceful shutdown just waits for the token
    axum::serve(listener, router)
        .with_graceful_shutdown(token.cancelled_owned())
        .await
        .context("Axum Streamable HTTP server error")?;

    info!("Streamable HTTP server finished");
    Ok(())
}

/// Display documentation content with enhanced markdown rendering
fn show_documentation(doc_type: DocType) -> anyhow::Result<()> {
    let (content, display_name, file_name) = match doc_type {
        DocType::Readme => {
            let content = include_str!("../../../README.md");
            (content, "README", "README.md")
        }
        DocType::Rule => {
            let content = include_str!("../../../cramp_rule.md");
            (content, "CRAMP Rules", "cramp_rule.md")
        }
    };

    // Check if we're in a TTY to determine if we should use colors
    use std::io::IsTerminal;
    let use_colors = std::io::stderr().is_terminal();

    // Always use interactive pager mode
    let formatted_content = if use_colors {
        format_markdown_with_colors(content, display_name)
    } else {
        format_markdown_plain(content, display_name)
    };
    run_interactive_pager(&formatted_content, content, file_name)?;

    Ok(())
}

/// Format markdown with ANSI colors (similar to mcat)
fn format_markdown_with_colors(content: &str, display_name: &str) -> String {
    let mut output = String::new();

    // ANSI color constants
    const RESET: &str = "\x1B[0m";
    const BOLD: &str = "\x1B[1m";
    const FG_BLUE: &str = "\x1B[34m";
    const FG_GREEN: &str = "\x1B[32m";
    const FG_YELLOW: &str = "\x1B[33m";
    const FG_CYAN: &str = "\x1B[36m";
    const FG_MAGENTA: &str = "\x1B[35m";
    const BG_BLUE: &str = "\x1B[44m";

    // Header
    output.push_str(&format!(
        "{}{}📖 {} Documentation{}",
        BOLD, FG_CYAN, display_name, RESET
    ));
    output.push_str(&format!(
        "\n{}{}═══════════════════════════════════════{}",
        BOLD, FG_BLUE, RESET
    ));
    output.push('\n');

    // Process content line by line for basic markdown formatting
    for line in content.lines() {
        if line.starts_with("# ") {
            // H1 headers
            let header = &line[2..];
            output.push_str(&format!("{}{}{}{}", BOLD, FG_GREEN, header, RESET));
        } else if line.starts_with("## ") {
            // H2 headers
            let header = &line[3..];
            output.push_str(&format!("{}{}{}{}", BOLD, FG_YELLOW, header, RESET));
        } else if line.starts_with("### ") {
            // H3 headers
            let header = &line[4..];
            output.push_str(&format!("{}{}{}{}", BOLD, FG_MAGENTA, header, RESET));
        } else if line.starts_with("- ") || line.starts_with("* ") {
            // List items
            output.push_str(&format!("{}{}{}{}", FG_CYAN, "• ", &line[2..], RESET));
        } else if line.starts_with("`") && line.ends_with("`") {
            // Inline code
            output.push_str(&format!("{}{}{}{}", BG_BLUE, line, RESET, RESET));
        } else {
            // Regular text
            output.push_str(line);
        }
        output.push('\n');
    }

    output
}

/// Format markdown as plain text
fn format_markdown_plain(content: &str, display_name: &str) -> String {
    let mut output = String::new();

    // Header
    output.push_str(&format!("📖 {} Documentation", display_name));
    output.push_str("\n═══════════════════════════════════════\n");

    // Content (remove markdown formatting for plain text)
    for line in content.lines() {
        if line.starts_with("# ") {
            output.push_str(&line[2..]);
        } else if line.starts_with("## ") {
            output.push_str(&line[3..]);
        } else if line.starts_with("### ") {
            output.push_str(&line[4..]);
        } else if line.starts_with("- ") || line.starts_with("* ") {
            output.push_str(&format!("• {}", &line[2..]));
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }

    output
}

/// Interactive pager implementation using crossterm with command support
fn run_interactive_pager(
    formatted_content: &str,
    raw_content: &str,
    file_name: &str,
) -> anyhow::Result<()> {
    use crossterm::{
        cursor,
        event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
        execute,
        style::Print,
        terminal::{self, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use std::io::{self, Write};

    // Enable raw mode and enter alternate screen
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, cursor::Hide)?;

    let lines: Vec<&str> = formatted_content.lines().collect();
    let (_, terminal_height) = terminal::size()?;
    let display_height = (terminal_height as usize).saturating_sub(1); // Reserve one line for status

    let mut scroll_offset = 0;
    let max_scroll = lines.len().saturating_sub(display_height);
    let mut command_mode = false;
    let mut command_input = String::new();
    let mut needs_redraw = true;

    // Helper function to draw the screen
    let draw_screen = |scroll_offset: usize, command_mode: bool, command_input: &str| -> anyhow::Result<()> {
        // Clear screen and display content
        execute!(
            io::stdout(),
            terminal::Clear(ClearType::All),
            cursor::MoveTo(0, 0)
        )?;

        // Display visible lines
        for i in 0..display_height {
            let line_index = scroll_offset + i;
            if line_index < lines.len() {
                execute!(io::stdout(), Print(lines[line_index]), Print("\r\n"))?;
            } else {
                execute!(io::stdout(), Print("~\r\n"))?;
            }
        }

        // Display status line
        let status = if command_mode {
            format!(":{}", command_input)
        } else if lines.len() <= display_height {
            "(END) - Press 'q' to quit, ':' for commands".to_string()
        } else {
            let percentage = if max_scroll == 0 {
                100
            } else {
                (scroll_offset * 100) / max_scroll
            };
            format!(
                "{}% - Use ↑↓/jk to scroll, 'q' to quit, ':' for commands",
                percentage
            )
        };

        execute!(
            io::stdout(),
            cursor::MoveTo(0, terminal_height - 1),
            terminal::Clear(ClearType::CurrentLine),
            Print(&format!("\x1B[7m{}\x1B[0m", status)) // Reverse video for status line
        )?;

        // Position cursor for command input
        if command_mode {
            execute!(
                io::stdout(),
                cursor::MoveTo((command_input.len() + 1) as u16, terminal_height - 1),
                cursor::Show
            )?;
        } else {
            execute!(io::stdout(), cursor::Hide)?;
        }

        io::stdout().flush()?;
        Ok(())
    };

    loop {
        // Only redraw if needed
        if needs_redraw {
            draw_screen(scroll_offset, command_mode, &command_input)?;
            needs_redraw = false;
        }

        // Handle input - block until we get an event
        if let Event::Key(key) = event::read()? {
            // Only process key press events, ignore release and repeat
            if key.kind != KeyEventKind::Press {
                continue;
            }

                if command_mode {
                    // Handle command mode input
                    match key {
                        KeyEvent {
                            code: KeyCode::Enter,
                            ..
                        } => {
                            // Execute command
                            if let Err(e) = execute_command(&command_input, raw_content, file_name)
                            {
                                // Show error message briefly
                                execute!(
                                    io::stdout(),
                                    cursor::MoveTo(0, terminal_height - 1),
                                    terminal::Clear(ClearType::CurrentLine),
                                    Print(&format!("\x1B[7mError: {}\x1B[0m", e))
                                )?;
                                io::stdout().flush()?;
                                std::thread::sleep(std::time::Duration::from_millis(2000));
                            }
                            command_mode = false;
                            command_input.clear();
                            needs_redraw = true;
                        }
                        KeyEvent {
                            code: KeyCode::Esc, ..
                        } => {
                            command_mode = false;
                            command_input.clear();
                            needs_redraw = true;
                        }
                        KeyEvent {
                            code: KeyCode::Backspace,
                            ..
                        } => {
                            command_input.pop();
                            needs_redraw = true;
                        }
                        KeyEvent {
                            code: KeyCode::Char(c),
                            ..
                        } => {
                            command_input.push(c);
                            needs_redraw = true;
                        }
                        _ => {}
                    }
                } else {
                    // Handle normal mode input
                    match key {
                        // Enter command mode - handle colon key specifically
                        KeyEvent {
                            code: KeyCode::Char(':' | '：'),
                            ..
                        } => {
                            command_mode = true;
                            command_input.clear();
                            needs_redraw = true;
                        }
                        // Quit
                        KeyEvent {
                            code: KeyCode::Char('q'),
                            modifiers: KeyModifiers::NONE,
                            ..
                        }
                        | KeyEvent {
                            code: KeyCode::Esc, ..
                        } => break,

                        // Scroll down
                        KeyEvent {
                            code: KeyCode::Down,
                            ..
                        }
                        | KeyEvent {
                            code: KeyCode::Char('j'),
                            modifiers: KeyModifiers::NONE,
                            ..
                        } => {
                            if scroll_offset < max_scroll {
                                scroll_offset += 1;
                                needs_redraw = true;
                            }
                        }

                        // Scroll up
                        KeyEvent {
                            code: KeyCode::Up, ..
                        }
                        | KeyEvent {
                            code: KeyCode::Char('k'),
                            modifiers: KeyModifiers::NONE,
                            ..
                        } => {
                            if scroll_offset > 0 {
                                scroll_offset -= 1;
                                needs_redraw = true;
                            }
                        }

                        // Page down
                        KeyEvent {
                            code: KeyCode::PageDown,
                            ..
                        }
                        | KeyEvent {
                            code: KeyCode::Char(' '),
                            modifiers: KeyModifiers::NONE,
                            ..
                        }
                        | KeyEvent {
                            code: KeyCode::Enter,
                            ..
                        } => {
                            let new_offset = (scroll_offset + display_height).min(max_scroll);
                            if new_offset != scroll_offset {
                                scroll_offset = new_offset;
                                needs_redraw = true;
                            }
                        }

                        // Page up
                        KeyEvent {
                            code: KeyCode::PageUp,
                            ..
                        } => {
                            let new_offset = scroll_offset.saturating_sub(display_height);
                            if new_offset != scroll_offset {
                                scroll_offset = new_offset;
                                needs_redraw = true;
                            }
                        }

                        // Home (go to top)
                        KeyEvent {
                            code: KeyCode::Home,
                            ..
                        }
                        | KeyEvent {
                            code: KeyCode::Char('g'),
                            modifiers: KeyModifiers::NONE,
                            ..
                        } => {
                            if scroll_offset != 0 {
                                scroll_offset = 0;
                                needs_redraw = true;
                            }
                        }

                        // End (go to bottom)
                        KeyEvent {
                            code: KeyCode::End, ..
                        }
                        | KeyEvent {
                            code: KeyCode::Char('G'),
                            modifiers: KeyModifiers::SHIFT,
                            ..
                        } => {
                            if scroll_offset != max_scroll {
                                scroll_offset = max_scroll;
                                needs_redraw = true;
                            }
                        }

                        _ => {}
                    }
                }
        }
    }

    // Cleanup: restore terminal
    execute!(io::stdout(), cursor::Show, LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;

    Ok(())
}

/// Execute commands in interactive mode
fn execute_command(command: &str, content: &str, _file_name: &str) -> anyhow::Result<()> {
    let command = command.trim();

    if command == "copy" {
        // Copy content to clipboard
        copy_to_clipboard_impl(content)?;
        return Ok(());
    } else if let Some(filename) = command.strip_prefix("write ") {
        // Write content to file
        let filename = filename.trim();
        if filename.is_empty() {
            return Err(anyhow::anyhow!("Filename cannot be empty"));
        }

        use std::fs;
        fs::write(filename, content)
            .map_err(|e| anyhow::anyhow!("Failed to write file '{}': {}", filename, e))?;
        return Ok(());
    } else {
        return Err(anyhow::anyhow!(
            "Unknown command: '{}'. Available commands: copy, write <filename>",
            command
        ));
    }
}

/// Copy content to clipboard using arboard
fn copy_to_clipboard_impl(content: &str) -> anyhow::Result<()> {
    use arboard::Clipboard;

    let mut clipboard =
        Clipboard::new().map_err(|e| anyhow::anyhow!("Failed to initialize clipboard: {}", e))?;

    clipboard
        .set_text(content)
        .map_err(|e| anyhow::anyhow!("Failed to write to clipboard: {}", e))?;

    Ok(())
}

/// Write rule content to specified file path
fn write_rule_to_file(path: &Path) -> anyhow::Result<()> {
    use std::fs;

    let rule_content = include_str!("../../../cramp_rule.md");
    
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent directories for '{:?}'", path))?;
    }
    
    // Write the rule content to the specified file
    fs::write(path, rule_content)
        .with_context(|| format!("Failed to write rule content to '{:?}'", path))?;
    
    info!("Rule content successfully written to: {:?}", path);
    Ok(())
}
