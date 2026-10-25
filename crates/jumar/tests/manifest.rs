#![expect(clippy::unwrap_used, reason = "fixture setup should panic on failure")]

use std::fs;
use std::path::Path;

/// `herdr/install.sh` downloads the release tagged with the manifest's version.
#[test]
fn herdr_manifest_version_should_match_crate_version() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../herdr-plugin.toml");
    let text = fs::read_to_string(manifest).unwrap();

    let version = text
        .lines()
        .find_map(|line| line.strip_prefix("version = "))
        .map(|v| v.trim_matches('"'));

    assert_eq!(version, Some(env!("CARGO_PKG_VERSION")));
}

fn claude_manifest(name: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.claude-plugin")
        .join(name);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn claude_plugin_version_should_match_crate_version() {
    assert_eq!(
        claude_manifest("plugin.json")["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[test]
fn claude_marketplace_version_should_match_crate_version() {
    assert_eq!(
        claude_manifest("marketplace.json")["plugins"][0]["version"],
        env!("CARGO_PKG_VERSION")
    );
}
