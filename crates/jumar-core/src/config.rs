//! ```toml
//! default_create_source = "work"
//!
//! [[source]]
//! name = "work"
//! globs = ["~/work/*/.worktrees/*"]
//! # {repo} is the main checkout's path, {repo_name} its directory name.
//! create_path = "{repo}/.worktrees/{name}"
//!
//! # Same name as a preset: replaces its globs (empty disables it), and its
//! # create_path when given.
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
    /// Where new worktrees go. Without one, the source cannot create worktrees.
    pub create_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, rename = "source")]
    user_sources: Vec<Source>,
    default_create_source: Option<String>,
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
                Some(preset) => {
                    preset.globs.clone_from(&user.globs);
                    if user.create_path.is_some() {
                        preset.create_path.clone_from(&user.create_path);
                    }
                }
                None => sources.push(user.clone()),
            }
        }
        sources
    }

    #[must_use]
    pub fn default_create_source(&self) -> &str {
        self.default_create_source.as_deref().unwrap_or("claude")
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
            globs: vec![
                "~/*/.claude/worktrees/*".into(),
                "~/github/*/.claude/worktrees/*".into(),
            ],
            create_path: Some("{repo}/.claude/worktrees/{name}".into()),
        },
        Source {
            name: "herdr".into(),
            globs: vec!["~/.herdr/worktrees/*/*".into()],
            create_path: None,
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
    fn sources_should_keep_preset_create_path_when_user_omits_it() {
        let config = parse("[[source]]\nname = \"claude\"\nglobs = [\"~/c/*\"]\n");

        assert!(config.sources()[0].create_path.is_some());
    }

    #[test]
    fn sources_should_replace_preset_create_path_when_given() {
        let config = parse("[[source]]\nname = \"claude\"\ncreate_path = \"~/w/{name}\"\n");

        assert_eq!(
            config.sources()[0].create_path.as_deref(),
            Some("~/w/{name}")
        );
    }

    #[test]
    fn default_create_source_should_be_claude_when_unset() {
        assert_eq!(parse("").default_create_source(), "claude");
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
