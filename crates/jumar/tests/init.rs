#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A `jumar` stand-in that hands off `$TARGET` (if set) and exits with `$CODE`.
const FAKE_JUMAR: &str = r#"#!/bin/sh
if [ -n "$TARGET" ]; then printf %s "$TARGET" > "$JUMAR_CWD_FILE"; fi
exit "${CODE:-0}"
"#;

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let fake = bin.join("jumar");
        fs::write(&fake, FAKE_JUMAR).unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        Self { dir }
    }

    fn target(&self) -> PathBuf {
        fs::canonicalize(self.dir.path().join("target")).unwrap()
    }

    /// Runs `jumar; echo "$? $PWD"` in `shell` with the function loaded.
    fn run(&self, shell: &str, target: Option<&Path>, code: u8) -> Option<String> {
        let script = Command::new(env!("CARGO_BIN_EXE_jumar"))
            .args(["init", shell])
            .output()
            .unwrap()
            .stdout;
        let script = String::from_utf8(script).unwrap();
        let (flags, report): (&[&str], _) = match shell {
            "bash" => (&["--norc", "--noprofile", "-c"], r#"echo "$? $PWD""#),
            "zsh" => (&["-f", "-c"], r#"echo "$? $PWD""#),
            _ => (&["--no-config", "-c"], r#"echo "$status $PWD""#),
        };
        let path = format!(
            "{}:{}",
            self.dir.path().join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = Command::new(shell);
        command
            .args(flags)
            .arg(format!("{script}\njumar\n{report}"))
            .current_dir(self.dir.path())
            .env("PATH", path)
            .env("CODE", code.to_string());
        if let Some(target) = target {
            command.env("TARGET", target);
        }
        let output = command.output().ok()?;
        Some(String::from_utf8(output.stdout).unwrap().trim().to_owned())
    }
}

fn installed(shell: &str) -> bool {
    Command::new(shell).arg("-c").arg("true").output().is_ok()
}

fn shells() -> impl Iterator<Item = &'static str> {
    ["bash", "zsh", "fish"].into_iter().filter(|s| installed(s))
}

#[test]
fn init_function_should_cd_to_handed_off_directory() {
    let fx = Fixture::new();
    for shell in shells() {
        let out = fx.run(shell, Some(&fx.target()), 0).unwrap();

        assert_eq!(out, format!("0 {}", fx.target().display()), "{shell}");
    }
}

#[test]
fn init_function_should_stay_put_without_hand_off() {
    let fx = Fixture::new();
    let here = fs::canonicalize(fx.dir.path()).unwrap();
    for shell in shells() {
        let out = fx.run(shell, None, 0).unwrap();

        assert_eq!(out, format!("0 {}", here.display()), "{shell}");
    }
}

#[test]
fn init_function_should_keep_exit_status() {
    let fx = Fixture::new();
    for shell in shells() {
        let out = fx.run(shell, None, 3).unwrap();

        assert!(out.starts_with("3 "), "{shell}: {out}");
    }
}
