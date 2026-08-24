use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
};

use serde_json::{json, Value};
use tempfile::tempdir;
use url::Url;

const COMPOSER_JSON: &str = r#"{
  "require": {
    "laravel/framework": "^12.0",
    "psr/log": "^3.0"
  }
}"#;

fn send(writer: &mut impl Write, message: &Value) {
    let body = serde_json::to_vec(message).expect("serialize LSP message");
    write!(writer, "Content-Length: {}\r\n\r\n", body.len()).expect("write LSP header");
    writer.write_all(&body).expect("write LSP body");
    writer.flush().expect("flush LSP message");
}

fn receive(reader: &mut impl BufRead) -> Value {
    let mut content_length = None;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("read LSP header");
        assert!(!header.is_empty(), "language server closed stdout");
        if header == "\r\n" {
            break;
        }
        if let Some(value) = header
            .strip_prefix("Content-Length:")
            .and_then(|value| value.trim().parse::<usize>().ok())
        {
            content_length = Some(value);
        }
    }

    let mut body = vec![0; content_length.expect("Content-Length header")];
    reader.read_exact(&mut body).expect("read LSP body");
    serde_json::from_slice(&body).expect("parse LSP response")
}

#[test]
fn serves_document_links_over_stdio() {
    let cache_directory = tempdir().expect("temporary cache directory");
    let mut child = Command::new(env!("CARGO_BIN_EXE_composer-language-server"))
        .env("COMPOSER_LANGUAGE_SERVER_CACHE_DIR", cache_directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start language server");
    let mut stdin = child.stdin.take().expect("language-server stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("language-server stdout"));

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "capabilities": {},
                "initializationOptions": {"check_updates": false}
            }
        }),
    );
    let initialized = receive(&mut stdout);
    assert_eq!(initialized["id"], 1);
    assert_eq!(
        initialized["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        initialized["result"]["capabilities"]["inlayHintProvider"],
        true
    );

    let directory = tempdir().expect("temporary directory");
    let composer_path = directory.path().join("composer.json");
    let installed_directory = directory.path().join("vendor/composer");
    fs::create_dir_all(&installed_directory).expect("installed directory");
    fs::write(
        installed_directory.join("installed.json"),
        r#"{"packages":[{"name":"psr/log","version":"3.0.2"}]}"#,
    )
    .expect("installed metadata");
    let uri = Url::from_file_path(composer_path)
        .expect("composer.json URL")
        .to_string();
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {
                "textDocument": {
                    "uri": uri,
                    "languageId": "json",
                    "version": 1,
                    "text": COMPOSER_JSON
                }
            }
        }),
    );
    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "textDocument/documentLink",
            "params": {"textDocument": {"uri": uri}}
        }),
    );
    let links = receive(&mut stdout);
    assert_eq!(links["result"].as_array().map(Vec::len), Some(2));
    assert_eq!(
        links["result"][1]["target"],
        "https://packagist.org/packages/psr/log"
    );

    send(
        &mut stdin,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "textDocument/inlayHint",
            "params": {
                "textDocument": {"uri": uri},
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 100, "character": 0}
                }
            }
        }),
    );
    let hints = receive(&mut stdout);
    assert_eq!(hints["result"].as_array().map(Vec::len), Some(1));
    assert_eq!(hints["result"][0]["label"], "v3.0.2");
    assert_eq!(hints["result"][0]["paddingLeft"], true);

    send(
        &mut stdin,
        &json!({"jsonrpc": "2.0", "id": 4, "method": "shutdown"}),
    );
    assert_eq!(receive(&mut stdout)["id"], 4);
    send(&mut stdin, &json!({"jsonrpc": "2.0", "method": "exit"}));
    drop(stdin);

    let status = child.wait().expect("wait for language server");
    assert!(status.success(), "language server exited with {status}");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("language-server stderr")
        .read_to_string(&mut stderr)
        .expect("read language-server stderr");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}
