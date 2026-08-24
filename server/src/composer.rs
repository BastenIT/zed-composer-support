use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use jsonc_parser::{tokens::Token as JsonToken, Scanner, ScannerOptions};
use serde_json::Value;
use tower_lsp_server::ls_types::{Position, Range};
use url::Url;

const MAX_INSTALLED_METADATA_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    ObjectStart,
    ObjectEnd,
    ArrayStart,
    ArrayEnd,
    Colon,
    Comma,
    String,
    Literal,
}

#[derive(Clone, Debug)]
struct Token {
    kind: TokenKind,
    start: usize,
    end: usize,
    value: String,
}

#[derive(Clone, Debug)]
enum NodeKind {
    Object(Vec<Property>),
    Array,
    Scalar,
}

#[derive(Clone, Debug)]
struct Node {
    kind: NodeKind,
    end: usize,
}

#[derive(Clone, Debug)]
struct Property {
    key: String,
    key_token: Token,
    value: Node,
}

#[derive(Clone, Debug)]
pub(crate) struct DependencyEntry {
    pub(crate) section: String,
    pub(crate) name: String,
    key_token: Token,
    pub(crate) value_end: usize,
}

pub(crate) fn dependency_entries(text: &str) -> Vec<DependencyEntry> {
    let tokens = tokenize(text);
    let Some((root, _)) = parse_value(&tokens, 0) else {
        return Vec::new();
    };
    let NodeKind::Object(sections) = root.kind else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for section in sections {
        if !is_dependency_section(&section.key) {
            continue;
        }
        let NodeKind::Object(dependencies) = section.value.kind else {
            continue;
        };

        for dependency in dependencies {
            if is_composer_package(&dependency.key) {
                entries.push(DependencyEntry {
                    section: section.key.clone(),
                    name: dependency.key,
                    key_token: dependency.key_token,
                    value_end: dependency.value.end,
                });
            }
        }
    }
    entries
}

fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut scanner = Scanner::new(
        text,
        &ScannerOptions {
            allow_single_quoted_strings: false,
            allow_hexadecimal_numbers: false,
            allow_unary_plus_numbers: false,
        },
    );

    while let Ok(Some(token)) = scanner.scan() {
        let start = scanner.token_start();
        let end = scanner.token_end();
        let (kind, value) = match token {
            JsonToken::OpenBrace => (TokenKind::ObjectStart, String::new()),
            JsonToken::CloseBrace => (TokenKind::ObjectEnd, String::new()),
            JsonToken::OpenBracket => (TokenKind::ArrayStart, String::new()),
            JsonToken::CloseBracket => (TokenKind::ArrayEnd, String::new()),
            JsonToken::Colon => (TokenKind::Colon, String::new()),
            JsonToken::Comma => (TokenKind::Comma, String::new()),
            JsonToken::String(value) => (TokenKind::String, value.into_owned()),
            JsonToken::Word(value) | JsonToken::Number(value) => {
                (TokenKind::Literal, value.to_owned())
            }
            JsonToken::Boolean(value) => (TokenKind::Literal, value.to_string()),
            JsonToken::Null => (TokenKind::Literal, "null".to_owned()),
            JsonToken::CommentLine(_) | JsonToken::CommentBlock(_) => continue,
        };
        tokens.push(Token {
            kind,
            start,
            end,
            value,
        });
    }
    tokens
}

fn parse_value(tokens: &[Token], index: usize) -> Option<(Node, usize)> {
    let token = tokens.get(index)?;
    match token.kind {
        TokenKind::ObjectStart => {
            let mut properties = Vec::new();
            let mut cursor = index + 1;
            let mut end = token.end;

            while let Some(current) = tokens.get(cursor) {
                match current.kind {
                    TokenKind::ObjectEnd => {
                        end = current.end;
                        cursor += 1;
                        break;
                    }
                    TokenKind::Comma => {
                        cursor += 1;
                        continue;
                    }
                    TokenKind::String => {}
                    _ => {
                        cursor += 1;
                        continue;
                    }
                }

                let key_token = current.clone();
                cursor += 1;
                if tokens
                    .get(cursor)
                    .is_some_and(|token| token.kind == TokenKind::Colon)
                {
                    cursor += 1;
                }
                let Some((value, next)) = parse_value(tokens, cursor) else {
                    cursor += 1;
                    continue;
                };
                properties.push(Property {
                    key: key_token.value.clone(),
                    key_token,
                    value,
                });
                cursor = next;
                if tokens
                    .get(cursor)
                    .is_some_and(|token| token.kind == TokenKind::Comma)
                {
                    cursor += 1;
                }
            }

            Some((
                Node {
                    kind: NodeKind::Object(properties),
                    end,
                },
                cursor,
            ))
        }
        TokenKind::ArrayStart => {
            let mut cursor = index + 1;
            let mut end = token.end;
            while let Some(current) = tokens.get(cursor) {
                if current.kind == TokenKind::ArrayEnd {
                    end = current.end;
                    cursor += 1;
                    break;
                }
                if current.kind == TokenKind::Comma {
                    cursor += 1;
                    continue;
                }
                if let Some((_, next)) = parse_value(tokens, cursor) {
                    cursor = next;
                } else {
                    cursor += 1;
                }
            }
            Some((
                Node {
                    kind: NodeKind::Array,
                    end,
                },
                cursor,
            ))
        }
        _ => Some((
            Node {
                kind: NodeKind::Scalar,
                end: token.end,
            },
            index + 1,
        )),
    }
}

fn is_dependency_section(section: &str) -> bool {
    matches!(
        section,
        "require" | "require-dev" | "conflict" | "replace" | "provide" | "suggest"
    )
}

pub(crate) fn is_update_section(section: &str) -> bool {
    matches!(section, "require" | "require-dev")
}

pub(crate) fn is_composer_package(name: &str) -> bool {
    let mut parts = name.split('/');
    let Some(vendor) = parts.next() else {
        return false;
    };
    let Some(package) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && !vendor.is_empty()
        && !package.is_empty()
        && vendor.chars().all(is_package_character)
        && package.chars().all(is_package_character)
}

fn is_package_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
}

pub(crate) fn package_range(text: &str, dependency: &DependencyEntry) -> Range {
    Range {
        start: offset_to_position(text, dependency.key_token.start + 1),
        end: offset_to_position(text, dependency.key_token.end.saturating_sub(1)),
    }
}

pub(crate) fn offset_to_position(text: &str, offset: usize) -> Position {
    let mut line = 0;
    let mut character = 0;
    for current in text[..offset.min(text.len())].chars() {
        if current == '\n' {
            line += 1;
            character = 0;
        } else {
            character += current.len_utf16() as u32;
        }
    }
    Position { line, character }
}

pub(crate) fn position_in_range(position: Position, range: Range) -> bool {
    position >= range.start && position <= range.end
}

pub(crate) fn composer_path_from_uri(uri: &str) -> Option<PathBuf> {
    let url = Url::parse(uri).ok()?;
    let path = url.to_file_path().ok()?;
    path.file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("composer.json"))
        .then_some(path)
}

pub(crate) fn installed_versions(uri: &str, document_text: &str) -> HashMap<String, String> {
    let Some(composer_path) = composer_path_from_uri(uri) else {
        return HashMap::new();
    };
    let project_directory = composer_path.parent().unwrap_or_else(|| Path::new("."));
    let vendor_directory =
        configured_vendor_directory(document_text).unwrap_or_else(|| "vendor".to_owned());
    let installed_path = project_directory
        .join(vendor_directory)
        .join("composer")
        .join("installed.json");

    let Ok(metadata) = fs::metadata(&installed_path) else {
        return HashMap::new();
    };
    if metadata.len() > MAX_INSTALLED_METADATA_BYTES {
        return HashMap::new();
    }
    let Ok(contents) = fs::read_to_string(installed_path) else {
        return HashMap::new();
    };
    installed_versions_from_json(&contents)
}

fn configured_vendor_directory(text: &str) -> Option<String> {
    let composer: Value = serde_json::from_str(text).ok()?;
    composer
        .get("config")?
        .get("vendor-dir")?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn installed_versions_from_json(contents: &str) -> HashMap<String, String> {
    let Ok(installed) = serde_json::from_str::<Value>(contents) else {
        return HashMap::new();
    };
    let packages = installed
        .as_array()
        .or_else(|| installed.get("packages").and_then(Value::as_array));
    let Some(packages) = packages else {
        return HashMap::new();
    };

    packages
        .iter()
        .filter_map(|package| {
            let name = package.get("name")?.as_str()?;
            let version = package
                .get("pretty_version")
                .and_then(Value::as_str)
                .or_else(|| package.get("version").and_then(Value::as_str))?;
            Some((name.to_ascii_lowercase(), version.to_owned()))
        })
        .collect()
}

pub(crate) fn version_label(version: &str) -> String {
    let version = version.trim();
    if version.starts_with(|character: char| character.is_ascii_digit()) {
        format!("v{version}")
    } else {
        version.to_owned()
    }
}

pub(crate) fn compare_versions(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    Some(stable_version_parts(left)?.cmp(&stable_version_parts(right)?))
}

fn stable_version_parts(version: &str) -> Option<[u64; 5]> {
    let normalized = version.trim().strip_prefix('v').unwrap_or(version.trim());
    let lower = normalized.to_ascii_lowercase();
    if ["dev", "alpha", "beta", "rc"].iter().any(|marker| {
        lower == *marker
            || lower.starts_with(&format!("{marker}-"))
            || lower.contains(&format!(".{marker}"))
            || lower.contains(&format!("-{marker}"))
            || lower.contains(&format!("_{marker}"))
    }) {
        return None;
    }

    let numeric_end = normalized
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(normalized.len());
    let numeric = &normalized[..numeric_end];
    if numeric.is_empty() {
        return None;
    }
    let mut parts = [0_u64; 5];
    for (index, part) in numeric.split('.').take(4).enumerate() {
        if part.is_empty() {
            return None;
        }
        parts[index] = part.parse().ok()?;
    }

    let suffix = &normalized[numeric_end..].to_ascii_lowercase();
    for prefix in ["-patch", ".patch", "-p", ".p"] {
        if let Some(patch) = suffix.strip_prefix(prefix) {
            parts[4] = patch.parse().ok()?;
            break;
        }
    }
    Some(parts)
}

pub(crate) fn newest_stable_version(metadata: &Value, package_name: &str) -> Option<String> {
    let packages = metadata.get("packages")?.as_object()?;
    let versions = packages
        .get(package_name)
        .or_else(|| packages.values().next())?
        .as_array()?;

    versions
        .iter()
        .filter_map(|release| {
            let version = release.get("version")?.as_str()?;
            let comparable = release
                .get("version_normalized")
                .and_then(Value::as_str)
                .unwrap_or(version);
            let parts = stable_version_parts(comparable)?;
            Some((parts, version.to_owned()))
        })
        .max_by_key(|(parts, _)| *parts)
        .map(|(_, version)| version)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;
    use url::Url;

    use super::*;

    const COMPOSER_JSON: &str = r#"{
  "name": "example/project",
  "require": {
    "php": "^8.3",
    "ext-json": "*",
    "laravel/framework": "^12.0",
    "psr/log": "^3.0"
  },
  "require-dev": {
    "phpunit/phpunit": "^12.0"
  },
  "replace": {
    "legacy/package": "self.version"
  }
}"#;

    #[test]
    fn finds_composer_dependencies_and_source_ranges() {
        let entries = dependency_entries(COMPOSER_JSON);
        let names: Vec<_> = entries
            .iter()
            .map(|entry| (entry.section.as_str(), entry.name.as_str()))
            .collect();
        assert_eq!(
            names,
            [
                ("require", "laravel/framework"),
                ("require", "psr/log"),
                ("require-dev", "phpunit/phpunit"),
                ("replace", "legacy/package"),
            ]
        );
        assert_eq!(
            package_range(COMPOSER_JSON, &entries[0]),
            Range::new(Position::new(5, 5), Position::new(5, 22))
        );
    }

    #[test]
    fn keeps_entries_available_while_the_document_is_incomplete() {
        let text = r#"{"require":{"psr/log":"^3.0""#;
        let entries = dependency_entries(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "psr/log");
    }

    #[test]
    fn uses_utf16_lsp_positions() {
        assert_eq!(offset_to_position("😀x", 4), Position::new(0, 2));
        assert_eq!(offset_to_position("😀\nx", 6), Position::new(1, 1));
    }

    #[test]
    fn loads_composer_one_and_two_installed_metadata() {
        let directory = tempdir().expect("temporary directory");
        let composer_path = directory.path().join("composer.json");
        let installed_directory = directory.path().join("dependencies/composer");
        fs::create_dir_all(&installed_directory).expect("installed directory");
        fs::write(
            installed_directory.join("installed.json"),
            r#"[{"name":"psr/log","version":"3.0.2"}]"#,
        )
        .expect("installed metadata");
        let text = r#"{"config":{"vendor-dir":"dependencies"},"require":{"psr/log":"^3"}}"#;
        let uri = Url::from_file_path(composer_path)
            .expect("file URL")
            .to_string();
        assert_eq!(
            installed_versions(&uri, text).get("psr/log"),
            Some(&"3.0.2".to_owned())
        );

        fs::write(
            installed_directory.join("installed.json"),
            r#"{"packages":[{"name":"psr/log","version":"3.0.1","pretty_version":"v3.0.2"}]}"#,
        )
        .expect("installed metadata");
        assert_eq!(
            installed_versions(&uri, text).get("psr/log"),
            Some(&"v3.0.2".to_owned())
        );
    }

    #[test]
    fn malformed_or_missing_metadata_is_ignored() {
        let directory = tempdir().expect("temporary directory");
        let composer_path = directory.path().join("composer.json");
        let uri = Url::from_file_path(composer_path)
            .expect("file URL")
            .to_string();
        assert!(installed_versions(&uri, "{}").is_empty());

        let installed_directory = directory.path().join("vendor/composer");
        fs::create_dir_all(&installed_directory).expect("installed directory");
        fs::write(installed_directory.join("installed.json"), "not json")
            .expect("installed metadata");
        assert!(installed_versions(&uri, "{}").is_empty());
    }

    #[test]
    fn formats_and_compares_stable_versions() {
        assert_eq!(version_label("3.2.1"), "v3.2.1");
        assert_eq!(version_label("v3.2.1"), "v3.2.1");
        assert_eq!(version_label("dev-main"), "dev-main");
        assert_eq!(
            compare_versions("v2.0.0", "1.9.9"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_versions("1.2.3", "v1.2.3"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(compare_versions("v2.0.0-RC1", "1.9.9"), None);
    }

    #[test]
    fn chooses_the_newest_stable_packagist_release() {
        let metadata = json!({
            "packages": {
                "vendor/package": [
                    {"version": "v3.0.0-RC1", "version_normalized": "3.0.0.0-RC1"},
                    {"version": "v2.4.0", "version_normalized": "2.4.0.0"},
                    {"version": "v2.3.1", "version_normalized": "2.3.1.0"}
                ]
            }
        });
        assert_eq!(
            newest_stable_version(&metadata, "vendor/package"),
            Some("v2.4.0".to_owned())
        );
    }
}
