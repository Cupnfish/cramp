use crate::ToolResult;
use crate::converters::apply_edits_to_string;
use crate::error::ToolboxError;
use std::fs;
use std::path::Path;

pub fn apply_lsp_edits_to_file(path: &Path, edits: &[lsp_types::TextEdit]) -> ToolResult<String> {
    let original_content = fs::read_to_string(path)?;
    let new_content = apply_edits_to_string(&original_content, edits)
        .map_err(|e| ToolboxError::ApplyEditFailed(format!("Logic error: {}", e)))?;
    fs::write(path, &new_content)?;
    Ok(new_content)
}
