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
    // ignore target directory and hidden files/dirs except root
    if entry.file_type().map_or(false, |ft| ft.is_dir()) && entry.file_name() == "target" {
        return false;
    }
    entry
        .file_name()
        .to_str()
        // show root ".", hide other dotfiles/dirs like .git
        .map(|s| !s.starts_with('.') || s == ".")
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
    let mut dirs_at_depth_limit = std::collections::HashSet::new();

    let walker_sync = WalkBuilder::new(&root_canonical)
        .hidden(false) // Don't enter hidden dirs like .git, but is_relevant filters names
        .git_ignore(true)
        .parents(false)
        .max_depth(max_depth_limit.map(|d| d + 1)) // Set walker depth limit
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
                // Use log::warn instead of eprintln in library code
                log::warn!("Error walking directory {:?}: {}", root, e);
                continue;
            }
        };

        let path = entry.path();
        let depth = entry.depth();

        // Simplified depth check as max_depth is set on builder
        if let Some(depth_limit) = max_depth_limit {
            // Track directories at the limit to report truncation
            if depth == depth_limit && entry.file_type().map_or(false, |ft| ft.is_dir()) {
                dirs_at_depth_limit.insert(path.to_path_buf());
            }
            if depth > depth_limit {
                // This path might still be yielded if max_depth is not set on builder
                continue;
            }
        }

        let is_dir = entry.file_type().map_or(false, |ft| ft.is_dir());
        // Add "..." if directory content is truncated by depth limit
        let type_indicator = if is_dir {
            if dirs_at_depth_limit.contains(path) {
                "/..."
            } else {
                "/"
            }
        } else {
            ""
        };

        let display_path = if path == root_canonical {
            ".".to_string()
        } else if let Some(relative) = diff_paths(path, &root_canonical) {
            relative.display().to_string()
        } else {
            continue;
        };

        // Skip the root entry itself if it's not "."
        if depth == 0 && display_path != "." {
            continue;
        }
        // Only display the root as "." or "./"
        if depth == 0 {
            tree_lines.push("./".to_string()); // Always show root as ./
            items_count += 1;
            continue; // Root is handled
        }

        let indent = "  ".repeat(depth);

        tree_lines.push(format!(
            "{}{}{}",
            indent,
            entry.file_name().to_string_lossy(),
            type_indicator
        ));
        items_count += 1;
    }

    if truncated_items > 0 {
        tree_lines.push(format!(
            "... and {} more entries (max_items limit reached)",
            truncated_items
        ));
    }
    // Report depth truncation only if items were actually skipped because of it
    if max_depth_limit.is_some() && !dirs_at_depth_limit.is_empty() && truncated_items == 0 {
        tree_lines.push(format!(
            "... content of {} directories truncated (max_depth limit reached)",
            dirs_at_depth_limit.len()
        ));
    }

    Ok(tree_lines.join("\n"))
}

pub fn apply_lsp_edits_to_file(path: &Path, edits: &[lsp_types::TextEdit]) -> ToolResult<String> {
    let original_content = fs::read_to_string(path)?;
    let new_content = apply_edits_to_string(&original_content, edits)
        .map_err(|e| ToolboxError::ApplyEditFailed(format!("Logic error: {}", e)))?;
    fs::write(path, &new_content)?;
    Ok(new_content)
}
