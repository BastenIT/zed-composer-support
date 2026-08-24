use std::{fs, path::Path};

fn first_toml_version(path: &Path) -> String {
    fs::read_to_string(path)
        .expect("read manifest")
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("version = \"")
                .and_then(|version| version.strip_suffix('"'))
                .map(str::to_owned)
        })
        .expect("version field")
}

#[test]
fn release_versions_match() {
    let server_directory = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = server_directory.parent().expect("repository root");
    let expected = env!("CARGO_PKG_VERSION");

    assert_eq!(first_toml_version(&root.join("Cargo.toml")), expected);
    assert_eq!(first_toml_version(&root.join("extension.toml")), expected);

    let launcher = fs::read_to_string(root.join("src/lib.rs")).expect("read extension launcher");
    assert!(
        launcher.contains(&format!("const SERVER_VERSION: &str = \"{expected}\";")),
        "src/lib.rs must use server version {expected}"
    );

    let neovim = fs::read_to_string(root.join("lua/composer_support/init.lua"))
        .expect("read Neovim integration");
    assert!(
        neovim.contains(&format!("local SERVER_VERSION = \"{expected}\"")),
        "lua/composer_support/init.lua must use server version {expected}"
    );
}
