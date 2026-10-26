//! The herdr plugin entry points: `jumar --action <id>`, run by herdr for the actions in
//! `herdr-plugin.toml`.

use std::path::PathBuf;

use anyhow::{Result, bail};
use jumar_core::herdr::Herdr;
use serde::Deserialize;

const PLUGIN_ID: &str = "joshbarrington.jumar";
const ENTRYPOINT: &str = "pane";

pub fn action(id: &str) -> Result<()> {
    match id {
        "open" | "toggle" => open(),
        other => bail!("unknown action `{other}`"),
    }
}

// The popup takes all input while it is open, so herdr never sees the toggle key then:
// toggle only ever opens it, and jumar closes it by exiting.
fn open() -> Result<()> {
    let cwd = focused_cwd()
        .or_else(std::env::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    Herdr::new().open_popup(&plugin_id(), ENTRYPOINT, &cwd)?;
    Ok(())
}

/// The part of the context herdr passes an action that says where the user is.
#[derive(Deserialize)]
struct Context {
    focused_pane_cwd: Option<PathBuf>,
    workspace_cwd: Option<PathBuf>,
}

/// The directory of the focused pane, so jumar lists that repo. Without it, herdr starts
/// the popup in the plugin's own directory.
fn focused_cwd() -> Option<PathBuf> {
    let json = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?;
    let context: Context = serde_json::from_str(&json).ok()?;
    context.focused_pane_cwd.or(context.workspace_cwd)
}

fn plugin_id() -> String {
    std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.to_owned())
}
