// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::Parser;
use voxix_app_lib::{CliArgs, Command};

fn main() {
    let cli_args = CliArgs::parse();

    // `serve` is dispatched before anything Tauri/GTK: the headless daemon
    // initializes no windows, webviews, tray, overlay, mic, shortcuts, or
    // paste machinery (SDK plan §3.1).
    if let Some(Command::Serve {
        bind,
        data_dir,
        token_stdout,
        max_audio_seconds,
        embed,
        opus,
        check_process_policy,
    }) = cli_args.command.clone()
    {
        if check_process_policy {
            // Hidden gate: automated no-background-throttling assertion (§15
            // G8/G22); unit shape `Type=notify` is documented for M5.
            match voxix_app_lib::service::notify::check_self_policy() {
                Ok(report) => {
                    println!("{}", serde_json::json!(report));
                    std::process::exit(voxix_app_lib::service::EXIT_OK);
                }
                Err(message) => {
                    eprintln!("error: {message}");
                    std::process::exit(voxix_app_lib::service::EXIT_POLICY);
                }
            }
        }
        let options = voxix_app_lib::service::config::ServeOptions {
            bind,
            data_dir,
            token_stdout,
            max_audio_seconds,
            embed,
            opus,
        };
        let code = voxix_app_lib::service::run_serve_cli(options);
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let _ = std::io::stderr().flush();
        std::process::exit(code);
    }

    #[cfg(target_os = "linux")]
    {
        // DMABUF renderer causes crashes on various GPU/display server configurations
        // See: https://github.com/tauri-apps/tauri/issues/9394
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        // Disable WebKitGTK's own compositing so transparent windows use the
        // compositor's RGBA pipeline instead of WebKit's internal buffer.
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        // Force X11 backend for the overlay webview: WebKitGTK does not render
        // content inside GTK layer shell windows on Wayland. X11/Xwayland
        // renders correctly and X11 properties handle all-desktops,
        // always-on-top, and no-focus.
        let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
        if session_type == "wayland" || session_type == "tty" {
            std::env::set_var("GDK_BACKEND", "x11");
        }
    }

    voxix_app_lib::run(cli_args)
}
