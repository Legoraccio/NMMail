//! File logging (daily rotation, 7 files) without extra threads.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

pub fn init() {
    let filter = EnvFilter::try_from_env("NMMAIL_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,async_imap=warn,rustls=warn,lettre=warn"));
    let dir = nmmail_core::paths::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("nmmaild")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir);
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);
    match file {
        Ok(file) if cfg!(debug_assertions) => {
            builder.with_writer(file.and(std::io::stderr)).init();
        }
        Ok(file) => builder.with_writer(file).init(),
        Err(_) => builder.with_writer(std::io::stderr).init(),
    }
}
