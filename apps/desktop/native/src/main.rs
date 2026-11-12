//! Native GPUI desktop UI for Wenyi.
//!
//! In-process frontend over `wenyi_core::service` — no sidecar, no HTTP.

mod nav;
mod service;
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
        println!("Usage: wenyi-native [--data-dir <workspace>]");
        println!("  --data-dir  isolated desktop workspace (default: platform data dir)");
        return;
    }
    if let Some(dir) = args
        .windows(2)
        .find(|w| w[0] == "--data-dir")
        .map(|w| w[1].clone())
    {
        std::env::set_var("WENYI_NATIVE_DATA_DIR", dir);
    }

    application().with_assets(assets::AllAssets).run(|cx| {
        init(cx);
        open_window(WindowOptions::default(), cx, |window, cx| {
            cx.new(|cx| views::App::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
