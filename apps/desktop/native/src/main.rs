//! Native GPUI desktop UI for Wenyi.
//!
//! Parallel implementation alongside the Tauri/React app. Talks to the same
//! Python sidecar over the same loopback REST+WS contract — no new backend
//! protocol. See `docs-next/18-desktop-parity.md`.

mod backend;
mod client;
mod nav;
mod theme;
mod views;

use gpui_kit::*;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("wenyi_native=info".parse().unwrap()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("Wenyi Native {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Usage: wenyi-native [--data-dir <workspace>] [--backend <apiBase> --token <token>]"
        );
        println!("  --data-dir  isolated desktop workspace (default: platform data dir)");
        println!("  --backend   connect to an already-running sidecar instead of spawning");
        return;
    }
    // Optional attach mode: --backend <apiBase> --token <token>.
    // Communicated to the App view via env (parsed once, before the UI starts).
    if let (Some(api_base), Some(token)) = (
        args.windows(2)
            .find(|w| w[0] == "--backend")
            .map(|w| w[1].clone()),
        args.windows(2)
            .find(|w| w[0] == "--token")
            .map(|w| w[1].clone()),
    ) {
        std::env::set_var("WENYI_NATIVE_BACKEND", &api_base);
        std::env::set_var("WENYI_NATIVE_TOKEN", &token);
    }
    if let Some(dir) = args
        .windows(2)
        .find(|w| w[0] == "--data-dir")
        .map(|w| w[1].clone())
    {
        std::env::set_var("WENYI_NATIVE_DATA_DIR", dir);
    }

    application()
        .with_assets(assets::AllAssets)
        .run(|cx| {
            init(cx);
            open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| views::App::new(window, cx))
            })
            .expect("Failed to open window");
        });
}
