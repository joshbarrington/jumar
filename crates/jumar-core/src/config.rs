//! ```toml
//! default_create_source = "work"
//! # Fetch the remote branch a new worktree starts from, such as origin/main, first.
//! fetch_on_create = true
//! # Re-read statuses and herdr agents this often while open; 0 turns it off.
//! refresh_seconds = 5
//!
//! # The branch worktrees are checked against, and `{base}` and new worktrees start
//! # from. Without one, origin/HEAD, then main or master.
//! [[repo]]
//! path = "~/work/app"
//! default_branch = "develop"
//! # After creating a worktree: copy these files from the main checkout when present
//! # (globs, relative to it), then run each command with `sh -c` in the new worktree.
//! copy = [".env", "config/*.local.toml"]
//! on_create = ["npm install"]
//!
//! [[source]]
//! name = "work"
//! globs = ["~/work/*/.worktrees/*"]
//! # {repo} is the main checkout's path, {repo_name} its directory name.
//! create_path = "{repo}/.worktrees/{name}"
//! open_in_herdr = false
//!
//! # Same name as a preset: replaces its globs (empty disables it), and its
//! # create_path and open_in_herdr when given. Without default_create_source, new
//! # worktrees go to the herdr source inside herdr, and the claude source outside it.
//! [[source]]
//! name = "claude"
//! globs = ["~/code/*/.claude/worktrees/*"]
//!
//! # Run with <Space> then `key` on a worktree. Same name as a preset (revdiff, reviewr)
//! # changes only the fields given; `command = []` removes it. Placeholders: {path}
//! # {repo} {branch} {base} {default_branch}, where {base} is the merge-base with the
//! # default branch.
//! [[tool]]
//! name = "lazygit"
//! key = "g"
//! command = ["lazygit"]
//! mode = "suspend"   # suspend | herdr-workspace | detach
//! ```

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::tool::unknown_placeholder;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub name: String,
    #[serde(default)]
    pub globs: Vec<String>,
    /// Where new worktrees go. Without one, the source cannot create worktrees.
    pub create_path: Option<String>,
    /// Open newly created worktrees as herdr workspaces.
    pub open_in_herdr: Option<bool>,
}

impl Source {
    #[must_use]
    pub fn opens_in_herdr(&self) -> bool {
        self.open_in_herdr.unwrap_or(false)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    pub name: String,
    pub key: char,
    pub command: Vec<String>,
    pub mode: ToolMode,
}

/// A `[[tool]]` entry: a new tool, or changes to the preset of the same name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolEntry {
    name: String,
    key: Option<char>,
    command: Option<Vec<String>>,
    mode: Option<ToolMode>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolMode {
    /// Hand the terminal to the tool in the worktree, and come back when it exits.
    #[default]
    Suspend,
    /// Focus the worktree's herdr workspace, run the command there, and close jumar.
    HerdrWorkspace,
    /// Start the tool in the background and keep going.
    Detach,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    /// The main checkout; a leading `~` is the home directory.
    pub path: String,
    pub default_branch: Option<String>,
    #[serde(default)]
    pub copy: Vec<String>,
    #[serde(default)]
    pub on_create: Vec<String>,
}

impl Repo {
    fn expanded_path(&self, home: &Path) -> PathBuf {
        expand_home(&self.path, home)
    }
}

/// `path` with a leading `~` replaced by `home`.
#[must_use]
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

const DEFAULT_REFRESH_SECONDS: u64 = 5;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, rename = "repo")]
    repos: Vec<Repo>,
    refresh_seconds: Option<u64>,
    fetch_on_create: Option<bool>,
    #[serde(default, rename = "source")]
    sources: Vec<Source>,
    #[serde(default, rename = "tool")]
    tools: Vec<ToolEntry>,
    default_create_source: Option<String>,
}

/// The presets merged with the user's config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    repos: Vec<Repo>,
    refresh_seconds: u64,
    fetch_on_create: bool,
    sources: Vec<Source>,
    tools: Vec<Tool>,
    default_create_source: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            refresh_seconds: DEFAULT_REFRESH_SECONDS,
            fetch_on_create: true,
            sources: presets(),
            tools: preset_tools(),
            default_create_source: None,
        }
    }
}

impl Config {
    /// # Errors
    ///
    /// Fails on invalid TOML, unknown fields, or tools that are incomplete, share a key,
    /// or use an unknown placeholder.
    pub fn from_toml(text: &str, path: &Path) -> Result<Self> {
        let file: File = toml::from_str(text).map_err(|source| Error::Config {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        let tools = merge_tools(file.tools).map_err(|message| Error::ConfigValue {
            path: path.to_path_buf(),
            message,
        })?;
        Ok(Self {
            repos: file.repos,
            refresh_seconds: file.refresh_seconds.unwrap_or(DEFAULT_REFRESH_SECONDS),
            fetch_on_create: file.fetch_on_create.unwrap_or(true),
            sources: merge_sources(&file.sources),
            tools,
            default_create_source: file.default_create_source,
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

    /// Presets first, then the user's own sources.
    #[must_use]
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Presets first, then the user's own tools.
    #[must_use]
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    #[must_use]
    pub fn default_create_source(&self) -> Option<&str> {
        self.default_create_source.as_deref()
    }

    #[must_use]
    pub fn fetch_on_create(&self) -> bool {
        self.fetch_on_create
    }

    /// `None` when auto-refresh is off.
    #[must_use]
    pub fn refresh_interval(&self) -> Option<Duration> {
        (self.refresh_seconds > 0).then(|| Duration::from_secs(self.refresh_seconds))
    }

    /// The `[[repo]]` paths, with a leading `~` expanded.
    #[must_use]
    pub fn repo_paths(&self, home: &Path) -> Vec<PathBuf> {
        self.repos.iter().map(|r| r.expanded_path(home)).collect()
    }

    /// The `[[repo]]` whose main checkout is `repo`.
    #[must_use]
    pub fn repo(&self, repo: &Path, home: &Path) -> Option<&Repo> {
        self.repos.iter().find(|r| {
            let path = r.expanded_path(home);
            std::fs::canonicalize(&path).unwrap_or(path) == repo
        })
    }

    /// The configured default branch of the repo whose main checkout is `repo`.
    #[must_use]
    pub fn default_branch(&self, repo: &Path, home: &Path) -> Option<&str> {
        self.repo(repo, home)?.default_branch.as_deref()
    }
}

pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".config")))?;
    Some(base.join("jumar").join("config.toml"))
}

/// Same-named user sources replace a preset's globs, and its other fields when given.
fn merge_sources(user_sources: &[Source]) -> Vec<Source> {
    let mut sources = presets();
    for user in user_sources {
        match sources.iter_mut().find(|s| s.name == user.name) {
            Some(preset) => {
                preset.globs.clone_from(&user.globs);
                if user.create_path.is_some() {
                    preset.create_path.clone_from(&user.create_path);
                }
                if user.open_in_herdr.is_some() {
                    preset.open_in_herdr = user.open_in_herdr;
                }
            }
            None => sources.push(user.clone()),
        }
    }
    sources
}

/// Same-named entries change only the preset fields they give; an empty command drops the
/// tool.
fn merge_tools(entries: Vec<ToolEntry>) -> Result<Vec<Tool>, String> {
    let mut tools = preset_tools();
    for entry in entries {
        if let Some(preset) = tools.iter_mut().find(|t| t.name == entry.name) {
            preset.key = entry.key.unwrap_or(preset.key);
            preset.command = entry.command.unwrap_or_else(|| preset.command.clone());
            preset.mode = entry.mode.unwrap_or(preset.mode);
            continue;
        }
        let (Some(key), Some(command)) = (entry.key, entry.command) else {
            return Err(format!("tool `{}` needs a key and a command", entry.name));
        };
        tools.push(Tool {
            name: entry.name,
            key,
            command,
            mode: entry.mode.unwrap_or_default(),
        });
    }
    tools.retain(|t| !t.command.is_empty());
    for (index, tool) in tools.iter().enumerate() {
        if tool.key.is_whitespace() || tool.key.is_control() {
            return Err(format!("tool `{}` needs a visible key", tool.name));
        }
        if let Some(other) = tools[..index].iter().find(|t| t.key == tool.key) {
            return Err(format!(
                "tools `{}` and `{}` both use <Space>{}",
                other.name, tool.name, tool.key
            ));
        }
        if let Some(name) = tool.command.iter().find_map(|arg| unknown_placeholder(arg)) {
            return Err(format!(
                "tool `{}` uses unknown placeholder {{{name}}}",
                tool.name
            ));
        }
    }
    Ok(tools)
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
            open_in_herdr: None,
        },
        Source {
            name: "herdr".into(),
            globs: vec!["~/.herdr/worktrees/*/*".into()],
            create_path: Some("~/.herdr/worktrees/{repo_name}/{name}".into()),
            open_in_herdr: Some(true),
        },
    ]
}

fn preset_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "revdiff".into(),
            key: 'd',
            command: vec!["revdiff".into(), "{base}".into()],
            mode: ToolMode::Suspend,
        },
        // reviewr's `open` acts on the focused workspace, which herdr-workspace mode
        // focuses first.
        Tool {
            name: "reviewr".into(),
            key: 'r',
            command: [
                "herdr",
                "plugin",
                "action",
                "invoke",
                "open",
                "--plugin",
                "persiyanov.reviewr",
            ]
            .map(Into::into)
            .to_vec(),
            mode: ToolMode::HerdrWorkspace,
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
        assert_eq!(names(parse("").sources()), ["claude", "herdr"]);
    }

    #[test]
    fn sources_should_append_user_source_after_presets() {
        let config = parse("[[source]]\nname = \"work\"\nglobs = [\"~/w/*\"]\n");

        assert_eq!(names(config.sources()), ["claude", "herdr", "work"]);
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
    fn tools_should_include_presets() {
        let config = parse("");

        let names: Vec<_> = config.tools().iter().map(|t| t.name.as_str()).collect();

        assert_eq!(names, ["revdiff", "reviewr"]);
    }

    #[test]
    fn tools_should_replace_preset_with_same_name() {
        let config = parse("[[tool]]\nname = \"revdiff\"\nkey = \"v\"\ncommand = [\"revdiff\"]\n");

        assert_eq!(config.tools()[0].key, 'v');
    }

    #[test]
    fn tools_should_drop_tool_with_empty_command() {
        let config = parse("[[tool]]\nname = \"reviewr\"\ncommand = []\n");

        assert_eq!(config.tools().len(), 1);
    }

    #[test]
    fn tools_should_keep_preset_command_when_only_key_given() {
        let config = parse("[[tool]]\nname = \"revdiff\"\nkey = \"v\"\n");

        assert_eq!(config.tools()[0].command, ["revdiff", "{base}"]);
    }

    fn error(text: &str) -> String {
        Config::from_toml(text, Path::new("c.toml"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn from_toml_should_reject_new_tool_without_command() {
        assert_eq!(
            error("[[tool]]\nname = \"x\"\nkey = \"x\"\n"),
            "invalid config c.toml: tool `x` needs a key and a command"
        );
    }

    #[test]
    fn from_toml_should_reject_tools_sharing_a_key() {
        assert_eq!(
            error("[[tool]]\nname = \"x\"\nkey = \"d\"\ncommand = [\"x\"]\n"),
            "invalid config c.toml: tools `revdiff` and `x` both use <Space>d"
        );
    }

    #[test]
    fn from_toml_should_reject_unknown_placeholder() {
        assert_eq!(
            error("[[tool]]\nname = \"x\"\nkey = \"x\"\ncommand = [\"x\", \"{bsae}\"]\n"),
            "invalid config c.toml: tool `x` uses unknown placeholder {bsae}"
        );
    }

    #[test]
    fn tools_should_parse_mode() {
        let config =
            parse("[[tool]]\nname = \"x\"\nkey = \"x\"\ncommand = [\"x\"]\nmode = \"detach\"\n");

        assert_eq!(config.tools()[2].mode, ToolMode::Detach);
    }

    #[test]
    fn refresh_interval_should_default_to_five_seconds() {
        assert_eq!(parse("").refresh_interval(), Some(Duration::from_secs(5)));
    }

    #[test]
    fn refresh_interval_should_be_off_when_zero() {
        assert_eq!(parse("refresh_seconds = 0\n").refresh_interval(), None);
    }

    #[test]
    fn repo_should_read_setup() {
        let config = parse("[[repo]]\npath = \"/a\"\ncopy = [\".env\"]\non_create = [\"make\"]\n");

        let repo = config.repo(Path::new("/a"), Path::new("/h")).unwrap();

        assert_eq!(
            (&repo.copy[..], &repo.on_create[..]),
            (&[".env".to_owned()][..], &["make".to_owned()][..])
        );
    }

    #[test]
    fn repo_paths_should_expand_home() {
        let config = parse("[[repo]]\npath = \"~/app\"\n\n[[repo]]\npath = \"/srv/api\"\n");

        let paths = config.repo_paths(Path::new("/h"));

        assert_eq!(paths, [PathBuf::from("/h/app"), PathBuf::from("/srv/api")]);
    }

    #[test]
    fn default_branch_should_match_repo_under_home() {
        let config = parse("[[repo]]\npath = \"~/app\"\ndefault_branch = \"develop\"\n");

        let branch =
            config.default_branch(Path::new("/nonexistent/h/app"), Path::new("/nonexistent/h"));

        assert_eq!(branch, Some("develop"));
    }

    #[test]
    fn default_branch_should_be_none_for_other_repo() {
        let config = parse("[[repo]]\npath = \"/a\"\ndefault_branch = \"develop\"\n");

        assert_eq!(
            config.default_branch(Path::new("/b"), Path::new("/h")),
            None
        );
    }

    #[test]
    fn default_create_source_should_be_unset_by_default() {
        assert_eq!(parse("").default_create_source(), None);
    }

    #[test]
    fn sources_should_keep_preset_open_in_herdr_when_user_omits_it() {
        let config = parse("[[source]]\nname = \"herdr\"\nglobs = [\"~/h/*\"]\n");

        assert!(config.sources()[1].opens_in_herdr());
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
