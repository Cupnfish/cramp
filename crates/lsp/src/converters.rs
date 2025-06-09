use crate::error::{RaError, Result};
use crate::models::*;
use itertools::Itertools;
use log::warn;
use lsp_types::{
    self as lsp, DiagnosticSeverity, DocumentSymbol, SymbolInformation, SymbolKind, WorkspaceEdit,
};
use path_clean::PathClean;
use similar::{ChangeTag, TextDiff};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use url::Url;

// --- Position / Range / Location (0-based <-> 0-based LSP) ---

pub fn position_to_lsp(pos: &Position) -> lsp::Position {
    lsp::Position {
        line: pos.line,
        character: pos.column,
    }
}
pub fn lsp_position_to_my(pos: &lsp::Position) -> Position {
    Position {
        line: pos.line,
        column: pos.character,
    }
}
pub fn range_to_lsp(range: &Range) -> lsp::Range {
    lsp::Range {
        start: position_to_lsp(&range.start),
        end: position_to_lsp(&range.end),
    }
}
pub fn lsp_range_to_my(range: &lsp::Range) -> Range {
    Range {
        start: lsp_position_to_my(&range.start),
        end: lsp_position_to_my(&range.end),
    }
}
// path_to_uri correctly uses url::Url
pub fn path_to_uri(path: &Path) -> Result<Url> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .clean();
    // Ensure canonical path for consistency
    let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
    Url::from_file_path(&canonical).map_err(|_| RaError::PathToUri(canonical))
}

// Convert url::Url to lsp_types::Uri
pub fn url_to_lsp_uri(url: &Url) -> lsp::Uri {
    lsp::Uri::from_str(url.as_str()).expect("Valid URL should convert to LSP Uri")
}

// Convert url::Url to lsp_types::Uri (consuming version)
pub fn url_to_lsp_uri_owned(url: Url) -> lsp::Uri {
    lsp::Uri::from_str(url.as_str()).expect("Valid URL should convert to LSP Uri")
}

// Convert lsp_types::Uri to url::Url
pub fn lsp_uri_to_url(uri: &lsp::Uri) -> Result<Url> {
    Url::parse(uri.as_str()).map_err(RaError::UriParseError)
}

pub fn uri_to_path(uri: &lsp::Uri) -> Result<PathBuf> {
    let as_url: url::Url = url::Url::parse(uri.as_str())?;
    as_url
        .to_file_path()
        .map_err(|_| RaError::UriToPath(as_url))
}

pub fn lsp_location_to_my(location: &lsp::Location) -> Result<Location> {
    Ok(Location {
        path: uri_to_path(&location.uri)?,
        range: lsp_range_to_my(&location.range),
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

/// Recursively flatten DocumentSymbol hierarchy
pub fn flatten_document_symbols(
    symbols: &[DocumentSymbol],
    path: PathBuf, // Absolute path
    results: &mut Vec<FlatSymbol>,
    get_relative_path_fn: &dyn Fn(&Path) -> String,
) {
    for symbol in symbols {
        let start_pos = lsp_position_to_my(&symbol.selection_range.start);
        results.push(FlatSymbol {
            name: symbol.name.clone(),
            kind: symbol_kind_to_string(&lsp_symbol_kind_to_my(symbol.kind)),
            file_path: get_relative_path_fn(&path),
            line: start_pos.line,
            column: start_pos.column,
        });
        if let Some(children) = &symbol.children {
            flatten_document_symbols(children, path.clone(), results, get_relative_path_fn);
        }
    }
}

/// Convert workspace symbols (SymbolInformation) to FlatSymbol
pub fn workspace_symbols_to_flat(
    symbols: &[SymbolInformation],
    get_relative_path_fn: &dyn Fn(&Path) -> String,
) -> Result<Vec<FlatSymbol>> {
    symbols
        .iter()
        .map(|symbol| {
            let path = uri_to_path(&symbol.location.uri)?;
            let start_pos = lsp_position_to_my(&symbol.location.range.start);
            Ok(FlatSymbol {
                name: symbol.name.clone(),
                kind: symbol_kind_to_string(&lsp_symbol_kind_to_my(symbol.kind)),
                file_path: get_relative_path_fn(&path),
                line: start_pos.line,
                column: start_pos.column,
            })
        })
        .collect()
}

// *** USABILITY: Add new helper function ***
/// Helper to find the first symbol matching the name in the DocumentSymbol hierarchy
pub fn find_symbol_by_name_in_hierarchy<'a>(
    symbols: &'a [DocumentSymbol],
    name: &str,
) -> Option<&'a DocumentSymbol> {
    // Check children first for more specific symbols (e.g., method within a struct)
    for symbol in symbols {
        if let Some(children) = &symbol.children {
            if let Some(child) = find_symbol_by_name_in_hierarchy(children, name) {
                // Prioritize certain kinds if the name matches exactly? For now, first match.
                if child.name == name {
                    return Some(child);
                }
            }
        }
    }
    // If not found in any children, check the top-level symbols in this slice
    symbols.iter().find(|symbol| symbol.name == name)
}
// ******************************************

// Helper to flatten DocumentSymbol hierarchy
pub fn find_symbol_in_hierarchy<'a>(
    symbols: &'a [DocumentSymbol],
    range: &lsp::Range,
) -> Option<&'a DocumentSymbol> {
    for symbol in symbols {
        // Check if the target range is within this symbol's range
        if symbol.range.start <= range.start && symbol.range.end >= range.end {
            if let Some(children) = &symbol.children {
                if let Some(child) = find_symbol_in_hierarchy(children, range) {
                    return Some(child); // Return more specific child
                }
            }
            return Some(symbol); // Return this if no child is more specific
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

// --- Cargo Metadata (1-based to 0-based MyDiagnostic) ---
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
            // Cargo is 1-based, convert to 0-based
            let start = Position {
                line: span.line_start.saturating_sub(1) as u32,
                column: span.column_start.saturating_sub(1) as u32,
            };
            let end = Position {
                line: span.line_end.saturating_sub(1) as u32,
                column: span.column_end.saturating_sub(1) as u32,
            };
            if start > end {
                warn!("Invalid range from cargo: {:?} at {:?}", span, path);
                return None;
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

// --- WorkspaceEdit -> Diff / FileEdit ---

// Convert LSP WorkspaceEdit to our FileEdit collection
pub fn workspace_edit_to_file_edits(edit: &WorkspaceEdit) -> Result<Vec<FileEdit>> {
    let mut file_edits_map = std::collections::HashMap::new();
    // Note: Only handling `changes`, not `document_changes` (create/rename/delete)
    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            let path = uri_to_path(uri)?;
            file_edits_map
                .entry(path)
                .or_insert_with(Vec::new)
                .extend(edits.clone());
        }
    }
    // Sort edits within each file descending!
    Ok(file_edits_map
        .into_iter()
        .map(|(path, mut edits)| {
            edits.sort_by(|a, b| b.range.start.cmp(&a.range.start));
            FileEdit { path, edits }
        })
        .collect())
}

// Apply edits (MUST be sorted descending!) and generate diff
pub fn generate_diff_preview(original_content: &str, edits: &[lsp::TextEdit]) -> String {
    let modified = match apply_edits_to_string(original_content, edits) {
        Ok(content) => content,
        Err(e) => return format!("[Error generating diff: {}]", e),
    };
    // Generate line-based diff
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

// Helper to apply edits (assuming edits are sorted descending)
pub fn apply_edits_to_string(
    original: &str,
    edits: &[lsp::TextEdit],
) -> std::result::Result<String, &'static str> {
    let mut content = original.to_string();
    let line_indices: Vec<usize> = std::iter::once(0)
        .chain(original.match_indices('\n').map(|(i, _)| i + 1))
        .collect();

    let line_col_to_offset = |line: u32, col: u32| -> Option<usize> {
        line_indices.get(line as usize).map(|&start_offset| {
            let line_end: usize = line_indices
                .get(line as usize + 1)
                .copied()
                .unwrap_or(original.len());
            let line_len = line_end.saturating_sub(start_offset);
            // Naive byte offset, LSP character should be UTF-16 code units
            start_offset + std::cmp::min(col as usize, line_len)
        })
    };

    for edit in edits {
        // edits must be sorted descending!
        let start_offset = line_col_to_offset(edit.range.start.line, edit.range.start.character)
            .ok_or("Invalid start offset")?;
        let end_offset = line_col_to_offset(edit.range.end.line, edit.range.end.character)
            .ok_or("Invalid end offset")?;
        if start_offset > end_offset || end_offset > content.len() {
            log::warn!(
                "Invalid edit range: {}..{} for content len {}, edit: {:?}",
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

// --- API Info to Markdown Summary ---
pub fn api_info_to_summary(api: &MyApiInfo) -> String {
    match api {
        MyApiInfo::Struct(s) => {
            let fields = s
                .fields
                .iter()
                .map(|f| format!("  - `{}`: {}", f.name, f.type_str.as_deref().unwrap_or("?")))
                .join("\n");
            let methods = s
                .methods
                .iter()
                .map(|m| format!("  - `fn {}`", m.name))
                .join("\n");
            format!(
                "```rust\n// Struct: {}\nstruct {} {{...}}\n```\n**Docs:** {}\n**Fields:**\n{}\n**Methods:**\n{}",
                s.name,
                s.name,
                s.docs.as_deref().unwrap_or("N/A"),
                fields,
                methods
            )
        }
        MyApiInfo::Trait(t) => {
            let methods = t
                .methods
                .iter()
                .map(|m| format!("  - `fn {}`", m.name))
                .join("\n");
            format!(
                "```rust\n// Trait: {}\ntrait {} {{...}}\n```\n**Docs:** {}\n**Methods:**\n{}",
                t.name,
                t.name,
                t.docs.as_deref().unwrap_or("N/A"),
                methods
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
                    loc.range.start.line + 1,   // Display as 1-based
                    loc.range.start.column + 1  // Display as 1-based
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
