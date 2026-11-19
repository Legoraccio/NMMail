//! Daemon main loop.

use std::process::ExitCode;
use std::time::Duration;

use nmmail_core::auth::secrets;
use nmmail_core::store::Store;
use nmmail_core::{Engine, paths};
use tracing::{error, info};

use crate::platform;

pub fn run(background: bool) -> ExitCode {
    crate::logging::init();
    platform::prepare_process();
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .thread_keep_alive(Duration::from_secs(5))
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!("runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(serve(background)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            error!("daemon failed: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(background: bool) -> anyhow::Result<()> {
    let Some(listener) = platform::bind_ipc()? else {
        info!("another instance is running");
        if !background {
            // Manual launch while running: just bring the UI up.
            let _ = crate::client::show();
        }
        return Ok(());
    };
    info!(
        version = env!("CARGO_PKG_VERSION"),
        pid = std::process::id(),
        "nmmaild starting"
    );

    let store = Store::open(&paths::db_path())?;
    let plat = platform::NativePlatform::new();
    let engine = Engine::start(store, secrets::platform_store(), plat.clone())?;
    plat.attach(engine.clone(), tokio::runtime::Handle::current());
    if let Err(e) = platform::register(true) {
        tracing::warn!("OS registration: {e:#}");
    }
    engine.apply_autostart();
    tokio::spawn(platform::accept_loop(listener, engine.clone()));

    if !background {
        engine.show_ui(None);
    }

    tokio::select! {
        _ = engine.shutdown_requested() => info!("shutdown requested"),
        _ = tokio::signal::ctrl_c() => info!("ctrl-c"),
    }
    plat.shutdown();
    Ok(())
}
