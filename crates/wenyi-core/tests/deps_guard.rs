//! Dependency-direction guard: wenyi-core must stay UI/server-free.
//!
//! Fails if the dependency tree ever contains GPUI, Tauri, HTTP server
//! frameworks, native TLS (openssl), or the desktop app layer.
//!
//! `hyper`/`hyper-util` are allowed: they are reqwest's HTTP *client*
//! transport (Slice 3 LLM providers), not a server framework. The ban
//! covers server frameworks (axum/actix/rocket/warp) and native TLS.

use std::process::Command;

const FORBIDDEN: &[&str] = &[
    "gpui",
    "tauri",
    "axum",
    "actix-web",
    "rocket",
    "warp",
    "openssl-sys",
    "native-tls",
    "wenyi-native",
    "wenyi-desktop",
];

#[test]
fn core_tree_has_no_ui_server_or_tls_deps() {
    let output = Command::new("cargo")
        .args([
            "tree",
            "-p",
            "wenyi-core",
            "--offline",
            "--prefix",
            "none",
            "--no-default-features",
        ])
        .output()
        .expect("cargo tree must run");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8_lossy(&output.stdout).to_lowercase();
    let mut violations = Vec::new();
    for forbidden in FORBIDDEN {
        for line in tree.lines() {
            let name = line.split_whitespace().next().unwrap_or("");
            if name == *forbidden || name.starts_with(&format!("{forbidden}-")) {
                violations.push(line.to_string());
            }
        }
    }
    assert!(
        violations.is_empty(),
        "forbidden dependencies in wenyi-core tree:\n{}",
        violations.join("\n")
    );
}
