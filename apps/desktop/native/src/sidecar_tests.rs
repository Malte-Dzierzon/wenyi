//! Sidecar lifecycle tests: spawn a fake Python engine, parse `WENYI_READY`,
//! verify token auth plumbing, shut down cleanly.
//!
//! The fake speaks the same protocol as `wenyi_desktop.desktop`:
//! one `WENYI_READY {protocol,port,pid}` line on stdout, then stdin
//! `{"command":"shutdown"}` to exit. No real Python needed.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::{LazyLock, Mutex};

use crate::backend::Backend;

/// Env vars are process-global: serialize tests that set them.
static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));
static TEST_COUNTER: LazyLock<std::sync::atomic::AtomicU64> =
    LazyLock::new(|| std::sync::atomic::AtomicU64::new(0));

fn fake_sidecar_script(port: u16, extra_lines: &[&str]) -> std::path::PathBuf {
    let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "wenyi-fake-sidecar-{}-{n}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("wenyi_desktop.py");
    let mut body = String::from("#!/usr/bin/env python3\nimport json, os, sys\n");
    for line in extra_lines {
        body.push_str(&format!("print({:?}, flush=True)\n", line));
    }
    body.push_str(&format!(
        "print('WENYI_READY ' + json.dumps({{'protocol': 1, 'port': {port}, 'pid': os.getpid()}}), flush=True)\n"
    ));
    body.push_str("assert len(os.environ.get('WENYI_API_TOKEN', '')) == 64, 'token must be 64 hex'\n");
    body.push_str("sys.stdin.readline()\n");
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn with_fake_python(path: &std::path::Path, f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap();
    // `spawn_command` runs `$WENYI_DESKTOP_PYTHON -m wenyi_desktop`; use the
    // system Python with PYTHONPATH pointed at our fake module.
    let orig_python = std::env::var_os("WENYI_DESKTOP_PYTHON");
    let orig_path = std::env::var_os("PYTHONPATH");
    std::env::set_var("WENYI_DESKTOP_PYTHON", "/usr/bin/python3");
    let mut paths = vec![path.parent().unwrap().to_path_buf()];
    if let Some(prev) = &orig_path {
        paths.push(std::path::PathBuf::from(prev));
    }
    std::env::set_var("PYTHONPATH", std::env::join_paths(paths).unwrap());
    f();
    match orig_python {
        Some(v) => std::env::set_var("WENYI_DESKTOP_PYTHON", v),
        None => std::env::remove_var("WENYI_DESKTOP_PYTHON"),
    }
    match orig_path {
        Some(v) => std::env::set_var("PYTHONPATH", v),
        None => std::env::remove_var("PYTHONPATH"),
    }
}

#[test]
fn spawn_parses_ready_and_shuts_down() {
    let script = fake_sidecar_script(18441, &[]);
    with_fake_python(&script, || {
        let mut backend =
            Backend::spawn(None).expect("spawn must parse WENYI_READY from fake sidecar");
        assert_eq!(backend.connection.api_base, "http://127.0.0.1:18441");
        assert_eq!(backend.connection.token.len(), 64);
        assert!(
            backend.connection.token.chars().all(|c| c.is_ascii_hexdigit()),
            "token must be hex"
        );
        backend.shutdown();
        assert!(backend.exited(), "shutdown command must stop the sidecar");
    });
}

#[test]
fn spawn_skips_warning_lines_before_ready() {
    let script = fake_sidecar_script(18442, &["some warning", "another log line"]);
    with_fake_python(&script, || {
        let mut backend = Backend::spawn(None).expect("warnings must not break ready parse");
        assert_eq!(backend.connection.api_base, "http://127.0.0.1:18442");
        backend.shutdown();
        assert!(backend.exited());
    });
}

#[test]
fn spawn_rejects_wrong_pid() {
    let n = TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("wenyi-fake-sidecar-pid-{}-{n}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("wenyi_desktop.py");
    let body = "#!/usr/bin/env python3\nimport json, os\nprint('WENYI_READY ' + json.dumps({'protocol': 1, 'port': 18443, 'pid': 1}), flush=True)\ninput()\n";
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    with_fake_python(&path, || {
        match Backend::spawn(None) {
            Ok(_) => panic!("PID mismatch must fail"),
            Err(err) => assert_eq!(err, "Invalid ready identity"),
        }
    });
}

/// Live roundtrip against the real Python sidecar: spawn, health check,
/// create project from bytes, list chapters, delete. Skipped when the
/// workspace Python or `wenyi_desktop` is unavailable.
#[test]
fn live_roundtrip_against_real_sidecar() {
    let _guard = ENV_LOCK.lock().unwrap();
    let workspace_root = std::env::var("WENYI_WORKSPACE_ROOT").ok().map_or_else(
        || {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../..")
                .canonicalize()
                .unwrap()
        },
        std::path::PathBuf::from,
    );
    let venv_python = workspace_root.join(".venv/bin/python");
    let backend_dir = workspace_root.join("apps/desktop/backend");
    if !venv_python.exists() || !backend_dir.join("wenyi_desktop").exists() {
        eprintln!("skipping live roundtrip: no workspace venv/sidecar");
        return;
    }
    let orig_python = std::env::var_os("WENYI_DESKTOP_PYTHON");
    let orig_path = std::env::var_os("PYTHONPATH");
    std::env::set_var("WENYI_DESKTOP_PYTHON", &venv_python);
    let desktop_pkg = workspace_root.join("apps/desktop/backend");
    let core_pkg = workspace_root.join("packages/core");
    let backend_pkg = workspace_root.join("packages/backend");
    let mut paths = vec![desktop_pkg, core_pkg, backend_pkg];
    if let Some(prev) = &orig_path {
        paths.push(std::path::PathBuf::from(prev));
    }
    std::env::set_var("PYTHONPATH", std::env::join_paths(paths).unwrap());
    let result = (|| -> Result<(), String> {
        let data_dir = std::env::temp_dir().join(format!(
            "wenyi-live-{}-{}",
            std::process::id(),
            TEST_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let mut backend =
            Backend::spawn(Some(&data_dir)).map_err(|e| format!("spawn: {e}"))?;
        let client = crate::client::Client::new(
            backend.connection.api_base.clone(),
            backend.connection.token.clone(),
        );
        let health = crate::client::block_on(client.health()).map_err(|e| format!("health: {e}"))?;
        assert_eq!(health.get("status").and_then(|v| v.as_str()), Some("ok"));
        let body = crate::client::ProjectCreate {
            name: "live-roundtrip".into(),
            source_lang: "en".into(),
            target_lang: "zh".into(),
            strategy: serde_json::Value::Object(Default::default()),
            prepare: false,
            pdf_backend: None,
            translation_mode: "standard".into(),
        };
        let detail = crate::client::block_on(client.create_project(
            &body,
            "hello.txt",
            b"Hello world.\nSecond paragraph.\n".to_vec(),
        ))
        .map_err(|e| format!("create: {e}"))?;
        let pid = detail.project.id.clone();
        assert!(!pid.is_empty());
        let projects =
            crate::client::block_on(client.list_projects()).map_err(|e| format!("list: {e}"))?;
        assert!(projects.iter().any(|p| p.id == pid));
        crate::client::block_on(client.delete_project(&pid))
            .map_err(|e| format!("delete: {e}"))?;
        backend.shutdown();
        if !backend.exited() {
            return Err("sidecar did not exit after shutdown".into());
        }
        let _ = std::fs::remove_dir_all(&data_dir);
        Ok(())
    })();
    match orig_python {
        Some(v) => std::env::set_var("WENYI_DESKTOP_PYTHON", v),
        None => std::env::remove_var("WENYI_DESKTOP_PYTHON"),
    }
    match orig_path {
        Some(v) => std::env::set_var("PYTHONPATH", v),
        None => std::env::remove_var("PYTHONPATH"),
    }
    result.unwrap();
}
