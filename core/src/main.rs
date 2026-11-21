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
        // Escape hatch for diagnosing renderer/backend issues on unusual
        // setups: VOXIX_WEBKIT_TWEAKS=off leaves every WebKit/GDK variable
        // untouched (the caller's environment, if any, wins).
        let tweaks_off = std::env::var("VOXIX_WEBKIT_TWEAKS")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(false);
        if !tweaks_off {
            let flag = |name: &str| {
                std::env::var(name)
                    .map(|v| {
                        matches!(
                            v.trim().to_ascii_lowercase().as_str(),
                            "1" | "true" | "yes" | "on"
                        )
                    })
                    .unwrap_or(false)
            };

            // WebKitGTK's compositing + DMABUF pipeline is what actually paints
            // RGBA/transparent webviews (e.g. the recording overlay). Disabling
            // either produced a blank overlay on X11/XWayland (Hyprland), so
            // both are now opt-out via env for setups that hit driver bugs
            // (see https://github.com/tauri-apps/tauri/issues/9394).
            if flag("VOXIX_WEBKIT_NO_DMABUF") {
                std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
            if flag("VOXIX_WEBKIT_NO_COMPOSITING") {
                std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
            }

            // Force X11 backend for the overlay webview: WebKitGTK does not render
            // content inside GTK layer shell windows on Wayland. X11/Xwayland
            // renders correctly and X11 properties handle all-desktops,
            // always-on-top, and no-focus.
            let session_type = std::env::var("XDG_SESSION_TYPE").unwrap_or_default();
            if (session_type == "wayland" || session_type == "tty")
                && std::env::var_os("GDK_BACKEND").is_none()
            {
                std::env::set_var("GDK_BACKEND", "x11");
            }
        }
    }

    voxix_app_lib::run(cli_args)
}
