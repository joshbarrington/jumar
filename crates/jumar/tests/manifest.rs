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
