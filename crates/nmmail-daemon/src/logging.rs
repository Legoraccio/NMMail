//! File logging (daily rotation, 7 files) without extra threads.
//!
//! `NMMAIL_LOG` accepts a target filter such as `info,nmmail_core=debug`.

use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::prelude::*;

const DEFAULT_FILTER: &str = "info,async_imap=warn,rustls=warn,lettre=warn";

pub fn init() {
    let filter: Targets = std::env::var("NMMAIL_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| DEFAULT_FILTER.parse().expect("valid default filter"));
    let dir = nmmail_core::paths::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("nmmaild")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir);
    let fmt = tracing_subscriber::fmt::layer().with_ansi(false);
    let registry = tracing_subscriber::registry().with(filter);
    match file {
        Ok(file) if cfg!(debug_assertions) => registry
            .with(fmt.with_writer(file.and(std::io::stderr)))
            .init(),
        Ok(file) => registry.with(fmt.with_writer(file)).init(),
        Err(_) => registry.with(fmt.with_writer(std::io::stderr)).init(),
    }
}
