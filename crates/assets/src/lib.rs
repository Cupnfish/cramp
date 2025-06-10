//! Static assets for the CRAMP project
//!
//! This crate provides a centralized location for all static assets
//! to avoid duplicate inclusion in the binary.

/// The content of the README.md file
pub const README_CONTENT: &str = include_str!("../../../README.md");

/// The content of the cramp_rule.md file
pub const RULE_CONTENT: &str = include_str!("../../../cramp_rule.md");

/// Get the README content
pub fn get_readme() -> &'static str {
    README_CONTENT
}

/// Get the rule content
pub fn get_rule() -> &'static str {
    RULE_CONTENT
}
