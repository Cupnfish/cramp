use crate::converters::cargo_diag_to_my;
use crate::error::{RaError, Result};
use crate::models::{CargoTestOutput, MyDiagnostic};
use cargo_metadata::{Error as CargoMetadataError, Message};
use log::{debug, info, warn};
use path_clean::PathClean;
use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

/// Runs `cargo check --message-format=json` and parses diagnostics.
/// Runs in the current thread (blocking).
pub fn run_external_cargo_check(project_root: &Path) -> Result<Vec<MyDiagnostic>> {
    info!(
        "Running external `cargo check --message-format=json` in {:?}",
        project_root
    );
    let mut command = Command::new("cargo")
        .args(["check", "--message-format=json", "--all-targets"])
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // Capture stderr to avoid polluting console
        .spawn()
        .map_err(|e| RaError::Process(format!("Failed to spawn cargo check: {}", e)))?;

    let reader = BufReader::new(command.stdout.take().expect("stdout captured"));
    let stderr_reader = BufReader::new(command.stderr.take().expect("stderr captured"));
    let mut diagnostics = Vec::new();
    // Use canonical path for consistent path joining in converter
    let root_path_canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf())
        .clean();

    // Log cargo stderr in a separate thread to avoid blocking and buffer issues
    let project_root_clone = project_root.to_path_buf();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in stderr_reader.lines() {
            if let Ok(line) = line {
                debug!(
                    "[cargo stderr {:?}]: {}",
                    project_root_clone.file_name().unwrap_or_default(),
                    line
                );
            }
        }
    });

    // Parse stdout JSON stream
    for message in Message::parse_stream(reader) {
        let message = message.map_err(|e| RaError::CargoMetadata(CargoMetadataError::Io(e)))?;

        match message {
            Message::CompilerMessage(msg) => {
                diagnostics.extend(cargo_diag_to_my(&msg.message, &root_path_canonical));
            }
            Message::BuildFinished(finished) => {
                debug!("Cargo build finished: success={}", finished.success);
            }
            Message::TextLine(line) => {
                if !line.trim().starts_with('{') {
                    debug!("[cargo text]: {}", line);
                } else {
                    warn!(
                        "[cargo text]: Possible JSON parse error swallowed by cargo_metadata: {}",
                        line
                    );
                }
            }
            Message::CompilerArtifact(_) | Message::BuildScriptExecuted(_) => {}
            _ => {}
        }
    }
    let status = command.wait()?;
    info!("`cargo check` finished with status: {}", status);
    if !status.success() {
        warn!(
            "`cargo check` command itself failed (exit code: {:?}), but parsed {} diagnostics.",
            status.code(),
            diagnostics.len()
        );
    }
    // Filter out duplicates that might arise from multiple targets/spans
    diagnostics.sort();
    diagnostics.dedup();
    Ok(diagnostics)
}

/// Runs `cargo test` and captures output.
/// Runs in the current thread (blocking).
pub fn run_external_cargo_test(
    project_root: &Path,
    test_name: Option<&str>,
) -> Result<CargoTestOutput> {
    info!(
        "Running external `cargo test` in {:?} for test {:?}",
        project_root, test_name
    );
    let mut cmd = Command::new("cargo");
    cmd.arg("test");
    if let Some(name) = test_name {
        cmd.arg(name);
    }
    // Arguments after -- go directly to the test binary
    cmd.arg("--");
    cmd.arg("--color=always");

    let output = cmd
        .current_dir(project_root)
        .output() // captures both stdout and stderr and waits for completion
        .map_err(|e| RaError::Process(format!("Failed to run cargo test: {}", e)))?;

    Ok(CargoTestOutput {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        success: output.status.success(),
    })
}
