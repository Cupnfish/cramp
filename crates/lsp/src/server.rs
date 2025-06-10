use crate::cargo::{run_external_cargo_check, run_external_cargo_test};
use crate::error::{RaError, ResponseError, Result};
use crate::lsp_codec::LspCodec;
use crate::models::{
    CargoTestOutput, MyApiInfo, MyCodeAction, MyDiagnostic, MyField, MyMethod, MyStructInfo,
    MyTraitInfo,
};
use crate::{MySymbolKind, converters::*};
use anyhow::Context;
use dashmap::DashMap;
use futures::{SinkExt, StreamExt};
use log::{debug, info, trace, warn};
use lsp_types::WorkspaceSymbolResponse;
use lsp_types::{
    self as lsp,
    CodeActionKind,
    CodeActionParams,
    CodeActionTriggerKind,
    Diagnostic,
    DocumentSymbol,
    DocumentSymbolParams,
    GotoDefinitionParams,
    HoverParams,
    InitializeParams,
    InitializedParams,
    Location as LspLocation,
    PublishDiagnosticsParams,
    SymbolKind,
    TextDocumentClientCapabilities,
    TextDocumentIdentifier,
    TextDocumentPositionParams,
    // Uri as LspUri, // Alias to avoid conflict - Removed unused import
    WindowClientCapabilities,
    WorkspaceClientCapabilities,
    WorkspaceFolder,
    WorkspaceSymbolParams,
    notification::*,
    request::WorkspaceSymbolRequest,
    request::*,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, RwLock, mpsc, oneshot};
use tokio::task::{JoinHandle, spawn_blocking};
use tokio_util::codec::{FramedRead, FramedWrite};
use url::Url;

type Responder = oneshot::Sender<Result<Value>>;
type PendingRequests = Arc<DashMap<u64, Responder>>;
type DiagnosticsMap = Arc<DashMap<Url, Vec<Diagnostic>>>;

type ProgressTokens = Arc<DashMap<lsp::ProgressToken, String>>;

#[derive(Debug)]
pub struct RaServer {
    project_root: PathBuf,
    project_name: String,
    child: Arc<RwLock<Option<Child>>>,
    writer_tx: mpsc::Sender<String>,
    // LSP-published diagnostics, not cargo check ones
    lsp_diagnostics: DiagnosticsMap,
    pending_requests: PendingRequests,
    progress_tokens: ProgressTokens,
    request_id: AtomicU64,
    join_handles: Arc<RwLock<Vec<JoinHandle<()>>>>,
    is_initialized: Arc<Notify>,
    initial_indexing_done: Arc<Notify>,
    is_shutting_down: Arc<RwLock<bool>>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
}

impl RaServer {
    #[allow(deprecated)] // Allow using root_uri
    pub async fn start_with_timeouts(
        project_root: &Path,
        project_name: String,
        request_timeout: Duration,
        shutdown_timeout: Duration,
        initial_wait: Duration,
    ) -> Result<Arc<Self>> {
        info!("[{}] Starting rust-analyzer...", project_name);
        let root_path_clone = project_root.to_path_buf();
        let root_path = spawn_blocking(move || {
            root_path_clone
                .canonicalize()
                .context("Failed to canonicalize project root")
        })
        .await
        .map_err(RaError::TaskJoin)??;

        let root_uri = async_path_to_uri(&root_path).await?;

        let mut child = Command::new("rust-analyzer")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(&root_path)
            .kill_on_drop(true)
            // .env("RA_LOG", "info,rust_analyzer::reload=debug")
            .spawn()
            .map_err(|e| RaError::Process(format!("Failed to spawn rust-analyzer: {}", e)))?;

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");

        let (writer_tx, writer_rx) = mpsc::channel::<String>(100);
        let lsp_diagnostics: DiagnosticsMap = Arc::new(DashMap::new());
        let pending_requests: PendingRequests = Arc::new(DashMap::new());
        let progress_tokens: ProgressTokens = Arc::new(DashMap::new());
        let is_initialized = Arc::new(Notify::new());
        let initial_indexing_done = Arc::new(Notify::new()); // <<< PERFORMANCE

        let mut handles = vec![];
        handles.push(tokio::spawn(stderr_loop(stderr, project_name.clone())));
        handles.push(tokio::spawn(writer_loop(stdin, writer_rx)));
        handles.push(tokio::spawn(reader_loop(
            stdout,
            lsp_diagnostics.clone(),
            pending_requests.clone(),
            progress_tokens.clone(),
            writer_tx.clone(),
            is_initialized.clone(),
            initial_indexing_done.clone(),
            project_name.clone(),
        )));

        let server = Arc::new(Self {
            project_root: root_path.clone(),
            project_name: project_name.clone(),
            child: Arc::new(RwLock::new(Some(child))),
            writer_tx: writer_tx.clone(),
            lsp_diagnostics,
            pending_requests,
            progress_tokens,
            request_id: AtomicU64::new(1),
            join_handles: Arc::new(RwLock::new(handles)),
            is_initialized: is_initialized.clone(),
            initial_indexing_done: initial_indexing_done.clone(),
            is_shutting_down: Arc::new(RwLock::new(false)),
            request_timeout,
            shutdown_timeout,
        });

        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(url_to_lsp_uri(&root_uri)),
            root_path: Some(root_path.to_string_lossy().into_owned()),
            capabilities: lsp::ClientCapabilities {
                workspace: Some(WorkspaceClientCapabilities {
                    apply_edit: Some(true),
                    // Enable workspace symbol search
                    symbol: Some(lsp::WorkspaceSymbolClientCapabilities {
                        dynamic_registration: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                text_document: Some(TextDocumentClientCapabilities {
                    code_action: Some(lsp::CodeActionClientCapabilities {
                        code_action_literal_support: Some(lsp::CodeActionLiteralSupport {
                            code_action_kind: lsp::CodeActionKindLiteralSupport {
                                value_set: vec![
                                    CodeActionKind::QUICKFIX.as_str().to_string(),
                                    CodeActionKind::REFACTOR.as_str().to_string(),
                                    CodeActionKind::REFACTOR_EXTRACT.as_str().to_string(),
                                    CodeActionKind::REFACTOR_INLINE.as_str().to_string(),
                                    CodeActionKind::SOURCE_ORGANIZE_IMPORTS.as_str().to_string(),
                                ],
                            },
                        }),
                        // Enable resolve support if needed for complex actions
                        resolve_support: Some(lsp::CodeActionCapabilityResolveSupport {
                            properties: vec!["edit".to_string(), "command".to_string()],
                        }),
                        is_preferred_support: Some(true),
                        data_support: Some(true),
                        ..Default::default()
                    }),
                    definition: Some(Default::default()),
                    hover: Some(lsp::HoverClientCapabilities {
                        content_format: Some(vec![
                            lsp::MarkupKind::Markdown,
                            lsp::MarkupKind::PlainText,
                        ]),
                        ..Default::default()
                    }),
                    document_symbol: Some(lsp::DocumentSymbolClientCapabilities {
                        hierarchical_document_symbol_support: Some(true),
                        // Define which symbols we are interested in
                        symbol_kind: Some(lsp::SymbolKindCapability {
                            value_set: Some(vec![
                                SymbolKind::MODULE,
                                SymbolKind::STRUCT,
                                SymbolKind::ENUM,
                                SymbolKind::INTERFACE,
                                SymbolKind::FUNCTION,
                                SymbolKind::METHOD,
                                SymbolKind::FIELD,
                                SymbolKind::CONSTANT,
                                SymbolKind::VARIABLE,
                                SymbolKind::TYPE_PARAMETER,
                                SymbolKind::CLASS,
                                SymbolKind::PROPERTY,
                                SymbolKind::ENUM_MEMBER,
                            ]),
                        }),
                        ..Default::default()
                    }),
                    publish_diagnostics: Some(Default::default()),
                    completion: Some(lsp::CompletionClientCapabilities {
                        completion_item: Some(lsp::CompletionItemCapability {
                            documentation_format: Some(vec![
                                lsp::MarkupKind::Markdown,
                                lsp::MarkupKind::PlainText,
                            ]),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                window: Some(WindowClientCapabilities {
                    work_done_progress: Some(true),
                    show_message: Some(Default::default()),
                    show_document: Some(Default::default()),
                    ..Default::default()
                }),
                // Ask for markdown support
                general: Some(lsp::GeneralClientCapabilities {
                    markdown: Some(lsp::MarkdownClientCapabilities {
                        parser: "cramp".into(),
                        version: Some("0.1".into()),
                        allowed_tags: None,
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            initialization_options: Some(json!({
                "check": { "command": "check", "features": "all" }, // check all features
                "cargo": { "allFeatures": true, "loadOutDirsFromCheck": true, "features": "all" },
                "procMacro": { "enable": true },
                 "rustfmt": {"extraArgs": ["+nightly"]}, // if needed
                 "lens": { "enable": false }, // Disable code lens for performance
                 "inlayHints": { "enable": false }, // Disable inlay hints for performance
            })),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: url_to_lsp_uri_owned(root_uri),
                name: project_name.clone(),
            }]),
            work_done_progress_params: Default::default(),
            trace: Some(lsp::TraceValue::Verbose), // Get more info
            client_info: Some(lsp::ClientInfo {
                name: "cramp-lsp".into(),
                version: Some("0.1.0".into()),
            }),
            locale: None,
        };

        debug!("[{}] Sending initialize request...", project_name);
        let init_id = server.request_id.fetch_add(1, Ordering::SeqCst);
        let (init_tx, init_rx) = oneshot::channel();
        server.pending_requests.insert(init_id, init_tx);
        // Check writer_tx is not closed before sending
        if server.writer_tx.is_closed() {
            return Err(RaError::ServerNotRunning);
        }
        server.send_raw_message(json!({"jsonrpc": "2.0", "id": init_id, "method": Initialize::METHOD, "params": init_params}).to_string()).await?;

        // Wait specifically for the initialize response
        // Use a longer timeout just for initialization
        match tokio::time::timeout(request_timeout.max(Duration::from_secs(60)), init_rx).await {
            Ok(Ok(Ok(_value))) => { /* reader loop will notify */ }
            Ok(Ok(Err(e))) => return Err(e),
            Ok(Err(e)) => return Err(RaError::OneshotRecv(e)),
            Err(_) => return Err(RaError::Timeout(request_timeout, init_id)),
        }
        // Wait for reader loop to process the response and notify
        server.is_initialized.notified().await;

        server
            .send_notification::<Initialized>(InitializedParams {})
            .await?;
        info!(
            "[{}] Rust-analyzer initialization handshake complete. Waiting up to {:?} for initial indexing...",
            project_name,
            initial_wait // Log the wait time
        );
        // *** PERFORMANCE: Allow more time for initial indexing ***
        // Wait for the initial_indexing_done notification, or timeout
        let wait_start = Instant::now();
        match tokio::time::timeout(initial_wait, server.initial_indexing_done.notified()).await {
            Ok(_) => info!(
                "[{}] Initial indexing reported complete in {:.2?}.",
                project_name,
                wait_start.elapsed()
            ),
            Err(_) => warn!(
                "[{}] Timeout ({:?}) waiting for initial indexing completion signal. Proceeding anyway.",
                project_name, initial_wait
            ),
        }
        // Clear tokens after initial wait
        server.progress_tokens.clear();

        info!("[{}] Server considered ready.", project_name);
        // **********************************************************

        Ok(server)
    }

    pub async fn wait_ready(&self) {
        self.is_initialized.notified().await;
        // Wait again in case notified() was called before the timeout in start() finished
        self.initial_indexing_done.notified().await;
    }

    pub async fn shutdown(&self) -> Result<()> {
        info!("[{}] Shutting down rust-analyzer...", self.project_name);
        // Prevent new requests
        *self.is_shutting_down.write().await = true;
        // Close the channel to stop the writer loop gracefully
        self.writer_tx.closed().await;

        if self.child.read().await.is_none() {
            debug!("[{}] Server process already gone.", self.project_name);
            // Still need to abort handles
        } else {
            // Try graceful shutdown
            let graceful = async {
                info!("[{}] Sending shutdown request.", self.project_name);
                // Use send_request_internal directly, skip wait_ready
                // match self.send_request_internal::<Shutdown>(()).await {
                // Check writer_tx is not closed before sending
                if !self.writer_tx.is_closed() {
                    match self.send_request_internal_no_wait::<Shutdown>(()).await {
                        Ok(_) => debug!("[{}] Shutdown request accepted.", self.project_name),
                        Err(e) => warn!("[{}] Shutdown request failed: {:?}", self.project_name, e),
                    }
                    info!("[{}] Sending exit notification.", self.project_name);
                    if !self.writer_tx.is_closed() {
                        self.send_notification::<Exit>(()).await?;
                    }
                } else {
                    warn!(
                        "[{}] Writer channel closed before shutdown/exit could be sent.",
                        self.project_name
                    );
                }
                // Give it a moment
                tokio::time::sleep(Duration::from_millis(500)).await;
                Ok::<(), RaError>(())
            };
            // Use the configured shutdown timeout
            if let Err(e) = tokio::time::timeout(self.shutdown_timeout, graceful).await {
                warn!(
                    "[{}] Graceful shutdown timeout/error: {:?}",
                    self.project_name, e
                );
            }
        }

        let mut child_opt = self.child.write().await;
        if let Some(mut child) = child_opt.take() {
            debug!("[{}] Killing process forcefully.", self.project_name);
            // Force kill if graceful failed or timed out
            let _ = child.kill().await;
            // Wait briefly for process to terminate
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .ok();
        }
        // Abort all background tasks
        let handles = std::mem::take(&mut *self.join_handles.write().await);
        info!(
            "[{}] Aborting {} background tasks.",
            self.project_name,
            handles.len()
        );
        // Await all aborted handles to ensure they finish cleanup
        futures_util::future::join_all(handles).await;

        self.pending_requests.clear();
        self.lsp_diagnostics.clear();
        self.progress_tokens.clear();
        info!("[{}] Shutdown complete.", self.project_name);
        Ok(())
    }

    async fn send_request_internal_no_wait<R: Request>(
        &self,
        params: R::Params,
    ) -> Result<R::Result>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        // Allow Shutdown even if shutting down flag is set
        if *self.is_shutting_down.read().await && R::METHOD != Shutdown::METHOD {
            return Err(RaError::ServerNotRunning);
        }
        // Don't wait for is_initialized or initial_indexing_done

        let request = json!({ "jsonrpc": "2.0", "id": id, "method": R::METHOD, "params": params });
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(id, tx);
        // Check writer_tx is not closed
        if self.writer_tx.is_closed() {
            self.pending_requests.remove(&id);
            return Err(RaError::ServerNotRunning);
        }
        self.send_raw_message(request.to_string()).await?;

        let timeout_duration = if R::METHOD == Shutdown::METHOD {
            self.shutdown_timeout // Use shutdown timeout for shutdown request
        } else {
            self.request_timeout
        };

        match tokio::time::timeout(timeout_duration, rx).await {
            Ok(Ok(Ok(value))) => Ok(serde_json::from_value(value)?),
            Ok(Ok(Err(e))) => Err(e), // Error from reader loop (e.g., LSP error response)
            Ok(Err(e)) => {
                self.pending_requests.remove(&id);
                Err(RaError::OneshotRecv(e))
            }
            Err(_) => {
                self.pending_requests.remove(&id);
                Err(RaError::Timeout(timeout_duration, id))
            }
        }
    }

    async fn send_request_internal<R: Request>(&self, params: R::Params) -> Result<R::Result>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        if R::METHOD != Shutdown::METHOD && R::METHOD != Initialize::METHOD {
            // Use a timeout for waiting, in case the notify is missed
            tokio::time::timeout(self.request_timeout, self.wait_ready())
                .await
                .map_err(|_| RaError::ServerNotInitialized)?;
        }
        self.send_request_internal_no_wait::<R>(params).await
    }

    async fn send_notification<N: Notification>(&self, params: N::Params) -> Result<()>
    where
        N::Params: Serialize,
    {
        if self.writer_tx.is_closed() {
            return Err(RaError::ServerNotRunning);
        }
        let notification = json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params });
        self.send_raw_message(notification.to_string()).await
    }

    async fn send_raw_message(&self, msg: String) -> Result<()> {
        if self.writer_tx.is_closed() {
            return Err(RaError::Channel("Writer channel closed".into()));
        }
        self.writer_tx
            .send(msg)
            .await
            .map_err(|e| RaError::Channel(e.to_string()))
    }
    // --- Getters ---
    pub fn root_path(&self) -> &Path {
        &self.project_root
    }
    pub fn name(&self) -> &str {
        &self.project_name
    }

    // --- API Implementation (0-based) ---
    pub fn get_cargo_check_diagnostics(&self) -> Result<Vec<MyDiagnostic>> {
        run_external_cargo_check(&self.project_root)
    }
    pub fn run_cargo_test(&self, test_name: Option<&str>) -> Result<CargoTestOutput> {
        run_external_cargo_test(&self.project_root, test_name)
    }

    // Get symbols for a specific file
    pub async fn list_document_symbols(&self, path: &Path) -> Result<Vec<DocumentSymbol>> {
        let uri = async_path_to_uri(path).await?;
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&uri)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let response = self
            .send_request_internal::<DocumentSymbolRequest>(params)
            .await?;
        // We request hierarchical symbols in capabilities
        Ok(match response {
            Some(lsp::DocumentSymbolResponse::Nested(symbols)) => symbols,
            Some(lsp::DocumentSymbolResponse::Flat(flat_symbols)) => {
                // Convert flat to nested if server doesn't support hierarchy (fallback)
                warn!(
                    "[{}] Server returned flat symbols, expected nested.",
                    self.name()
                );
                flat_symbols
                    .into_iter()
                    .map(|s| DocumentSymbol {
                        name: s.name,
                        detail: None,
                        kind: s.kind,
                        tags: s.tags,
                        #[allow(deprecated)]
                        deprecated: None,
                        range: s.location.range,
                        selection_range: s.location.range, // Use full range as selection
                        children: None,
                    })
                    .collect()
            }
            // Fallback or empty
            _ => vec![],
        })
    }

    // Search symbols across the workspace
    pub async fn search_workspace_symbols(
        &self,
        query: &str,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        let params = WorkspaceSymbolParams {
            query: query.to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        // RA might return null or an array
        let response: Option<WorkspaceSymbolResponse> = self
            .send_request_internal::<WorkspaceSymbolRequest>(params)
            .await?;
        Ok(response)
    }

    pub async fn get_code_actions(
        &self,
        path: &Path,
        range: crate::models::Range,
    ) -> Result<Vec<MyCodeAction>> {
        let uri = async_path_to_uri(path).await?;
        let lsp_range: lsp_types::Range = range.into();
        // Provide LSP diagnostics in context for better actions
        // Filter diagnostics that overlap with the requested range
        let context_diagnostics = self
            .lsp_diagnostics
            .get(&uri)
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    // Check for range overlap
                    .filter(|d| !(d.range.end < lsp_range.start || d.range.start > lsp_range.end))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        let params = CodeActionParams {
            text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&uri)),
            range: lsp_range,
            context: lsp::CodeActionContext {
                diagnostics: context_diagnostics,
                // Request specific kinds for performance
                only: Some(vec![
                    CodeActionKind::QUICKFIX,
                    CodeActionKind::REFACTOR,
                    CodeActionKind::SOURCE_ORGANIZE_IMPORTS,
                ]),
                trigger_kind: Some(CodeActionTriggerKind::INVOKED),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        info!(
            "[{}] Requesting code actions for {:?} range {:?}",
            self.name(),
            path.file_name().unwrap_or_default(),
            lsp_range
        );
        let response = self
            .send_request_internal::<CodeActionRequest>(params)
            .await?;

        let actions: Vec<_> = response
         .unwrap_or_default()
         .into_iter()
         .filter_map(|action| {
             match action {
                 lsp::CodeActionOrCommand::CodeAction(ca) => {
                      // Filter out actions without edits unless they are commands we might handle
                      if ca.edit.is_none() && ca.command.is_none() && ca.kind != Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS) {
                           debug!("[{}] Skipping code action without edit/command: {}", self.name(), ca.title);
                           return None;
                      }
                       // Handle organize imports specifically if no edit is provided directly
                      let edit_to_use = if ca.edit.is_none() && ca.kind == Some(CodeActionKind::SOURCE_ORGANIZE_IMPORTS) {
                            // This might require resolving the action or executing the command
                           warn!("[{}] Organize Imports action requires resolve or command execution, skipping: {}", self.name(), ca.title);
                            None // Or implement resolve logic
                      } else {
                           ca.edit.clone()
                      };

                     Some(MyCodeAction {
                         title: ca.title.clone(),
                         edit: edit_to_use, // Use the resolved or direct edit
                         original_action: Some(ca), // Keep original for potential resolve
                     })
                 },
                 lsp::CodeActionOrCommand::Command(cmd) => {
                      warn!("[{}] Ignoring command action: {}", self.name(), cmd.title);
                      None
                  }, // Ignore commands for now
             }
         })
         .collect();
        info!(
            "[{}] Received {} applicable code actions.",
            self.name(),
            actions.len()
        );
        Ok(actions)
    }

    // Optional: Resolve a code action if it requires more info (e.g., organize imports)
    pub async fn resolve_code_action(&self, action: lsp::CodeAction) -> Result<lsp::CodeAction> {
        self.send_request_internal::<CodeActionResolveRequest>(action)
            .await
    }

    pub async fn goto_definition(
        &self,
        path: &Path,
        pos: crate::models::Position,
    ) -> Result<Vec<crate::models::Location>> {
        let uri = async_path_to_uri(path).await?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&uri)),
                position: pos.into(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let response = self.send_request_internal::<GotoDefinition>(params).await?;
        let locations: Vec<LspLocation> = match response {
            None => vec![],
            Some(lsp::GotoDefinitionResponse::Scalar(loc)) => vec![loc],
            Some(lsp::GotoDefinitionResponse::Array(locs)) => locs,
            Some(lsp::GotoDefinitionResponse::Link(links)) => links
                .into_iter()
                // Filter out links without target_uri or range
                .filter_map(|link| Some(LspLocation::new(link.target_uri, link.target_range)))
                .collect(),
        };
        // For typical goto-definition (few results), direct await is fine.
        let mut my_locations = Vec::with_capacity(locations.len());
        for loc in locations {
            match async_lsp_location_to_my(&loc).await {
                Ok(my_loc) => my_locations.push(my_loc),
                Err(e) => warn!(
                    "[{}] Failed to convert location {:?}: {}",
                    self.name(),
                    loc,
                    e
                ),
            }
        }
        Ok(my_locations)
    }

    /// Gets detailed structure, docs, and definition location for symbol at position
    pub async fn get_api_structure(
        &self,
        path: &Path,
        pos: crate::models::Position,
    ) -> Result<Option<MyApiInfo>> {
        // Step 1: Find the definition location
        let definitions = self.goto_definition(path, pos).await?;
        let definition = match definitions.first() {
            Some(def) => def,
            // Symbol not found or definition cannot be determined
            None => {
                debug!(
                    "[{}] No definition found for {:?} at {:?}",
                    self.name(),
                    path,
                    pos
                );
                return Ok(None);
            }
        };
        let def_uri = async_path_to_uri(&definition.path).await?;
        let def_lsp_range: lsp_types::Range = definition.range.into();
        let def_lsp_pos = def_lsp_range.start;
        let def_doc_id = TextDocumentIdentifier::new(url_to_lsp_uri(&def_uri));

        let doc_symbols_params = DocumentSymbolParams {
            text_document: def_doc_id.clone(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let hover_params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: def_doc_id,
                position: def_lsp_pos,
            },
            work_done_progress_params: Default::default(),
        };

        // Use tokio::join! to run requests in parallel
        let (symbols_result, hover_result) = tokio::join!(
            self.send_request_internal::<DocumentSymbolRequest>(doc_symbols_params),
            self.send_request_internal::<HoverRequest>(hover_params)
        );
        // ***********************************************************

        let hover_resp = hover_result.ok().flatten(); // Get Option<Hover>
        let docs = hover_resp.as_ref().and_then(extract_hover_docs);

        // Process symbols result
        let symbols = match symbols_result {
            Ok(Some(lsp::DocumentSymbolResponse::Nested(symbols))) => symbols,
            Ok(_) => {
                warn!(
                    "[{}] No or flat symbols found for definition file {:?}",
                    self.name(),
                    definition.path
                );
                vec![] // Treat as no symbols found
            }
            Err(e) => {
                warn!(
                    "[{}] Error fetching symbols for definition {:?}: {}",
                    self.name(),
                    definition,
                    e
                );
                // Fallback to just hover info if symbols fail
                return Ok(Some(MyApiInfo::Other {
                    name: format!("[Symbol Error at {:?}]", def_lsp_pos),
                    kind: MySymbolKind::Other,
                    docs,
                    location: Some(definition.clone()),
                }));
            }
        };

        let symbols_clone = symbols.clone();
        let target_symbol_opt = spawn_blocking(move || {
            find_symbol_in_hierarchy(&symbols_clone, &def_lsp_range)
                // Clone the found symbol out of the blocking context if found
                .cloned()
        })
        .await
        .map_err(RaError::TaskJoin)?;

        let target_symbol = match target_symbol_opt {
            Some(sym) => sym,
            None => {
                warn!(
                    "[{}] Could not find exact symbol in hierarchy for definition range {:?} in {:?}",
                    self.name(),
                    def_lsp_range,
                    definition.path
                );
                // Fallback: return basic info from hover if symbol structure lookup fails
                return Ok(Some(MyApiInfo::Other {
                    // Try to get name from hover if possible, otherwise generic
                    name: hover_resp
                        .as_ref()
                        .and_then(|h| extract_hover_docs(h))
                        // Extract first word heuristic
                        .and_then(|s| {
                            s.split_whitespace()
                                .find(|&w| !w.is_empty())
                                .map(String::from)
                        })
                        .unwrap_or_else(|| format!("[Unknown Symbol at {:?}]", def_lsp_pos)),
                    kind: MySymbolKind::Other,
                    docs,
                    location: Some(definition.clone()),
                }));
            }
        };

        // Basic info for non-struct/trait types
        let other_info = MyApiInfo::Other {
            name: target_symbol.name.clone(),
            kind: lsp_symbol_kind_to_my(target_symbol.kind),
            docs: docs.clone(),
            location: Some(definition.clone()),
        };

        let definition = definition.clone();
        let info_result = spawn_blocking(move || {
            match target_symbol.kind {
                SymbolKind::STRUCT | SymbolKind::CLASS => {
                    let mut info = MyStructInfo {
                        name: target_symbol.name.clone(),
                        docs,
                        location: Some(definition.clone()),
                        ..Default::default()
                    };
                    if let Some(children) = &target_symbol.children {
                        for child in children {
                            if child.kind == SymbolKind::FIELD || child.kind == SymbolKind::PROPERTY
                            {
                                info.fields.push(MyField {
                                    name: child.name.clone(),
                                    type_str: child.detail.clone(),
                                    // Heuristic for public fields
                                    is_public: !child.name.starts_with('_'),
                                })
                            } else if matches!(
                                child.kind,
                                SymbolKind::METHOD | SymbolKind::FUNCTION
                            ) {
                                info.methods.push(MyMethod {
                                    name: child.name.clone(),
                                    signature_str: child.detail.clone(),
                                    // Docs for methods require separate hover requests, omit for perf
                                    docs: None,
                                    has_self: child
                                        .detail
                                        .as_ref()
                                        .map(|s| s.contains("self"))
                                        .unwrap_or(false),
                                })
                            }
                        }
                    }
                    Ok(Some(MyApiInfo::Struct(info)))
                }
                SymbolKind::INTERFACE => {
                    // Trait
                    let mut info = MyTraitInfo {
                        name: target_symbol.name.clone(),
                        docs,
                        location: Some(definition.clone()),
                        ..Default::default()
                    };
                    if let Some(children) = &target_symbol.children {
                        for child in children {
                            if matches!(child.kind, SymbolKind::METHOD | SymbolKind::FUNCTION) {
                                info.methods.push(MyMethod {
                                    name: child.name.clone(),
                                    signature_str: child.detail.clone(),
                                    docs: None, // Omit method docs for perf
                                    has_self: child
                                        .detail
                                        .as_ref()
                                        .map(|s| s.contains("self"))
                                        .unwrap_or(false),
                                })
                            } else if child.kind == SymbolKind::TYPE_PARAMETER {
                                info.associated_types.push(child.name.clone())
                            }
                        }
                    }
                    Ok(Some(MyApiInfo::Trait(info)))
                }
                SymbolKind::ENUM => {
                    // Handle Enums specifically if needed, or fall through to Other
                    Ok(Some(other_info))
                }
                // Return the basic 'Other' info for functions, enums, consts, etc.
                _ => Ok(Some(other_info)),
            }
        })
        .await
        .map_err(RaError::TaskJoin)?; // Handle JoinError

        info_result
    }

    // --- VFS Notifications ---
    // Notify RA about file changes on disk, so its VFS is updated
    pub async fn notify_file_change(&self, path: &Path) -> Result<()> {
        let content = tokio::fs::read_to_string(path).await?;
        self.notify_file_content(path, &content).await
    }

    pub async fn notify_file_content(&self, path: &Path, content: &str) -> Result<()> {
        use lsp_types::{
            DidChangeTextDocumentParams, DidOpenTextDocumentParams, TextDocumentContentChangeEvent,
            TextDocumentItem, VersionedTextDocumentIdentifier,
            notification::{DidChangeTextDocument, DidOpenTextDocument},
        };
        let uri = async_path_to_uri(path).await?;
        let lsp_uri = url_to_lsp_uri(&uri);

        // Always send DidOpen first to ensure RA knows about the file
        self.send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: lsp_uri.clone(),
                language_id: "rust".to_string(),
                // Use a dummy version, RA manages its own
                version: 0,
                text: content.to_string(),
            },
        })
        .await?;
        // Send full content change for simplicity
        // RA often prefers full content updates
        self.send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                // Use the same URI
                uri: lsp_uri,
                // version must increment, but we send full text, so RA syncs
                version: 1,
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None, // No range means full document replace
                range_length: None,
                text: content.to_string(),
            }],
        })
        .await?;
        debug!(
            "[{}] Notified RA of change to {:?}",
            self.project_name, path
        );
        Ok(())
    }
}

impl Drop for RaServer {
    fn drop(&mut self) {
        // Best effort cleanup in drop, but shutdown() should always be called explicitly
        debug!("[{}] Dropping RaServer.", self.project_name);
        // Ensure shutting down flag is set
        if let Ok(mut shutting_down) = self.is_shutting_down.try_write() {
            *shutting_down = true;
        }

        let child_lock = self.child.clone();
        let handles = self.join_handles.clone();
        let name = self.project_name.clone();
        // Try to spawn cleanup on the current runtime
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                warn!(
                    "[{}] RaServer dropped without explicit shutdown(). Force killing...",
                    name
                );
                let mut child_opt = child_lock.write().await;
                if let Some(mut child) = child_opt.take() {
                    let _ = child.kill().await;
                    tokio::time::timeout(Duration::from_secs(2), child.wait())
                        .await
                        .ok();
                }
                let mut handles_list = handles.write().await;
                for handle in handles_list.drain(..) {
                    handle.abort();
                }
                // No join_all here to avoid blocking drop further
            });
        } else {
            // If no runtime, can't do much async cleanup
            log::error!(
                "[{}] Cannot cleanly kill RA process in Drop: no tokio runtime",
                name
            );
        }
    }
}

async fn writer_loop<W: AsyncWrite + Unpin>(writer: W, mut rx: mpsc::Receiver<String>) {
    let mut framed = FramedWrite::new(writer, LspCodec::default());
    // Use recv() which waits, or close() to stop when channel closes
    while let Some(message) = rx.recv().await {
        if framed.send(message).await.is_err() {
            log::error!("Failed to write to rust-analyzer stdin, exiting writer loop.");
            break;
        }
    }
    // Ensure buffer is flushed before closing
    if framed.flush().await.is_err() {
        log::error!("Failed to flush writer on close.");
    }
    debug!("Writer loop finished.");
    // Dropping framed closes the writer
}

async fn reader_loop<R: AsyncRead + Unpin>(
    reader: R,
    lsp_diagnostics: DiagnosticsMap,
    pending: PendingRequests,
    progress_tokens: ProgressTokens,
    writer_tx: mpsc::Sender<String>,
    is_initialized: Arc<Notify>,
    initial_indexing_done: Arc<Notify>,
    project_name: String,
) {
    let mut framed = FramedRead::new(reader, LspCodec::default());
    let mut initialized_flag = false;
    let mut indexing_complete_notified = false;
    let start_time = Instant::now();

    while let Some(message_result) = framed.next().await {
        match message_result {
            Ok(json_str) => match serde_json::from_str::<Value>(&json_str) {
                Ok(value) => {
                    process_message(
                        value,
                        &lsp_diagnostics,
                        &pending,
                        &progress_tokens,
                        &writer_tx,
                        &is_initialized,
                        &initial_indexing_done,
                        &mut initialized_flag,
                        &mut indexing_complete_notified,
                        &start_time,
                        &project_name,
                    )
                    .await
                }
                Err(e) => log::error!(
                    "[{}] Failed to parse JSON: {} -> {}",
                    project_name,
                    json_str,
                    e
                ),
            },
            Err(e) => {
                log::error!(
                    "[{}] Error reading from stream: {}. Closing loop.",
                    project_name,
                    e
                );
                break; // Exit loop on read error
            }
        }
    }
    debug!("[{}] Reader loop finished (stream ended).", project_name);
    // Notify in case we are still waiting
    if !initialized_flag {
        is_initialized.notify_one();
    }
    if !indexing_complete_notified {
        initial_indexing_done.notify_one();
    }

    // Clean up pending requests if stream closes unexpectedly
    let keys: Vec<u64> = pending.iter().map(|entry| *entry.key()).collect();
    if !keys.is_empty() {
        warn!(
            "[{}] Cleaning up {} pending requests on stream close.",
            project_name,
            keys.len()
        );
    }
    for key in keys {
        if let Some((_, sender)) = pending.remove(&key) {
            // Inform waiting tasks that the server is gone
            let _ = sender.send(Err(RaError::ServerNotRunning));
        }
    }
    progress_tokens.clear();
}

async fn process_progress(
    params: &lsp::ProgressParams,
    progress_tokens: &ProgressTokens,
    initial_indexing_done: &Arc<Notify>,
    indexing_complete_notified: &mut bool,
    start_time: &Instant,
    project_name: &str,
) {
    match &params.value {
        lsp::ProgressParamsValue::WorkDone(work) => {
            match work {
                lsp::WorkDoneProgress::Begin(begin) => {
                    progress_tokens.insert(params.token.clone(), begin.title.clone());
                    // Check if this is the main indexing task
                    if begin.title.contains("indexing") || begin.title.contains("loading") {
                        info!(
                            "[RA:{}] Progress Begin: {} ({:.2?} elapsed)",
                            project_name,
                            begin.title,
                            start_time.elapsed()
                        );
                    } else {
                        debug!("[RA:{}] Progress Begin: {}", project_name, begin.title);
                    }
                }
                lsp::WorkDoneProgress::Report(report) => {
                    if let Some(entry) = progress_tokens.get(&params.token) {
                        let title = entry.value();
                        // Log indexing progress periodically
                        if (title.contains("indexing") || title.contains("loading"))
                            && report.percentage.is_some()
                        {
                            info!(
                                "[RA:{}] Progress: {} - {}% {}",
                                project_name,
                                title,
                                report.percentage.unwrap_or(0),
                                report.message.as_deref().unwrap_or("")
                            );
                        } else {
                            trace!(
                                "[RA:{}] Progress: {} - {}",
                                project_name,
                                title,
                                report.message.as_deref().unwrap_or("")
                            );
                        }
                    }
                }
                lsp::WorkDoneProgress::End(end) => {
                    if let Some((_, title)) = progress_tokens.remove(&params.token) {
                        info!(
                            "[RA:{}] Progress End: {} {}",
                            project_name,
                            title,
                            end.message.as_deref().unwrap_or("")
                        );
                        // Heuristic: if the main indexing/loading task ends, notify
                        if !*indexing_complete_notified
                            && (title.contains("indexing")
                                || title.contains("loading")
                                || title.contains("cargo check"))
                        {
                            info!(
                                "[RA:{}] Detected end of initial heavy work ('{}'). Notifying ready. ({:.2?} elapsed)",
                                project_name,
                                title,
                                start_time.elapsed()
                            );
                            initial_indexing_done.notify_one();
                            *indexing_complete_notified = true;
                        }
                    }
                }
            }
        }
    }
}

async fn process_message(
    value: Value,
    lsp_diagnostics: &DiagnosticsMap,
    pending: &PendingRequests,
    progress_tokens: &ProgressTokens,
    writer_tx: &mpsc::Sender<String>,
    is_initialized: &Arc<Notify>,
    initial_indexing_done: &Arc<Notify>,
    initialized_flag: &mut bool,
    indexing_complete_notified: &mut bool,
    start_time: &Instant,
    project_name: &str,
) {
    // Check if it's a response to one of our requests
    if let Some(id_val) = value.get("id") {
        // IDs can be numbers or strings
        let id_opt = id_val
            .as_u64()
            .or_else(|| id_val.as_str().and_then(|s| s.parse::<u64>().ok()))
            // Handle potential null id in error responses
            .or_else(|| if id_val.is_null() { Some(0) } else { None });

        // Check if it's a request FROM the server TO the client
        if value.get("method").is_some() {
            let method = value
                .get("method")
                .and_then(|method| method.as_str())
                .unwrap_or("unknown");
            debug!(
                "[{}] Received request from server: {}",
                project_name, method
            );

            // Auto-respond to common server requests
            if method == "workspace/configuration" {
                // RA asks for config, provide empty
                if let Some(id) = id_val.as_i64() {
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": [{}] }); // Array of configs
                    let _ = writer_tx.send(response.to_string()).await;
                }
            } else if method == "client/registerCapability"
                || method == "client/unregisterCapability"
            {
                // RA asks to register/unregister a capability, acknowledge
                if let Some(id) = id_val.as_i64() {
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": null });
                    let _ = writer_tx.send(response.to_string()).await;
                }
            } else if method == "window/workDoneProgress/create" {
                if let (Some(id), Ok(params)) = (
                    id_val.as_i64(),
                    serde_json::from_value::<lsp::WorkDoneProgressCreateParams>(
                        value["params"].clone(),
                    ),
                ) {
                    debug!(
                        "[RA:{}] Creating progress token: {:?}",
                        project_name, params.token
                    );
                    progress_tokens.insert(params.token, "Starting...".to_string());
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": null });
                    let _ = writer_tx.send(response.to_string()).await;
                }
            } else if method == "workspace/applyEdit" {
                // RA wants client to apply an edit (e.g., from a command)
                warn!(
                    "[RA:{}] Received workspace/applyEdit request - not yet implemented.",
                    project_name
                );
                if let Some(id) = id_val.as_i64() {
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": {"applied": false, "failureReason": "Client cannot apply edits"} });
                    let _ = writer_tx.send(response.to_string()).await;
                }
            } else {
                // Respond with error for unhandled methods
                if let Some(id) = id_val.as_i64() {
                    let response = json!({
                         "jsonrpc": "2.0",
                         "id": id,
                         "error": {"code": lsp_types::error_codes::REQUEST_FAILED, "message": format!("Method {} not found", method)}
                    });
                    let _ = writer_tx.send(response.to_string()).await;
                }
            }

        // It's a RESPONSE to our request
        } else if let Some(id) = id_opt {
            // Response to our request
            if !*initialized_flag {
                // The first response must be the initialize result
                info!("[{}] Received initialize result.", project_name);
                *initialized_flag = true;
                is_initialized.notify_one();
            }
            // Find the responder channel for this ID
            if let Some((_, responder)) = pending.remove(&id) {
                if let Some(error_val) = value.get("error") {
                    // Request failed
                    let lsp_error: Result<ResponseError> =
                        serde_json::from_value(error_val.clone()).map_err(Into::into);
                    // Send the error back to the caller
                    let _ = responder.send(Err(lsp_error
                        .map(RaError::LspRequestError)
                        .unwrap_or_else(|e| RaError::Lsp(format!("Invalid error format: {}", e)))));
                } else {
                    // Request succeeded, send the result
                    let _ = responder.send(Ok(value.get("result").cloned().unwrap_or(json!(null))));
                }
            } else if id != 0 {
                // Response received for an ID we don't know (maybe timed out?)
                debug!(
                    "[{}] Received response for unknown or timed-out ID: {}",
                    project_name, id
                );
            }
        }
        // It's a NOTIFICATION from the server
    } else if let Some(method) = value.get("method") {
        // Notification from server
        match method.as_str() {
            Some(PublishDiagnostics::METHOD) => {
                if let Ok(params) =
                    serde_json::from_value::<PublishDiagnosticsParams>(value["params"].clone())
                {
                    let uri_clone = params.uri.clone();
                    if let Ok(url) = spawn_blocking(move || lsp_uri_to_url(&uri_clone)).await {
                        if let Ok(url) = url {
                            // debug!("[RA:{}] Diagnostics for {}", project_name, url);
                            lsp_diagnostics.insert(url, params.diagnostics);
                        }
                    }
                }
            }
            Some("$/progress") => {
                if let Ok(params) =
                    serde_json::from_value::<lsp::ProgressParams>(value["params"].clone())
                {
                    process_progress(
                        &params,
                        progress_tokens,
                        initial_indexing_done,
                        indexing_complete_notified,
                        start_time,
                        project_name,
                    )
                    .await;
                }
            }
            Some("window/logMessage") | Some("window/showMessage") => {
                // Log messages from RA
                if let Ok(params) =
                    serde_json::from_value::<lsp::LogMessageParams>(value["params"].clone())
                {
                    // Adjust log level based on message type
                    match params.typ {
                        lsp::MessageType::ERROR => {
                            log::error!("[RA:{}] {}", project_name, params.message)
                        }
                        lsp::MessageType::WARNING => {
                            log::warn!("[RA:{}] {}", project_name, params.message)
                        }
                        lsp::MessageType::INFO => {
                            log::info!("[RA:{}] {}", project_name, params.message)
                        }
                        lsp::MessageType::LOG => {
                            log::debug!("[RA:{}] {}", project_name, params.message)
                        }
                        _ => log::info!("[RA:{}] {}", project_name, params.message),
                    }
                }
            }
            // RA custom notification for analysis status
            Some("rust-analyzer/status") => {
                if let Ok(params) = serde_json::from_value::<Value>(value["params"].clone()) {
                    trace!("[RA-Status:{}] {}", project_name, params);
                    // Heuristic: if status reports "ready" and we haven't notified yet
                    if !*indexing_complete_notified
                        && params["health"] == "ok"
                        && params.get("message").is_none()
                    {
                        info!(
                            "[RA:{}] Status is 'ok' with no message. Notifying ready. ({:.2?} elapsed)",
                            project_name,
                            start_time.elapsed()
                        );
                        initial_indexing_done.notify_one();
                        *indexing_complete_notified = true;
                    }
                }
            }
            _ => {
                // Ignore other notifications
                log::trace!("[{}] Unhandled notification: {}", project_name, method);
            }
        }
    } else {
        // Malformed message
        log::warn!("[{}] Received malformed message: {}", project_name, value);
    }
}

async fn stderr_loop<R: AsyncRead + Unpin + Send>(stderr: R, project_name: String) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut reader = BufReader::new(stderr).lines();
    // Use while let Ok(Some(line)) to handle errors and end-of-stream
    loop {
        match reader.next_line().await {
            Ok(Some(line)) => log::warn!("[RA-stderr:{}] {}", project_name, line),
            Ok(None) => break, // Stream ended
            Err(e) => {
                log::error!("[RA-stderr:{}] Error reading stderr: {}", project_name, e);
                break; // Stop on error
            }
        }
    }
    debug!("[{}] Stderr loop finished.", project_name);
}
