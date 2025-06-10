use crate::error::{RaError, Result};
use crate::models::*;
use itertools::Itertools;
use log::warn;
use lsp_types::{
    self as lsp, DiagnosticSeverity, DocumentSymbol, SymbolInformation, SymbolKind, WorkspaceEdit,
};
use path_clean::PathClean;
use rayon::prelude::*;
use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use tokio::task::spawn_blocking;
use url::Url;

pub async fn async_path_to_uri(path: &Path) -> Result<Url> {
    let path = path.to_path_buf();
    spawn_blocking(move || {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(&path)
        }
        .clean();
        let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
        Url::from_file_path(&canonical).map_err(|_| RaError::PathToUri(canonical))
    })
    .await
    .map_err(RaError::TaskJoin)?
}

pub fn path_to_uri(path: &Path) -> Result<Url> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean();
    let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    Url::from_file_path(&canonical).map_err(|_| RaError::PathToUri(canonical))
}

pub fn url_to_lsp_uri(url: &Url) -> lsp::Uri {
    lsp::Uri::from_str(url.as_str()).expect("Valid URL should convert to LSP Uri")
}

pub fn url_to_lsp_uri_owned(url: Url) -> lsp::Uri {
    lsp::Uri::from_str(url.as_str()).expect("Valid URL should convert to LSP Uri")
}

pub fn lsp_uri_to_url(uri: &lsp::Uri) -> Result<Url> {
    Url::parse(uri.as_str()).map_err(RaError::UriParseError)
}

pub async fn async_uri_to_path(uri: &lsp::Uri) -> Result<PathBuf> {
    let uri_string = uri.as_str().to_string();
    spawn_blocking(move || {
        let as_url: url::Url = url::Url::parse(&uri_string)?;
        as_url
            .to_file_path()
            .map_err(|_| RaError::UriToPath(as_url))
    })
    .await
    .map_err(RaError::TaskJoin)?
}

pub fn uri_to_path(uri: &lsp::Uri) -> Result<PathBuf> {
    let as_url: url::Url = url::Url::parse(uri.as_str())?;
    as_url
        .to_file_path()
        .map_err(|_| RaError::UriToPath(as_url))
}

pub async fn async_lsp_location_to_my(location: &lsp::Location) -> Result<Location> {
    Ok(Location {
        path: async_uri_to_path(&location.uri).await?,
        range: location.range.into(),
    })
}

pub fn lsp_location_to_my(location: &lsp::Location) -> Result<Location> {
    Ok(Location {
        path: uri_to_path(&location.uri)?,
        range: location.range.into(),
    })
}

// --- Diagnostics ---
pub fn lsp_severity_to_my(severity: Option<DiagnosticSeverity>) -> MySeverity {
    match severity {
        Some(DiagnosticSeverity::ERROR) => MySeverity::Error,
        Some(DiagnosticSeverity::WARNING) => MySeverity::Warning,
        Some(DiagnosticSeverity::INFORMATION) => MySeverity::Information,
        Some(DiagnosticSeverity::HINT) => MySeverity::Hint,
        Some(_) => MySeverity::Unknown,
        None => MySeverity::Unknown,
    }
}

// --- Symbols ---
pub fn lsp_symbol_kind_to_my(kind: SymbolKind) -> MySymbolKind {
    match kind {
        SymbolKind::MODULE => MySymbolKind::Module,
        SymbolKind::STRUCT => MySymbolKind::Struct,
        SymbolKind::ENUM => MySymbolKind::Enum,
        SymbolKind::INTERFACE => MySymbolKind::Trait, // Traits
        SymbolKind::FUNCTION => MySymbolKind::Function,
        SymbolKind::METHOD => MySymbolKind::Method,
        SymbolKind::FIELD | SymbolKind::PROPERTY => MySymbolKind::Field,
        SymbolKind::CONSTANT => MySymbolKind::Const,
        SymbolKind::VARIABLE => MySymbolKind::Static, // Approximation
        SymbolKind::TYPE_PARAMETER => MySymbolKind::Type,
        SymbolKind::CLASS => MySymbolKind::Struct, // Approximation
        _ => MySymbolKind::Other,
    }
}
pub fn symbol_kind_to_string(kind: &MySymbolKind) -> String {
    format!("{:?}", kind).to_lowercase()
}

pub fn flatten_document_symbols(
    symbols: &[DocumentSymbol],
    path: PathBuf, // Absolute path
    results: &mut Vec<FlatSymbol>,
    get_relative_path_fn: &dyn Fn(&Path) -> String,
) {
    for symbol in symbols {
        let start_pos = symbol.selection_range.start;
        results.push(FlatSymbol {
            name: symbol.name.clone(),
            kind: symbol_kind_to_string(&lsp_symbol_kind_to_my(symbol.kind)),
            file_path: get_relative_path_fn(&path),
            line: start_pos.line,
            character: start_pos.character,
        });
        if let Some(children) = &symbol.children {
            flatten_document_symbols(children, path.clone(), results, get_relative_path_fn);
        }
    }
}

pub fn workspace_symbols_to_flat(
    symbols: &[SymbolInformation],
    get_relative_path_fn: &dyn Fn(&Path) -> String,
) -> Result<Vec<FlatSymbol>> {
    symbols
        .iter()
        .map(|symbol| {
            let path = uri_to_path(&symbol.location.uri)?;
            let start_pos = symbol.location.range.start;
            Ok(FlatSymbol {
                name: symbol.name.clone(),
                kind: symbol_kind_to_string(&lsp_symbol_kind_to_my(symbol.kind)),
                file_path: get_relative_path_fn(&path),
                line: start_pos.line,
                character: start_pos.character,
            })
        })
        .collect()
}

pub fn workspace_symbols_to_flat_optimized(
    symbols: &[SymbolInformation],
    root_path: &std::path::Path,
) -> Result<Vec<FlatSymbol>> {
    use pathdiff::diff_paths;

    let results: Result<Vec<_>> = symbols
        .par_iter()
        .map(|symbol| {
            let path = uri_to_path(&symbol.location.uri)?;
            let start_pos = symbol.location.range.start;

            let file_path = diff_paths(&path, root_path)
                .unwrap_or_else(|| path.to_path_buf())
                .display()
                .to_string();

            Ok(FlatSymbol {
                name: symbol.name.clone(),
                kind: symbol_kind_to_string(&lsp_symbol_kind_to_my(symbol.kind)),
                file_path,
                line: start_pos.line,
                character: start_pos.character,
            })
        })
        .collect();
    results
}

pub fn find_symbol_by_name_in_hierarchy<'a>(
    symbols: &'a [DocumentSymbol],
    name: &str,
) -> Option<&'a DocumentSymbol> {
    for symbol in symbols {
        if symbol.name == name {
            return Some(symbol);
        }
        if let Some(children) = &symbol.children {
            if let Some(child) = find_symbol_by_name_in_hierarchy(children, name) {
                return Some(child);
            }
        }
    }
    None
}

pub fn find_symbol_in_hierarchy<'a>(
    symbols: &'a [DocumentSymbol],
    range: &lsp::Range,
) -> Option<&'a DocumentSymbol> {
    for symbol in symbols {
        if symbol.range.start <= range.start && symbol.range.end >= range.end {
            if let Some(children) = &symbol.children {
                if let Some(child) = find_symbol_in_hierarchy(children, range) {
                    return Some(child);
                }
            }
            return Some(symbol);
        }
    }
    None
}

pub fn extract_hover_docs(hover: &lsp::Hover) -> Option<String> {
    match &hover.contents {
        lsp::HoverContents::Scalar(lsp::MarkedString::String(s)) => Some(s.clone()),
        lsp::HoverContents::Scalar(lsp::MarkedString::LanguageString(ls)) => Some(ls.value.clone()),
        lsp::HoverContents::Array(arr) => Some(
            arr.iter()
                .map(|ms| match ms {
                    lsp::MarkedString::String(s) => s.clone(),
                    lsp::MarkedString::LanguageString(ls) => ls.value.clone(),
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        lsp::HoverContents::Markup(markup) => Some(markup.value.clone()),
    }
}

pub fn cargo_diag_to_my(
    diag: &cargo_metadata::diagnostic::Diagnostic,
    root_path: &Path,
) -> Vec<MyDiagnostic> {
    let severity = match diag.level {
        cargo_metadata::diagnostic::DiagnosticLevel::Error
        | cargo_metadata::diagnostic::DiagnosticLevel::Ice => MySeverity::Error,
        cargo_metadata::diagnostic::DiagnosticLevel::Warning => MySeverity::Warning,
        cargo_metadata::diagnostic::DiagnosticLevel::Note => MySeverity::Information,
        cargo_metadata::diagnostic::DiagnosticLevel::Help => MySeverity::Hint,
        _ => MySeverity::Unknown,
    };
    let code = diag.code.as_ref().map(|c| c.code.clone());
    let message = diag.message.clone();

    diag.spans
        .iter()
        .filter_map(|span| {
            let path = root_path.join(&span.file_name).clean();
            let start = Position {
                line: span.line_start.saturating_sub(1) as u32,
                character: span.column_start.saturating_sub(1) as u32,
            };
            let end = Position {
                line: span.line_end.saturating_sub(1) as u32,
                character: span.column_end.saturating_sub(1) as u32,
            };
            if start > end {
                if !(start.line == end.line && start.character > end.character) {
                    warn!("Invalid range from cargo: {:?} at {:?}", span, path);
                    return None;
                }
            }
            Some(MyDiagnostic {
                location: Location {
                    path,
                    range: Range { start, end },
                },
                severity: severity.clone(),
                message: message.clone(),
                code: code.clone(),
            })
        })
        .collect()
}

pub fn workspace_edit_to_file_edits(edit: &WorkspaceEdit) -> Result<Vec<FileEdit>> {
    let mut file_edits_map = std::collections::HashMap::new();
    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            let path = uri_to_path(uri)?;
            file_edits_map
                .entry(path)
                .or_insert_with(Vec::new)
                .extend(edits.clone());
        }
    }
    if let Some(doc_changes) = &edit.document_changes {
        match doc_changes {
            lsp::DocumentChanges::Edits(edits_vec) => {
                for doc_edit in edits_vec {
                    let path = uri_to_path(&doc_edit.text_document.uri)?;
                    file_edits_map.entry(path).or_insert_with(Vec::new).extend(
                        doc_edit.edits.iter().map(|e| match e {
                            lsp::OneOf::Left(te) => te.clone(),
                            lsp::OneOf::Right(ate) => ate.text_edit.clone(),
                        }),
                    );
                }
            }
            lsp::DocumentChanges::Operations(_) => {
                warn!(
                    "WorkspaceEdit contained resource operations (create/rename/delete) which are ignored."
                );
            }
        }
    }

    Ok(file_edits_map
        .into_iter()
        .map(|(path, mut edits)| {
            edits.par_sort_by(|a, b| b.range.start.cmp(&a.range.start));
            FileEdit { path, edits }
        })
        .collect())
}

pub fn generate_diff_preview(original_content: &str, edits: &[lsp::TextEdit]) -> String {
    let modified = match apply_edits_to_string(original_content, edits) {
        Ok(content) => content,
        Err(e) => return format!("[Error generating diff: {}]", e),
    };
    let diff = TextDiff::from_lines(original_content, &modified);
    diff.iter_all_changes()
        .map(|change| {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            format!("{}{}", sign, change)
        })
        .collect::<String>()
}

pub fn apply_edits_to_string(
    original: &str,
    edits: &[lsp::TextEdit],
) -> std::result::Result<String, &'static str> {
    let mut content = original.to_string();
    let line_indices: Vec<usize> = std::iter::once(0)
        .chain(original.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    let line_col_to_offset = |line: u32, c: u32, text: &str| -> Option<usize> {
        line_indices.get(line as usize).map(|&start_offset| {
            let line_content = text.lines().nth(line as usize).unwrap_or("");
            let char_offset: usize = line_content
                .chars()
                .take(c as usize)
                .map(|c| c.len_utf8())
                .sum();
            start_offset + char_offset
        })
    };

    for edit in edits {
        let start_offset =
            line_col_to_offset(edit.range.start.line, edit.range.start.character, &content)
                .ok_or("Invalid start offset")?;
        let end_offset =
            line_col_to_offset(edit.range.end.line, edit.range.end.character, &content)
                .ok_or("Invalid end offset")?;

        if start_offset > end_offset || end_offset > content.len() || start_offset > content.len() {
            if start_offset == end_offset
                && start_offset == content.len()
                && !edit.new_text.is_empty()
            {
            } else {
                log::warn!(
                    "Invalid or out-of-bounds edit range: {}..{} for content len {}, edit: {:?}",
                    start_offset,
                    end_offset,
                    content.len(),
                    edit
                );
                continue;
            }
        }
        if !content.is_char_boundary(start_offset) || !content.is_char_boundary(end_offset) {
            warn!(
                "Edit range not on char boundary: {}..{} for content len {}, edit: {:?}",
                start_offset,
                end_offset,
                content.len(),
                edit
            );
            continue;
        }

        content.replace_range(start_offset..end_offset, &edit.new_text);
    }
    Ok(content)
}

pub fn api_info_to_summary(api: &MyApiInfo) -> String {
    match api {
        MyApiInfo::Struct(s) => {
            let fields = s
                .fields
                .iter()
                .map(|f| format!("  - `{}`: {}", f.name, f.type_str.as_deref().unwrap_or("?")))
                .join("\n");
            let fields_str = if fields.is_empty() {
                "  (none)".to_string()
            } else {
                fields
            };
            let methods = s
                .methods
                .iter()
                .map(|m| {
                    format!(
                        "  - `fn {}{}`",
                        m.name,
                        m.signature_str.as_deref().unwrap_or("")
                    )
                })
                .join("\n");
            let methods_str = if methods.is_empty() {
                "  (none)".to_string()
            } else {
                methods
            };
            format!(
                "```rust\n// Struct: {}\nstruct {} {{...}}\n```\n**Docs:** {}\n**Fields:**\n{}\n**Methods:**\n{}",
                s.name,
                s.name,
                s.docs.as_deref().unwrap_or("N/A"),
                fields_str,
                methods_str
            )
        }
        MyApiInfo::Trait(t) => {
            let methods = t
                .methods
                .iter()
                .map(|m| {
                    format!(
                        "  - `fn {}{}`",
                        m.name,
                        m.signature_str.as_deref().unwrap_or("")
                    )
                })
                .join("\n");
            let methods_str = if methods.is_empty() {
                "  (none)".to_string()
            } else {
                methods
            };
            format!(
                "```rust\n// Trait: {}\ntrait {} {{...}}\n```\n**Docs:** {}\n**Methods:**\n{}",
                t.name,
                t.name,
                t.docs.as_deref().unwrap_or("N/A"),
                methods_str
            )
        }
        MyApiInfo::Other {
            name,
            kind,
            docs,
            location,
        } => {
            let loc_str = location.as_ref().map_or("N/A".to_string(), |loc| {
                format!(
                    "{}:{}:{}",
                    loc.path.display(),
                    loc.range.start.line + 1,
                    loc.range.start.character + 1
                )
            });
            format!(
                "```rust\n// Symbol: {} ({})\n```\n**Docs:** {}\n**Location:** {}",
                name,
                symbol_kind_to_string(kind),
                docs.as_deref().unwrap_or("N/A"),
                loc_str
            )
        }
    }
}
