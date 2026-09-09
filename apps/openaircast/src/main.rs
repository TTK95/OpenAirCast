//! OpenAirCast — stream Windows system audio to AirPlay 2 receiver groups.
//!
//! Run with no arguments to launch the Control Center window and tray. Run
//! `--list` to print discovered AirPlay devices and exit.

// Use the Windows subsystem (no console window) for the normal app, but keep
// a console when built for debugging via `--list`.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[allow(dead_code)] // frozen contracts + subproject-2 seams stay ahead of use.
mod app;
mod app_handle;
// The one seam onto the resilience backend: command port, snapshot
// projection, bridge thread, and the seam `run()` starts the app on.
mod backend_bridge;
mod cast;
// The legacy device stage. `run()` no longer starts it -- `backend_device_seam`
// does -- but `legacy_device_seam` keeps it wired and compiling as the way
// back, and `effect_to_command` is still the shell's effect translation.
#[allow(dead_code)]
mod device_service;
mod platform;
#[allow(dead_code)] // worker API surface completed by subproject 2 diagnostics.
mod preferences;
mod setup_diagnostic;
#[allow(dead_code)] // snapshot adapter + legacy shims removed gradually.
mod tray;
#[allow(dead_code)] // view layer is rooted only through the erased eframe vtable.
mod ui;

use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let diagnostic_position = args.iter().position(|arg| arg == "--diagnose-group");
    let diagnostic_mode = diagnostic_position.is_some();
    let log_filter = if diagnostic_mode {
        "off,airplay_timing::ptp=debug,airplay_client::connection=info".to_string()
    } else {
        std::env::var("RUST_LOG").unwrap_or_else(|_| "warn,homepod_cast=info".into())
    };
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(log_filter))
        .init();

    if let Some(position) = diagnostic_position {
        if position != 1 {
            eprintln!("Usage: openaircast --diagnose-group <exact-name-1> <exact-name-2>");
            return Err(anyhow::anyhow!("--diagnose-group must be argument 1"));
        }
        let diagnostic_args = args.iter().skip(2).cloned().collect::<Vec<_>>();
        setup_diagnostic::requested_names(diagnostic_args.clone()).map_err(|message| {
            eprintln!("Usage: openaircast --diagnose-group <exact-name-1> <exact-name-2>");
            anyhow::anyhow!(message)
        })?;

        let runtime = tokio::runtime::Runtime::new()?;
        let result = runtime.block_on(setup_diagnostic::run(diagnostic_args));
        runtime.shutdown_background();
        return result;
    }

    // Diagnostic: exercise the single-receiver or PTP group path with logs.
    let group_selftest = args.iter().any(|a| a == "--selftest-group");
    if group_selftest || args.iter().any(|a| a == "--selftest") {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async {
            let mut devices = match cast::discover(Duration::from_secs(3)).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("discover failed: {e:#}");
                    return;
                }
            };
            if devices.is_empty() {
                tracing::error!("no device found");
                return;
            }
            if group_selftest && devices.len() < 2 {
                tracing::error!("group selftest requires at least two discovered receivers");
                return;
            }
            if group_selftest {
                devices.truncate(8);
            } else {
                devices.truncate(1);
            }
            let targets = devices
                .iter()
                .map(|device| format!("{} ({})", device.name, device.model))
                .collect::<Vec<_>>()
                .join(", ");
            tracing::info!("selftest targets: {targets}");
            tracing::info!("=== starting session ===");
            match cast::Session::start(devices, cast::DEFAULT_VOLUME).await {
                Ok(mut s) => {
                    let secs = 50u32;
                    tracing::info!("streaming for {secs}s with 2s keepalive (play audio now)");
                    let mut elapsed = 0;
                    while elapsed < secs {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        s.feedback().await;
                        elapsed += 2;
                    }
                    s.stop().await;
                    tracing::info!("stopped");
                }
                Err(e) => tracing::error!("start failed: {e:#}"),
            }
            tracing::info!("selftest complete");
        });
        // The session was stopped and awaited above, so nothing this route
        // cares about is still running. What can remain is a `spawn_blocking`
        // task the AirPlay streamer abandons when its sender thread outlives
        // the join budget, and a blocking task cannot be aborted -- so a plain
        // runtime drop would block here forever. Handing the runtime to the
        // background releases it without waiting and without a deadline; the
        // leftover thread is reaped when this process exits normally, which is
        // what returning from `main` does.
        rt.shutdown_background();
        return Ok(());
    }

    if args.iter().any(|a| a == "--list") {
        let rt = tokio::runtime::Runtime::new()?;
        let devices = rt.block_on(cast::discover(Duration::from_secs(3)))?;
        if devices.is_empty() {
            println!("No AirPlay devices found.");
        } else {
            println!("AirPlay devices:");
            for d in &devices {
                let ipv4 = d.addresses.iter().find(|a| a.is_ipv4());
                println!(
                    "  {:<24} {:<18} {}",
                    d.name,
                    d.model,
                    ipv4.map(|a| a.to_string()).unwrap_or_default()
                );
            }
        }
        return Ok(());
    }

    app::run()
}
