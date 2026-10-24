use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Starts `command` in `dir` without waiting, detached from the terminal.
pub fn detach(command: &[String], dir: &Path) -> Result<()> {
    let (program, args) = split(command)?;
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("could not start {program}"))?;
    Ok(())
}

/// Runs `command` in `dir` with the terminal handed over, and waits for it.
pub fn run_attached(command: &[String], dir: &Path) -> Result<()> {
    let (program, args) = split(command)?;
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .with_context(|| format!("could not start {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// Runs `command` to completion without a terminal, failing with its stderr.
pub fn run_captured(command: &[String], dir: &Path) -> Result<()> {
    let (program, args) = split(command)?;
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("could not start {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn split(command: &[String]) -> Result<(&String, &[String])> {
    command.split_first().context("tool has an empty command")
}
