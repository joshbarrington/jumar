//! The herdr plugin entry points: `jumar --action <id>`, run by herdr for the actions in
//! `herdr-plugin.toml`.

use anyhow::{Result, bail};
use jumar_core::herdr::Herdr;

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
    Herdr::new().open_popup(&plugin_id(), ENTRYPOINT)?;
    Ok(())
}

fn plugin_id() -> String {
    std::env::var("HERDR_PLUGIN_ID").unwrap_or_else(|_| PLUGIN_ID.to_owned())
}
