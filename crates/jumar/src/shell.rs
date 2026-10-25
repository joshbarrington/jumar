use std::path::Path;

use anyhow::{Context, Result};
use clap::ValueEnum;

/// Set by the `jumar init` function; jumar writes the chosen directory there. A file is
/// used rather than stdout because the TUI draws on stdout.
const CWD_FILE: &str = "JUMAR_CWD_FILE";

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

// `ret` rather than `status`, which is read-only in zsh.
const POSIX: &str = r#"jumar() {
    local tmp dir ret
    tmp="$(mktemp -t jumar.XXXXXX)" || return
    JUMAR_CWD_FILE="$tmp" command jumar "$@"
    ret=$?
    dir="$(cat -- "$tmp")"
    rm -f -- "$tmp"
    if [ -n "$dir" ] && [ "$dir" != "$PWD" ]; then
        cd -- "$dir" || return
    fi
    return $ret
}
"#;

const FISH: &str = r#"function jumar
    set -l tmp (mktemp -t jumar.XXXXXX); or return
    JUMAR_CWD_FILE=$tmp command jumar $argv
    set -l ret $status
    set -l dir (cat -- $tmp)
    rm -f -- $tmp
    if test -n "$dir"; and test "$dir" != "$PWD"
        cd -- $dir
    end
    return $ret
end
"#;

pub fn init_script(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash | Shell::Zsh => POSIX,
        Shell::Fish => FISH,
    }
}

/// Passes `dir` to the shell function, or prints it with a setup hint without one.
pub fn hand_off(dir: &Path) -> Result<()> {
    if let Some(file) = std::env::var_os(CWD_FILE) {
        return std::fs::write(&file, dir.as_os_str().as_encoded_bytes())
            .with_context(|| format!("could not write {}", Path::new(&file).display()));
    }
    println!("{}", dir.display());
    let shell = parent_command().or_else(|| std::env::var("SHELL").ok());
    eprintln!("jumar: {} to cd on Enter", setup_hint(shell.as_deref()));
    Ok(())
}

/// `$SHELL` is the login shell, which need not be the one jumar was started from.
fn parent_command() -> Option<String> {
    let output = std::process::Command::new("ps")
        .args([
            "-o",
            "comm=",
            "-p",
            &std::os::unix::process::parent_id().to_string(),
        ])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_owned()).filter(|s| !s.is_empty())
}

fn setup_hint(shell: Option<&str>) -> &'static str {
    let name = shell.and_then(|s| Path::new(s).file_name()?.to_str());
    match name.map(|n| n.trim_start_matches('-')) {
        Some("fish") => "add `jumar init fish | source` to ~/.config/fish/config.fish",
        Some("bash") => "add `eval \"$(jumar init bash)\"` to ~/.bashrc",
        Some("zsh") => "add `eval \"$(jumar init zsh)\"` to ~/.zshrc",
        _ => "set up `jumar init <bash|zsh|fish>` in your shell config",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_hint_should_use_fish_syntax_for_fish() {
        assert!(setup_hint(Some("/opt/homebrew/bin/fish")).contains("| source"));
    }

    #[test]
    fn setup_hint_should_name_all_shells_when_unknown() {
        assert!(setup_hint(None).contains("<bash|zsh|fish>"));
    }
}
