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
    eprintln!(
        "jumar: add `eval \"$(jumar init zsh)\"` (or bash/fish) to your shell config to cd on Enter"
    );
    Ok(())
}
