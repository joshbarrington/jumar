//! The herdr plugin entry points: `jumar --action <id>`, run by herdr for the actions in
//! `herdr-plugin.toml`, and the bookkeeping of jumar's own overlay pane.

use std::fs;
use std::path::PathBuf;

use anyhow::{Result, bail};
use jumar_core::herdr::Herdr;

const PLUGIN_ID: &str = "joshbarrington.jumar";
const ENTRYPOINT: &str = "pane";

pub fn action(id: &str) -> Result<()> {
    match id {
        "open" => open(),
        "toggle" => toggle(),
        other => bail!("unknown action `{other}`"),
    }
}

fn open() -> Result<()> {
    Herdr::new().open_overlay(&plugin_id(), ENTRYPOINT)?;
    Ok(())
}

fn toggle() -> Result<()> {
    if let Some(file) = pane_file()
        && let Ok(pane) = fs::read_to_string(&file)
    {
        // A pane that no longer exists leaves a stale file behind; open a new one then.
        let closed = Herdr::new().close_pane(pane.trim()).is_ok();
        let _ = fs::remove_file(&file);
        if closed {
            return Ok(());
        }
    }
    open()
}

/// Records jumar's overlay pane while it runs, so `toggle` can close it.
pub struct PaneRecord(Option<PathBuf>);

impl PaneRecord {
    /// Writes the record only when jumar runs as the plugin's pane.
    pub fn register() -> Self {
        let in_plugin_pane = std::env::var_os("HERDR_PLUGIN_ENTRYPOINT_ID").is_some();
        let file = pane_file().filter(|_| in_plugin_pane).and_then(|file| {
            let pane = std::env::var("HERDR_PANE_ID").ok()?;
            fs::create_dir_all(file.parent()?).ok()?;
            fs::write(&file, pane).ok()?;
            Some(file)
        });
        Self(file)
    }
}

impl Drop for PaneRecord {
    fn drop(&mut self) {
        if let Some(file) = &self.0 {
            let _ = fs::remove_file(file);
        }
    }
}

fn plugin_id() -> String {
    std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.to_owned())
}

fn pane_file() -> Option<PathBuf> {
    let dir = std::env::var_os("HERDR_PLUGIN_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::home_dir()
                .map(|home| home.join(".local/state/herdr/plugins").join(plugin_id()))
        })?;
    Some(dir.join("pane"))
}
