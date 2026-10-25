use std::io::{self, Write};

use anyhow::{Context, Result};

/// Writes `text` to stdout. A reader that stops early, like `head`, is not an error.
pub fn write(text: &str) -> Result<()> {
    let mut stdout = io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        written => written.context("could not write to stdout"),
    }
}
