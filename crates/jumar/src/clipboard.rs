use std::io::{self, Write};
use std::process::{Command, Stdio};

use base64::Engine;

const TOOLS: [&[&str]; 4] = [
    &["pbcopy"],
    &["wl-copy"],
    &["xclip", "-selection", "clipboard"],
    &["xsel", "--clipboard", "--input"],
];

/// Tries the platform clipboard tools, then the terminal's OSC 52, which also works
/// over ssh when the terminal allows it.
pub fn copy(text: &str) -> io::Result<()> {
    if TOOLS.iter().any(|tool| pipe(tool, text).is_ok()) {
        return Ok(());
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    let mut stdout = io::stdout();
    write!(stdout, "\x1b]52;c;{encoded}\x07")?;
    stdout.flush()
}

fn pipe(tool: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(tool[0])
        .args(&tool[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?
        .write_all(text.as_bytes())?;
    if child.wait()?.success() {
        Ok(())
    } else {
        Err(io::Error::other("clipboard tool failed"))
    }
}
