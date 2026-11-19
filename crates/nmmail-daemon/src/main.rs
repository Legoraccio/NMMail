//! `nmmaild` — NM Mail background daemon.
//!
//! Modes:
//! * `nmmaild` / `nmmaild --background` — run the daemon (single instance).
//!   Without `--background` the UI is opened too (manual launch).
//! * `nmmaild --activate <nmmail:uri>` — forward a notification click to the
//!   running daemon (starting it if needed) and exit.
//! * `nmmaild --show` — ask the running daemon to show the UI.
//! * `nmmaild --test-notification` — show a sample toast and print the OS status.
//! * `nmmaild --register` / `--unregister` — per-user OS integration.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod client;
mod daemon;
mod i18n;
mod logging;
mod platform;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a.eq_ignore_ascii_case(f));
    if flag("--version") {
        println!("nmmaild {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if let Some(pos) = args
        .iter()
        .position(|a| a.eq_ignore_ascii_case("--activate"))
    {
        let uri = args.get(pos + 1).cloned().unwrap_or_default();
        return client::activate(&uri);
    }
    if flag("--test-notification") {
        println!("{}", platform::test_notification_cli());
        return ExitCode::SUCCESS;
    }
    if flag("--show") {
        return client::show();
    }
    if flag("--register") {
        return match platform::register(true) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
                ExitCode::FAILURE
            }
        };
    }
    if flag("--unregister") {
        return match platform::register(false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{e:#}");
                ExitCode::FAILURE
            }
        };
    }
    daemon::run(flag("--background"))
}
