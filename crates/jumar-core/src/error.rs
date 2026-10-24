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
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
