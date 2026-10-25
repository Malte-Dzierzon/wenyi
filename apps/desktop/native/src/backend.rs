//! Sidecar lifecycle: spawn the Python engine, parse `WENYI_READY`, shut down.
//!
//! Same protocol as the Tauri host (`apps/desktop/src/process.rs`):
//! spawn `wenyi-engine` (or `$WENYI_DESKTOP_PYTHON -m wenyi_desktop` in dev),
//! 64-hex token via `WENYI_API_TOKEN`, PID-verified ready line, shutdown via
//! stdin `{"command":"shutdown"}` with 30s grace then kill.

use std::{
    io::{BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// Parsed sidecar connection: base URL + bearer token.
#[derive(Debug, Clone)]
pub struct Connection {
    pub api_base: String,
    pub token: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Ready {
    protocol: u8,
    port: u16,
    pid: u32,
}

fn parse_ready(line: &str, pid: u32) -> Result<u16, &'static str> {
    let payload = line
        .strip_prefix("WENYI_READY ")
        .ok_or("Invalid ready protocol")?;
    let value: Ready = serde_json::from_str(payload).map_err(|_| "Invalid ready payload")?;
    if value.protocol != 1 || value.port == 0 || value.pid != pid {
        return Err("Invalid ready identity");
    }
    Ok(value.port)
}

fn token() -> Result<String, &'static str> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "Secure randomness unavailable")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Resolve the sidecar executable: bundled `wenyi-engine` next to us, or the
/// dev override `$WENYI_DESKTOP_PYTHON -m wenyi_desktop`.
fn spawn_command() -> Result<Command, &'static str> {
    if cfg!(debug_assertions) {
        if let Some(python) = std::env::var_os("WENYI_DESKTOP_PYTHON") {
            let mut command = Command::new(python);
            command.env_remove("__PYVENV_LAUNCHER__");
            command.args(["-m", "wenyi_desktop"]);
            return Ok(command);
        }
    }
    let exe = std::env::current_exe().map_err(|_| "Cannot locate executable")?;
    let dir = exe.parent().ok_or("Cannot locate executable dir")?;
    // Dev layout: apps/desktop/native/target/debug/ → look for sidecar nearby,
    // else fall back to `wenyi-engine` on PATH.
    for candidate in [
        dir.join("wenyi-engine"),
        dir.join("sidecar/wenyi-engine"),
        PathBuf::from("wenyi-engine"),
    ] {
        if candidate.exists() || candidate.parent() == Some(Path::new("")) {
            let mut command = Command::new(candidate);
            command.env_remove("__PYVENV_LAUNCHER__");
            return Ok(command);
        }
    }
    Err("No sidecar found (set WENYI_DESKTOP_PYTHON for dev)")
}

pub struct Backend {
    child: Child,
    pub connection: Connection,
}

impl Backend {
    pub fn spawn(workspace: Option<&Path>) -> Result<Self, &'static str> {
        let token = token()?;
        let mut command = spawn_command()?;
        if let Some(workspace) = workspace {
            command.arg("--data-dir").arg(workspace);
        }
        command
            .env("WENYI_API_TOKEN", &token)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn().map_err(|_| "Could not start local engine")?;
        let pid = child.id();
        let output = child.stdout.take().ok_or("Missing protocol pipe")?;
        let mut reader = BufReader::new(output);
        let mut line = String::new();
        let port = std::io::BufRead::read_line(
            &mut std::io::Read::take(&mut reader, 4096),
            &mut line,
        )
        .map_err(|_| "Could not read ready protocol")
        .and_then(|_| parse_ready(&line, pid))?;
        // Drain remaining stdout in the background (never log it: may hold secrets).
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut reader, &mut std::io::sink());
        });
        tracing::info!(port, "sidecar ready");
        Ok(Self {
            child,
            connection: Connection {
                api_base: format!("http://127.0.0.1:{port}"),
                token,
            },
        })
    }

    pub fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    pub fn shutdown(&mut self) {
        if let Some(mut input) = self.child.stdin.take() {
            let _ = input.write_all(b"{\"command\":\"shutdown\"}\n");
        }
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until {
            if self.exited() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_line_parses() {
        assert_eq!(
            parse_ready("WENYI_READY {\"protocol\":1,\"port\":1234,\"pid\":42}", 42).unwrap(),
            1234
        );
    }

    #[test]
    fn ready_line_rejects_bad_identity() {
        assert!(parse_ready("WENYI_READY {\"protocol\":2,\"port\":1234,\"pid\":42}", 42).is_err());
        assert!(parse_ready("WENYI_READY {\"protocol\":1,\"port\":1234,\"pid\":43}", 42).is_err());
        assert!(parse_ready("garbage", 42).is_err());
    }

    #[test]
    fn token_is_64_hex() {
        let t = token().unwrap();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
