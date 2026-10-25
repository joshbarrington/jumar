use std::process::Command;

/// Like `jumar init zsh | head -0`: the reader is gone before jumar writes.
#[test]
fn init_should_exit_cleanly_when_stdout_is_closed() {
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_jumar"))
        .args(["init", "zsh"])
        .stdout(writer)
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
}
