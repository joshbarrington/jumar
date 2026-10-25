use std::io;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid config {path}: {source}")]
    Config {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("invalid config {path}: {message}")]
    ConfigValue { path: PathBuf, message: String },
    #[error("invalid glob `{pattern}`: {source}")]
    Glob {
        pattern: String,
        #[source]
        source: glob::PatternError,
    },
    #[error("could not run git: {0}")]
    GitSpawn(#[source] io::Error),
    #[error("`git {args}` failed in {dir}: {stderr}")]
    Git {
        dir: PathBuf,
        args: String,
        stderr: String,
    },
    #[error("unexpected git output: {0}")]
    GitParse(String),
    #[error("could not run herdr: {0}")]
    HerdrSpawn(#[source] io::Error),
    #[error("{0}")]
    Herdr(String),
    #[error("{} has no default branch (no origin/HEAD, main or master)", .0.display())]
    NoDefaultBranch(PathBuf),
    #[error("could not copy {}: {source}", path.display())]
    Copy {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("`{command}` failed in the new worktree: {output}")]
    Setup { command: String, output: String },
    #[error("path is not valid UTF-8: {}", .0.display())]
    NonUtf8Path(PathBuf),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
