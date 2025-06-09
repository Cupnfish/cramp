use crate::ToolResult;
use crate::converters::apply_edits_to_string;
use crate::error::ToolboxError;
use ignore::{DirEntry, WalkBuilder};
use path_clean::PathClean;
use pathdiff::diff_paths;
use std::fs;
use std::path::Path;

// Helper filter
fn is_relevant(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| !s.starts_with('.') || s == ".") // show root, hide other dotfiles/dirs
        .unwrap_or(false)
}

pub fn get_file_tree_string(
    root: &Path,
    max_items: Option<usize>,
    max_depth_limit: Option<usize>,
) -> ToolResult<String> {
    let root_canonical = root.canonicalize()?.clean();
    let mut tree_lines = Vec::new();
    let mut items_count = 0; // Initialized here
    let mut truncated_items = 0; // Initialized here
    // let mut depth_truncated_dirs = 0; // This variable is not used with the synchronous walker

    // The parallel walker code was removed due to complexity and errors.
    // Switched to a synchronous walker below.
    // tree_lines.clear(); // Not needed, initialized above
    // items_count = 0; // Not needed, initialized above
    // truncated_items = 0; // Not needed, initialized above
    // depth_truncated_dirs = 0; // This variable is not used with the synchronous walker
    let mut dirs_at_depth_limit = std::collections::HashSet::new();

    let walker_sync = WalkBuilder::new(&root_canonical)
        .hidden(false)
        .git_ignore(true)
        .parents(false)
        .sort_by_file_path(|a, b| a.cmp(b))
        .filter_entry(|e| is_relevant(e))
        .build(); // Synchronous walker

    for entry_result in walker_sync {
        if let Some(limit) = max_items {
            if items_count >= limit {
                truncated_items += 1;
                continue;
            }
        }

        let entry = match entry_result {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Error walking directory: {}", e);
                continue;
            }
        };

        let path = entry.path();
        let depth = entry.depth();

        if let Some(depth_limit) = max_depth_limit {
            if depth > depth_limit {
                if depth == depth_limit + 1 && entry.file_type().map_or(false, |ft| ft.is_dir()) {
                    if let Some(parent) = path.parent() {
                        if diff_paths(parent, &root_canonical).map_or(0, |p| p.components().count())
                            == depth_limit
                        {
                            dirs_at_depth_limit.insert(parent.to_path_buf());
                        }
                    }
                }
                continue;
            }
        }

        let is_dir = entry.file_type().map_or(false, |ft| ft.is_dir());
        let type_indicator = if is_dir { "/" } else { "" };

        let display_path = if path == root_canonical {
            ".".to_string()
        } else if let Some(relative) = diff_paths(path, &root_canonical) {
            relative.display().to_string()
        } else {
            continue;
        };

        if depth == 0 && display_path != "." {
            continue;
        }

        let indent = if depth > 0 {
            "  ".repeat(depth)
        } else {
            "".to_string()
        };
        if depth == 0 {
            tree_lines.push(format!("{}/", display_path));
        } else {
            tree_lines.push(format!(
                "{}{}{}",
                indent,
                entry.file_name().to_string_lossy(),
                type_indicator
            ));
        }
        items_count += 1;
    }

    if truncated_items > 0 {
        tree_lines.push(format!(
            "... and {} more entries (max_items limit reached)",
            truncated_items
        ));
    }
    if !dirs_at_depth_limit.is_empty() {
        tree_lines.push(format!(
            "... and content of {} directories truncated (max_depth limit reached)",
            dirs_at_depth_limit.len()
        ));
    }

    Ok(tree_lines.join("\n"))
}

// Edits must be sorted descending!
pub fn apply_lsp_edits_to_file(path: &Path, edits: &[lsp_types::TextEdit]) -> ToolResult<String> {
    let original_content = fs::read_to_string(path)?;
    let new_content = apply_edits_to_string(&original_content, edits)
        .map_err(|e| ToolboxError::ApplyEditFailed(format!("Logic error: {}", e)))?;
    fs::write(path, &new_content)?;
    Ok(new_content)
}
