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
use log::{debug, info, warn};
use lsp_types::WorkspaceSymbolResponse;
use lsp_types::{
    self as lsp, CodeActionKind, CodeActionParams, CodeActionTriggerKind, Diagnostic,
    DocumentSymbol, DocumentSymbolParams, GotoDefinitionParams, HoverParams, InitializeParams,
    InitializedParams, Location as LspLocation, PublishDiagnosticsParams, SymbolKind,
    TextDocumentClientCapabilities, TextDocumentIdentifier, TextDocumentPositionParams,
    WindowClientCapabilities, WorkspaceClientCapabilities, WorkspaceFolder, WorkspaceSymbolParams,
    notification::*, request::WorkspaceSymbolRequest, request::*,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::{Child, Command};
use tokio::sync::{Notify, RwLock, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite};
use url::Url;

type Responder = oneshot::Sender<Result<Value>>;
type PendingRequests = Arc<DashMap<u64, Responder>>;
type DiagnosticsMap = Arc<DashMap<Url, Vec<Diagnostic>>>;

#[derive(Debug)]
pub struct RaServer {
    project_root: PathBuf,
    project_name: String,
    child: Arc<RwLock<Option<Child>>>,
    writer_tx: mpsc::Sender<String>,
    // LSP-published diagnostics, not cargo check ones
    lsp_diagnostics: DiagnosticsMap,
    pending_requests: PendingRequests,
    request_id: AtomicU64,
    join_handles: Arc<RwLock<Vec<JoinHandle<()>>>>,
    is_initialized: Arc<Notify>,
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
        let root_path = project_root
            .canonicalize()
            .context("Failed to canonicalize project root")?;
        let root_uri = path_to_uri(&root_path)?;

        let mut child = Command::new("rust-analyzer")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .current_dir(&root_path)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| RaError::Process(format!("Failed to spawn rust-analyzer: {}", e)))?;

        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let stderr = child.stderr.take().expect("child stderr");

        let (writer_tx, writer_rx) = mpsc::channel::<String>(100);
        let lsp_diagnostics: DiagnosticsMap = Arc::new(DashMap::new());
        let pending_requests: PendingRequests = Arc::new(DashMap::new());
        let is_initialized = Arc::new(Notify::new());

        let mut handles = vec![];
        handles.push(tokio::spawn(stderr_loop(stderr, project_name.clone())));
        handles.push(tokio::spawn(writer_loop(stdin, writer_rx)));
        handles.push(tokio::spawn(reader_loop(
            stdout,
            lsp_diagnostics.clone(),
            pending_requests.clone(),
            writer_tx.clone(),
            is_initialized.clone(),
            project_name.clone(),
        )));

        let server = Arc::new(Self {
            project_root: root_path.clone(),
            project_name: project_name.clone(),
            child: Arc::new(RwLock::new(Some(child))),
            writer_tx: writer_tx.clone(),
            lsp_diagnostics,
            pending_requests,
            request_id: AtomicU64::new(1),
            join_handles: Arc::new(RwLock::new(handles)),
            is_initialized: is_initialized.clone(),
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
                                ],
                            },
                        }),
                        resolve_support: None,
                        ..Default::default()
                    }),
                    definition: Some(Default::default()),
                    hover: Some(Default::default()),
                    document_symbol: Some(lsp::DocumentSymbolClientCapabilities {
                        hierarchical_document_symbol_support: Some(true),
                        ..Default::default()
                    }),
                    publish_diagnostics: Some(Default::default()),
                    ..Default::default()
                }),
                window: Some(WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
            initialization_options: Some(
                json!({ "check": { "command": "check", }, "cargo": { "allFeatures": true, } }),
            ),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: url_to_lsp_uri_owned(root_uri),
                name: project_name.clone(),
            }]),
            work_done_progress_params: Default::default(),
            trace: None,
            client_info: None,
            locale: None,
        };

        debug!("[{}] Sending initialize request...", project_name);
        let init_id = server.request_id.fetch_add(1, Ordering::SeqCst);
        let (init_tx, init_rx) = oneshot::channel();
        server.pending_requests.insert(init_id, init_tx);
        server.send_raw_message(json!({"jsonrpc": "2.0", "id": init_id, "method": Initialize::METHOD, "params": init_params}).to_string()).await?;

        // Wait specifically for the initialize response
        match tokio::time::timeout(request_timeout, init_rx).await {
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
            "[{}] Rust-analyzer initialization handshake complete. Waiting {:?} for initial indexing...",
            project_name,
            initial_wait // Log the wait time
        );
        // *** PERFORMANCE: Allow more time for initial indexing ***
        // Show indexing progress during the wait
        let progress_interval = std::cmp::min(initial_wait.as_secs() / 10, 10); // Update every 10% or max 10 seconds
        let progress_interval = std::cmp::max(progress_interval, 1); // At least 1 second
        let total_steps = initial_wait.as_secs() / progress_interval;
        
        for step in 1..=total_steps {
            tokio::time::sleep(Duration::from_secs(progress_interval)).await;
            let progress_percent = (step * 100) / total_steps;
            info!("[{}] Indexing progress: {}% ({}/{})", project_name, progress_percent, step, total_steps);
        }
        
        // Sleep for any remaining time
        let remaining_time = initial_wait.as_secs() % progress_interval;
        if remaining_time > 0 {
            tokio::time::sleep(Duration::from_secs(remaining_time)).await;
        }
        
        info!("[{}] Indexing complete, server ready.", project_name);
        // **********************************************************

        Ok(server)
    }

    pub async fn shutdown(&self) -> Result<()> {
        info!("[{}] Shutting down rust-analyzer...", self.project_name);
        *self.is_shutting_down.write().await = true;

        if self.child.read().await.is_none() {
            debug!("[{}] Server already shut down.", self.project_name);
            return Ok(());
        }

        // Try graceful shutdown
        let graceful = async {
            // Check again before sending requests during shutdown
            if self.child.read().await.is_some() {
                self.send_request_internal::<Shutdown>(()).await?;
                self.send_notification::<Exit>(()).await?;
            }
            Ok::<(), RaError>(())
        };
        // Use the configured shutdown timeout
        if let Err(e) = tokio::time::timeout(self.shutdown_timeout, graceful).await {
            warn!(
                "[{}] Graceful shutdown timeout/error: {:?}",
                self.project_name, e
            );
        }

        let mut child_opt = self.child.write().await;
        if let Some(mut child) = child_opt.take() {
            debug!("[{}] Killing process.", self.project_name);
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        let handles = std::mem::take(&mut *self.join_handles.write().await);
        for handle in handles {
            handle.abort();
        }
        self.pending_requests.clear();
        self.lsp_diagnostics.clear();
        info!("[{}] Shutdown complete.", self.project_name);
        Ok(())
    }

    async fn send_request_internal<R: Request>(&self, params: R::Params) -> Result<R::Result>
    where
        R::Params: Serialize,
        R::Result: DeserializeOwned,
    {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);
        if *self.is_shutting_down.read().await {
            return Err(RaError::ServerNotRunning);
        }
        if R::METHOD != Shutdown::METHOD {
            self.is_initialized.notified().await;
        }

        let request = json!({ "jsonrpc": "2.0", "id": id, "method": R::METHOD, "params": params });
        let (tx, rx) = oneshot::channel();
        self.pending_requests.insert(id, tx);
        self.send_raw_message(request.to_string()).await?;

        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(serde_json::from_value(value)?),
            Ok(Ok(Err(e))) => Err(e), // Error from reader loop (e.g., LSP error response)
            Ok(Err(e)) => Err(RaError::OneshotRecv(e)),
            Err(_) => {
                self.pending_requests.remove(&id);
                Err(RaError::Timeout(self.request_timeout, id))
            }
        }
    }

    async fn send_notification<N: Notification>(&self, params: N::Params) -> Result<()>
    where
        N::Params: Serialize,
    {
        let notification = json!({ "jsonrpc": "2.0", "method": N::METHOD, "params": params });
        self.send_raw_message(notification.to_string()).await
    }

    async fn send_raw_message(&self, msg: String) -> Result<()> {
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
        let uri = path_to_uri(path)?;
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
        let response: Option<WorkspaceSymbolResponse> = self
            .send_request_internal::<WorkspaceSymbolRequest>(params)
            .await?;
        // WorkspaceSymbolResponse can be null or an array
        Ok(response)
    }

    pub async fn get_code_actions(
        &self,
        path: &Path,
        range: crate::models::Range,
    ) -> Result<Vec<MyCodeAction>> {
        let uri = path_to_uri(path)?;
        let lsp_range = range_to_lsp(&range);
        // Provide LSP diagnostics in context for better actions
        let context_diagnostics = self
            .lsp_diagnostics
            .get(&uri)
            .map(|entry| {
                entry
                    .value()
                    .iter()
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
                only: Some(vec![CodeActionKind::QUICKFIX, CodeActionKind::REFACTOR]),
                trigger_kind: Some(CodeActionTriggerKind::INVOKED),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let response = self
            .send_request_internal::<CodeActionRequest>(params)
            .await?;
        Ok(response
            .unwrap_or_default()
            .into_iter()
            .filter_map(|action| {
                match action {
                    lsp::CodeActionOrCommand::CodeAction(ca) => Some(MyCodeAction {
                        title: ca.title.clone(),
                        edit: ca.edit.clone(),
                        original_action: Some(ca),
                    }),
                    lsp::CodeActionOrCommand::Command(_) => None, // Ignore commands
                }
            })
            .collect())
    }

    pub async fn goto_definition(
        &self,
        path: &Path,
        pos: crate::models::Position,
    ) -> Result<Vec<crate::models::Location>> {
        let uri = path_to_uri(path)?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&uri)),
                position: position_to_lsp(&pos),
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
                .map(|link| LspLocation::new(link.target_uri, link.target_range))
                .collect(),
        };
        locations
            .into_iter()
            .map(|loc| lsp_location_to_my(&loc))
            .collect()
    }

    /// Gets detailed structure, docs, and definition location for symbol at position
    pub async fn get_api_structure(
        &self,
        path: &Path,
        pos: crate::models::Position,
    ) -> Result<Option<MyApiInfo>> {
        let definitions = self.goto_definition(path, pos).await?;
        let definition = match definitions.first() {
            Some(def) => def,
            None => return Ok(None), // Symbol not found
        };
        let def_uri = path_to_uri(&definition.path)?;
        let def_lsp_range = range_to_lsp(&definition.range);
        let def_lsp_pos = def_lsp_range.start;

        let doc_symbols_params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&def_uri)),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let doc_symbols_resp = self
            .send_request_internal::<DocumentSymbolRequest>(doc_symbols_params)
            .await?;
        let symbols = match doc_symbols_resp {
            Some(lsp::DocumentSymbolResponse::Nested(symbols)) => symbols,
            _ => {
                warn!(
                    "[{}] No nested symbols found for definition {:?}",
                    self.name(),
                    definition
                );
                return Ok(None);
            } // No symbols or flat list
        };
        let target_symbol = match find_symbol_in_hierarchy(&symbols, &def_lsp_range) {
            Some(sym) => sym,
            None => {
                warn!(
                    "[{}] Could not find exact symbol in hierarchy for definition {:?}",
                    self.name(),
                    definition
                );
                // Fallback: return basic info if we can't find the exact symbol structure
                let hover_params = HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&def_uri)),
                        position: def_lsp_pos,
                    },
                    work_done_progress_params: Default::default(),
                };
                let hover_resp = self
                    .send_request_internal::<HoverRequest>(hover_params)
                    .await?;
                let docs = hover_resp.as_ref().and_then(extract_hover_docs);
                return Ok(Some(MyApiInfo::Other {
                    name: format!("[Unknown Symbol at {:?}]", def_lsp_pos),
                    kind: MySymbolKind::Other,
                    docs,
                    location: Some(definition.clone()),
                }));
            }
        };

        let hover_params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier::new(url_to_lsp_uri(&def_uri)),
                position: def_lsp_pos,
            },
            work_done_progress_params: Default::default(),
        };
        let hover_resp = self
            .send_request_internal::<HoverRequest>(hover_params)
            .await?;
        let docs = hover_resp.as_ref().and_then(extract_hover_docs);
        // Basic info for non-struct/trait types
        let other_info = MyApiInfo::Other {
            name: target_symbol.name.clone(),
            kind: lsp_symbol_kind_to_my(target_symbol.kind),
            docs: docs.clone(),
            location: Some(definition.clone()),
        };

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
                        if child.kind == SymbolKind::FIELD {
                            info.fields.push(MyField {
                                name: child.name.clone(),
                                type_str: child.detail.clone(),
                                is_public: true,
                            })
                        } else if matches!(child.kind, SymbolKind::METHOD | SymbolKind::FUNCTION) {
                            info.methods.push(MyMethod {
                                name: child.name.clone(),
                                signature_str: child.detail.clone(),
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
                                docs: None,
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
            // Return the basic 'Other' info for functions, enums, consts, etc.
            _ => Ok(Some(other_info)),
        }
    }

    // --- VFS Notifications ---
    pub async fn notify_file_content(&self, path: &Path, content: &str) -> Result<()> {
        use lsp_types::{
            DidChangeTextDocumentParams, DidOpenTextDocumentParams, TextDocumentContentChangeEvent,
            TextDocumentItem, VersionedTextDocumentIdentifier,
            notification::{DidChangeTextDocument, DidOpenTextDocument},
        };
        let uri = path_to_uri(path)?;
        self.send_notification::<DidOpenTextDocument>(DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: url_to_lsp_uri(&uri),
                language_id: "rust".to_string(),
                version: 0,
                text: content.to_string(),
            },
        })
        .await?;
        // Send full content change for simplicity
        self.send_notification::<DidChangeTextDocument>(DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: url_to_lsp_uri_owned(uri),
                version: 1, // version must increment
            },
            content_changes: vec![TextDocumentContentChangeEvent {
                range: None,
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
        debug!("[{}] Dropping RaServer.", self.project_name);
        let child_lock = self.child.clone();
        let handles = self.join_handles.clone();
        let name = self.project_name.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut child_opt = child_lock.write().await;
                if let Some(mut child) = child_opt.take() {
                    warn!(
                        "[{}] Killing child process in Drop (shutdown not called)",
                        name
                    );
                    let _ = child.kill().await;
                }
                for handle in handles.write().await.drain(..) {
                    handle.abort();
                }
            });
        } else {
            log::error!(
                "[{}] Cannot cleanly kill RA process in Drop: no tokio runtime",
                name
            );
        }
    }
}

async fn writer_loop<W: AsyncWrite + Unpin>(writer: W, mut rx: mpsc::Receiver<String>) {
    let mut framed = FramedWrite::new(writer, LspCodec::default());
    while let Some(message) = rx.recv().await {
        if framed.send(message).await.is_err() {
            log::error!("Failed to write to rust-analyzer stdin, exiting writer loop.");
            break;
        }
    }
    debug!("Writer loop finished.");
}

async fn reader_loop<R: AsyncRead + Unpin>(
    reader: R,
    lsp_diagnostics: DiagnosticsMap,
    pending: PendingRequests,
    writer_tx: mpsc::Sender<String>,
    is_initialized: Arc<Notify>,
    project_name: String,
) {
    let mut framed = FramedRead::new(reader, LspCodec::default());
    let mut initialized_flag = false;

    while let Some(Ok(json_str)) = framed.next().await {
        match serde_json::from_str::<Value>(&json_str) {
            Ok(value) => {
                process_message(
                    value,
                    &lsp_diagnostics,
                    &pending,
                    &writer_tx,
                    &is_initialized,
                    &mut initialized_flag,
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
        }
    }
    debug!("[{}] Reader loop finished (stream ended).", project_name);
    // Clean up pending requests if stream closes unexpectedly
    let keys: Vec<u64> = pending.iter().map(|entry| *entry.key()).collect();
    for key in keys {
        if let Some((_, sender)) = pending.remove(&key) {
            let _ = sender.send(Err(RaError::ServerNotRunning));
        }
    }
}

async fn process_message(
    value: Value,
    lsp_diagnostics: &DiagnosticsMap,
    pending: &PendingRequests,
    writer_tx: &mpsc::Sender<String>,
    is_initialized: &Arc<Notify>,
    initialized_flag: &mut bool,
    project_name: &str,
) {
    if let Some(id_val) = value.get("id") {
        let id_opt = id_val
            .as_u64()
            .or_else(|| id_val.as_str().and_then(|s| s.parse::<u64>().ok()));

        if value.get("method").is_some() {
            // Request from server
            debug!(
                "[{}] Received request from server: {}",
                project_name,
                value.get("method").unwrap_or(&json!("unknown"))
            );
            // Auto-respond to common server requests
            if value["method"] == "workspace/configuration"
                || value["method"] == "client/registerCapability"
            {
                if let Some(id) = id_val.as_i64() {
                    let response = json!({ "jsonrpc": "2.0", "id": id, "result": json!({}) }); // Empty config/ack
                    let _ = writer_tx.send(response.to_string()).await;
                }
            }
        } else if let Some(id) = id_opt {
            // Response to our request
            if !*initialized_flag {
                info!("[{}] Received initialize result.", project_name);
                *initialized_flag = true;
                is_initialized.notify_one();
            }
            if let Some((_, responder)) = pending.remove(&id) {
                if let Some(error_val) = value.get("error") {
                    let lsp_error: Result<ResponseError> =
                        serde_json::from_value(error_val.clone()).map_err(Into::into);
                    let _ = responder.send(Err(lsp_error
                        .map(RaError::LspRequestError)
                        .unwrap_or_else(|e| RaError::Lsp(format!("Invalid error format: {}", e)))));
                } else {
                    let _ = responder.send(Ok(value.get("result").cloned().unwrap_or(json!(null))));
                }
            }
        }
    } else if let Some(method) = value.get("method") {
        // Notification from server
        match method.as_str() {
            Some(PublishDiagnostics::METHOD) => {
                if let Ok(params) =
                    serde_json::from_value::<PublishDiagnosticsParams>(value["params"].clone())
                {
                    if let Ok(url) = lsp_uri_to_url(&params.uri) {
                        lsp_diagnostics.insert(url, params.diagnostics);
                    }
                }
            }
            Some("window/logMessage") | Some("window/showMessage") => {
                if let Ok(params) =
                    serde_json::from_value::<lsp::LogMessageParams>(value["params"].clone())
                {
                    info!("[RA:{}] {}", project_name, params.message);
                }
            }
            _ => {
                log::trace!("[{}] Unhandled notification: {}", project_name, method);
            }
        }
    }
}

async fn stderr_loop<R: AsyncRead + Unpin + Send>(stderr: R, project_name: String) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let mut reader = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = reader.next_line().await {
        log::warn!("[RA-stderr:{}] {}", project_name, line);
    }
    debug!("[{}] Stderr loop finished.", project_name);
}
