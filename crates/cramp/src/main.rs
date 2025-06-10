use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
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
use serde::{Deserialize, Serialize};
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
const DEFAULT_CONTROL_PORT: u16 = 8081; // Separate port for stdio control
const DEFAULT_SSE_PATH: &str = "/sse";
const DEFAULT_MESSAGE_PATH: &str = "/message";
const DEFAULT_STREAM_HTTP_PATH: &str = "/mcp";
const CONTROL_API_PATH: &str = "/control";

// --- CLI Structure ---

#[derive(Parser, Debug)]
#[command(name = "cramp")]
#[command(version, about = "CRAMP: CLI Runner and Controller for MCP Toolbox Service", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(short, long, value_enum, default_value_t = LogLevel::Info, global = true)]
    log_level: LogLevel,
    #[arg(long, default_value_t = false, global = true)]
    allow_env_log: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum Command {
    /// Run the MCP Toolbox server with stdio transport
    Stdio {
        #[command(flatten)]
        serve_args: ServeArgsFlat,
    },
    /// Run the MCP Toolbox server with SSE transport
    Sse {
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value = DEFAULT_SSE_PATH)]
        sse_path: String,
        #[arg(long, default_value = DEFAULT_MESSAGE_PATH)]
        message_path: String,
        #[command(flatten)]
        serve_args: ServeArgsFlat,
    },
    /// Run the MCP Toolbox server with StreamHttp transport
    StreamHttp {
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value = DEFAULT_STREAM_HTTP_PATH)]
        path: String,
        #[command(flatten)]
        serve_args: ServeArgsFlat,
    },
    /// List projects
    List {
        #[command(flatten)]
        control_args: ControlArgsFlat,
    },
    /// Add a new project from path (and activate it)
    Add {
        path: PathBuf,
        #[command(flatten)]
        control_args: ControlArgsFlat,
    },
    /// Remove a project by name
    Remove {
        name: String,
        #[command(flatten)]
        control_args: ControlArgsFlat,
    },
    /// Set the active project by name
    SetActive {
        name: String,
        #[command(flatten)]
        control_args: ControlArgsFlat,
    },
    /// Check server status
    Status {
        #[command(flatten)]
        control_args: ControlArgsFlat,
    },
    /// View documentation (README or cramp_rule)
    Doc {
        #[arg(value_enum)]
        doc_type: DocType,
    },
    /// Write rule content to specified file path
    Rule { path: PathBuf },
}

#[derive(Args, Debug, Clone)]
struct ServeArgs {
    #[command(subcommand)]
    transport: TransportCommand,
    /// Enable the control API
    #[arg(long, default_value_t = true, global = true)]
    enable_control_api: bool,
    /// Port for the control API when using Stdio transport
    #[arg(long, default_value_t = DEFAULT_CONTROL_PORT, global = true)]
    control_port: u16,

    #[arg(long, default_value_t = 210, global = true)]
    request_timeout: u64,
    #[arg(long, default_value_t = 130, global = true)]
    shutdown_timeout: u64,
    #[arg(long, default_value_t = 125, global = true)]
    initial_wait: u64,
    #[arg(long, default_value_t = 300, global = true)]
    sse_keep_alive: u64,
    #[arg(long, default_value_t = true, global = true)]
    stateful_mode: bool,
}

#[derive(Args, Debug, Clone)]
struct ServeArgsFlat {
    /// Enable the control API
    #[arg(long, default_value_t = true, global = true)]
    enable_control_api: bool,
    /// Port for the control API when using Stdio transport
    #[arg(long, default_value_t = DEFAULT_CONTROL_PORT, global = true)]
    control_port: u16,

    #[arg(long, default_value_t = 210, global = true)]
    request_timeout: u64,
    #[arg(long, default_value_t = 130, global = true)]
    shutdown_timeout: u64,
    #[arg(long, default_value_t = 125, global = true)]
    initial_wait: u64,
    #[arg(long, default_value_t = 300, global = true)]
    sse_keep_alive: u64,
    #[arg(long, default_value_t = true, global = true)]
    stateful_mode: bool,
}

#[derive(Args, Debug, Clone)]
struct ControlArgs {
    #[command(subcommand)]
    action: ControlAction,
    /// Base URL for the server's control API (e.g., http://127.0.0.1:8080/control or http://127.0.0.1:8081/control for stdio)
    #[arg(long, default_value = "http://127.0.0.1:8080/control", global = true)]
    server_url: String,
    /// Disable proxy for HTTP requests
    #[arg(long, default_value_t = true, global = true)]
    no_proxy: bool,
}

#[derive(Args, Debug, Clone)]
struct ControlArgsFlat {
    /// Base URL for the server's control API (e.g., http://127.0.0.1:8080/control or http://127.0.0.1:8081/control for stdio)
    #[arg(long, default_value = "http://127.0.0.1:8080/control", global = true)]
    server_url: String,
    /// Disable proxy for HTTP requests
    #[arg(long, default_value_t = true, global = true)]
    no_proxy: bool,
}

#[derive(Subcommand, Debug, Clone)]
enum ControlAction {
    /// List projects
    List,
    /// Add a new project from path (and activate it)
    Add { path: PathBuf },
    /// Remove a project by name
    Remove { name: String },
    /// Set the active project by name
    SetActive { name: String },
    /// Check server status
    Status,
}

#[derive(Subcommand, Debug, Clone)]
enum TransportCommand {
    Stdio,
    Sse {
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value = DEFAULT_SSE_PATH)]
        sse_path: String,
        #[arg(long, default_value = DEFAULT_MESSAGE_PATH)]
        message_path: String,
    },
    StreamHttp {
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        #[arg(long, default_value = DEFAULT_STREAM_HTTP_PATH)]
        path: String,
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
        Command::Stdio { serve_args } => {
            let args = ServeArgs {
                transport: TransportCommand::Stdio,
                enable_control_api: serve_args.enable_control_api,
                control_port: serve_args.control_port,
                request_timeout: serve_args.request_timeout,
                shutdown_timeout: serve_args.shutdown_timeout,
                initial_wait: serve_args.initial_wait,
                sse_keep_alive: serve_args.sse_keep_alive,
                stateful_mode: serve_args.stateful_mode,
            };
            run_server(args).await
        }
        Command::Sse {
            host,
            port,
            sse_path,
            message_path,
            serve_args,
        } => {
            let args = ServeArgs {
                transport: TransportCommand::Sse {
                    host,
                    port,
                    sse_path,
                    message_path,
                },
                enable_control_api: serve_args.enable_control_api,
                control_port: serve_args.control_port,
                request_timeout: serve_args.request_timeout,
                shutdown_timeout: serve_args.shutdown_timeout,
                initial_wait: serve_args.initial_wait,
                sse_keep_alive: serve_args.sse_keep_alive,
                stateful_mode: serve_args.stateful_mode,
            };
            run_server(args).await
        }
        Command::StreamHttp {
            host,
            port,
            path,
            serve_args,
        } => {
            let args = ServeArgs {
                transport: TransportCommand::StreamHttp { host, port, path },
                enable_control_api: serve_args.enable_control_api,
                control_port: serve_args.control_port,
                request_timeout: serve_args.request_timeout,
                shutdown_timeout: serve_args.shutdown_timeout,
                initial_wait: serve_args.initial_wait,
                sse_keep_alive: serve_args.sse_keep_alive,
                stateful_mode: serve_args.stateful_mode,
            };
            run_server(args).await
        }
        Command::List { control_args } => {
            let args = ControlArgs {
                action: ControlAction::List,
                server_url: control_args.server_url,
                no_proxy: control_args.no_proxy,
            };
            run_control_client(args).await
        }
        Command::Add { path, control_args } => {
            let args = ControlArgs {
                action: ControlAction::Add { path },
                server_url: control_args.server_url,
                no_proxy: control_args.no_proxy,
            };
            run_control_client(args).await
        }
        Command::Remove { name, control_args } => {
            let args = ControlArgs {
                action: ControlAction::Remove { name },
                server_url: control_args.server_url,
                no_proxy: control_args.no_proxy,
            };
            run_control_client(args).await
        }
        Command::SetActive { name, control_args } => {
            let args = ControlArgs {
                action: ControlAction::SetActive { name },
                server_url: control_args.server_url,
                no_proxy: control_args.no_proxy,
            };
            run_control_client(args).await
        }
        Command::Status { control_args } => {
            let args = ControlArgs {
                action: ControlAction::Status,
                server_url: control_args.server_url,
                no_proxy: control_args.no_proxy,
            };
            run_control_client(args).await
        }
        Command::Doc { doc_type } => show_documentation(doc_type),
        Command::Rule { path } => write_rule_to_file(&path),
    }
}

// --- Server Runner ---
async fn run_server(args: ServeArgs) -> Result<()> {
    info!("Starting CRAMP MCP Toolbox Service...");
    let service = Arc::new(McpToolboxService::with_timeouts(
        args.request_timeout,
        args.shutdown_timeout,
        args.initial_wait,
    ));
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

    let control_router = if args.enable_control_api {
        Some(control_api_router(service.clone()))
    } else {
        info!("Control API is disabled.");
        None
    };

    let server_result = match args.transport {
        TransportCommand::Stdio => {
            // Stdio needs a separate HTTP server for control API
            run_stdio(
                service.clone(),
                shutdown_token.clone(),
                control_router,
                args.control_port,
            )
            .await
        }
        TransportCommand::Sse {
            host,
            port,
            sse_path,
            message_path,
        } => {
            // Merge control API into the main HTTP server
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
                        args.sse_keep_alive,
                    )
                },
                control_router,
                "SSE",
            )
            .await
        }
        TransportCommand::StreamHttp { host, port, path } => {
            // Merge control API into the main HTTP server
            run_http_server(
                service.clone(),
                shutdown_token.clone(),
                &host,
                port,
                |s| create_stream_http_router(s, &path, args.sse_keep_alive, args.stateful_mode),
                control_router,
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
async fn run_stdio(
    service: Arc<McpToolboxService>,
    token: CancellationToken,
    control_router: Option<Router>,
    control_port: u16,
) -> Result<()> {
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

    match control_router {
        Some(router) => {
            let addr = SocketAddr::from_str(&format!("{}:{}", DEFAULT_HOST, control_port))?;
            info!(
                "Control API listening on: http://{}{} ",
                addr, CONTROL_API_PATH
            );
            let listener = tokio::net::TcpListener::bind(addr).await?;
            let control_app = Router::new().nest(CONTROL_API_PATH, router);
            let control_future = axum::serve(listener, control_app)
                .with_graceful_shutdown(token.clone().cancelled_owned());

            // Run both concurrently
            tokio::select! {
                res = stdio_future => res.context("Stdio error"),
                res = control_future => res.context("Control API server error"),
                 _ = token.cancelled() => { info!("Stdio/Control shutdown requested."); Ok(())}
            }
        }
        None => {
            // Only run stdio
            tokio::select! {
                 res = stdio_future => res.context("Stdio error"),
                  _ = token.cancelled() => { info!("Stdio shutdown requested."); Ok(())}
            }
        }
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
    control_router: Option<Router>,
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

    let mut app = router_factory(service);

    if let Some(ctrl_router) = control_router {
        info!(
            "Control API available at: http://{}{}",
            addr, CONTROL_API_PATH
        );
        app = app.nest(CONTROL_API_PATH, ctrl_router);
    }

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

// --- Control API Handlers & Router ---

#[derive(Serialize, Deserialize)]
struct AddProjectRequest {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct StatusResponse {
    status: String,
    message: Option<String>,
}

type SharedState = State<Arc<McpToolboxService>>;
type ApiResult = std::result::Result<Json<StatusResponse>, (StatusCode, String)>;

async fn list_projects_handler(
    State(service): SharedState,
) -> std::result::Result<String, (StatusCode, String)> {
    service
        .toolbox
        .list_projects()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn add_project_handler(
    State(service): SharedState,
    Json(req): Json<AddProjectRequest>,
) -> ApiResult {
    let name = req
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    info!(
        "Control API: Adding project '{}' from '{}'",
        name,
        req.path.display()
    );
    service
        .toolbox
        .add_server(name.clone(), req.path.clone())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to add: {}", e),
            )
        })?;
    // Auto-activate
    service
        .toolbox
        .set_active_project(&name)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Added but failed to activate: {}", e),
            )
        })?;

    Ok(Json(StatusResponse {
        status: "ok".into(),
        message: Some(format!("Added and activated project '{}'", name)),
    }))
}

async fn remove_project_handler(
    State(service): SharedState,
    AxumPath(name): AxumPath<String>,
) -> ApiResult {
    info!("Control API: Removing project '{}'", name);
    let msg = service
        .toolbox
        .remove_server(&name)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(Json(StatusResponse {
        status: "ok".into(),
        message: Some(msg),
    }))
}

async fn set_active_handler(
    State(service): SharedState,
    AxumPath(name): AxumPath<String>,
) -> ApiResult {
    info!("Control API: Activating project '{}'", name);
    let msg = service
        .toolbox
        .set_active_project(&name)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(Json(StatusResponse {
        status: "ok".into(),
        message: Some(msg),
    }))
}

async fn status_handler() -> impl IntoResponse {
    (StatusCode::OK, "Server is running")
}

fn control_api_router(service: Arc<McpToolboxService>) -> Router {
    Router::new()
        .route(
            "/projects",
            get(list_projects_handler).post(add_project_handler),
        )
        .route("/projects/{name}", delete(remove_project_handler))
        .route("/projects/{name}/activate", post(set_active_handler))
        .route("/status", get(status_handler))
        .with_state(service)
}

// --- Control Client ---
#[instrument(skip_all)]
async fn run_control_client(args: ControlArgs) -> Result<()> {
    let mut client_builder = reqwest::Client::builder();
    if args.no_proxy {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder.build()?;
    let base_url = args.server_url.trim_end_matches('/');
    info!("Connecting to control API at: {}", base_url);

    let response = match args.action {
        ControlAction::Status => client.get(format!("{}/status", base_url)).send().await?,
        ControlAction::List => client.get(format!("{}/projects", base_url)).send().await?,
        ControlAction::Add { path } => {
            let absolute_path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            };
            let payload = AddProjectRequest {
                path: absolute_path,
            };
            client
                .post(format!("{}/projects", base_url))
                .json(&payload)
                .send()
                .await?
        }
        ControlAction::Remove { name } => {
            client
                .delete(format!("{}/projects/{}", base_url, name))
                .send()
                .await?
        }
        ControlAction::SetActive { name } => {
            client
                .post(format!("{}/projects/{}/activate", base_url, name))
                .send()
                .await?
        }
    };

    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<no body>".to_string());

    if status.is_success() {
        println!("SUCCESS ({}):", status);
        // Try parsing JSON message if it looks like it
        if body.starts_with('{') {
            if let Ok(json) = serde_json::from_str::<StatusResponse>(&body) {
                println!("{}", json.message.unwrap_or(json.status));
            } else {
                println!("{}", body); // Print raw body if not our StatusResponse
            }
        } else {
            println!("{}", body); // Print raw body (e.g. from list)
        }
    } else {
        eprintln!("ERROR ({}):", status);
        eprintln!("{}", body);
        std::process::exit(1);
    }
    Ok(())
}

// +----------------------------------------------------------------++
// | --- Doc / Rule Functions (Kept from original) ------------------ |
// +-----------------------------------------------------------------+
fn show_documentation(doc_type: DocType) -> Result<()> {
    info!("Displaying documentation: {:?}", doc_type);
    let (content, display_name, file_name) = match doc_type {
        DocType::Readme => (include_str!("../../../README.md"), "README", "README.md"),
        DocType::Rule => (
            include_str!("../../../cramp_rule.md"),
            "CRAMP Rules",
            "cramp_rule.md",
        ),
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
fn write_rule_to_file(path: &Path) -> Result<()> {
    use std::fs;
    let rule_content = include_str!("../../../cramp_rule.md");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Failed to create dir {:?}", parent))?;
    }
    fs::write(path, rule_content).with_context(|| format!("Failed to write to {:?}", path))?;
    info!("Rule content written to: {:?}", path);
    Ok(())
}
