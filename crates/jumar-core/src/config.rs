//! ```toml
//! [[source]]
//! name = "work"
//! globs = ["~/work/*/.worktrees/*"]
//!
//! # Same name as a preset: replaces it. Empty globs disable it.
//! [[source]]
//! name = "claude"
//! globs = ["~/code/*/.claude/worktrees/*"]
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub name: String,
    #[serde(default)]
    pub globs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "source")]
    user_sources: Vec<Source>,
}

impl Config {
    /// # Errors
    ///
    /// Fails on invalid TOML or unknown fields.
    pub fn from_toml(text: &str, path: &Path) -> Result<Self> {
        toml::from_str(text).map_err(|source| Error::Config {
            path: path.to_path_buf(),
            source: Box::new(source),
        })
    }

    /// A missing file yields the default config.
    ///
    /// # Errors
    ///
    /// Fails when the file exists but cannot be read or parsed.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text, path),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(Error::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Presets first, with same-named user sources replacing them, then the rest.
    #[must_use]
    pub fn sources(&self) -> Vec<Source> {
        let mut sources = presets();
        for user in &self.user_sources {
            match sources.iter_mut().find(|s| s.name == user.name) {
                Some(preset) => preset.globs.clone_from(&user.globs),
                None => sources.push(user.clone()),
            }
        }
        sources
    }
}

pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".config")))?;
    Some(base.join("jumar").join("config.toml"))
}

fn presets() -> Vec<Source> {
    vec![
        Source {
            name: "claude".into(),
            globs: vec!["~/github/*/.claude/worktrees/*".into()],
        },
        Source {
            name: "herdr".into(),
            globs: vec!["~/.herdr/worktrees/*/*".into()],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::from_toml(text, Path::new("config.toml")).unwrap()
    }

    fn names(sources: &[Source]) -> Vec<&str> {
        sources.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn sources_should_be_presets_when_config_empty() {
        assert_eq!(names(&parse("").sources()), ["claude", "herdr"]);
    }

    #[test]
    fn sources_should_append_user_source_after_presets() {
        let config = parse("[[source]]\nname = \"work\"\nglobs = [\"~/w/*\"]\n");

        assert_eq!(names(&config.sources()), ["claude", "herdr", "work"]);
    }

    #[test]
    fn sources_should_replace_preset_globs_when_name_matches() {
        let config = parse("[[source]]\nname = \"claude\"\nglobs = [\"~/code/*/x/*\"]\n");

        assert_eq!(config.sources()[0].globs, ["~/code/*/x/*"]);
    }

    #[test]
    fn sources_should_disable_preset_when_user_globs_empty() {
        let config = parse("[[source]]\nname = \"herdr\"\n");

        assert!(config.sources()[1].globs.is_empty());
    }

    #[test]
    fn from_toml_should_reject_unknown_fields() {
        let err = Config::from_toml("colour = 1\n", Path::new("c.toml")).unwrap_err();

        assert!(
            err.to_string().starts_with("invalid config c.toml"),
            "{err}"
        );
    }

    #[test]
    fn load_should_return_default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();

        let config = Config::load(&dir.path().join("none.toml")).unwrap();

        assert_eq!(config, Config::default());
    }
}
