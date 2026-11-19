//! Placeholder platform layer for macOS / Linux (to be implemented: tray,
//! native notifications, LaunchAgent / XDG autostart, sleep/wake events).
//! It keeps the daemon functional (sync + IPC over a unix socket) so the
//! shared code can already be exercised on those systems.

use std::io;
use std::sync::Arc;

use anyhow::Result;
use nmmail_core::model::{AccountId, MessageId};
use nmmail_core::{Engine, Notification, Platform, ipc, paths};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use crate::i18n::Lang;

pub fn prepare_process() {}

pub fn system_lang() -> Lang {
    let lang = std::env::var("LANG").unwrap_or_default();
    if lang.starts_with("it") {
        Lang::It
    } else {
        Lang::En
    }
}

pub fn allow_foreground() {}

pub fn register(_enable: bool) -> Result<()> {
    Ok(())
}

pub fn test_notification_cli() -> String {
    "unsupported".into()
}

pub fn spawn_daemon() -> Result<()> {
    std::process::Command::new(std::env::current_exe()?)
        .arg("--background")
        .spawn()?;
    Ok(())
}

pub fn bind_ipc() -> Result<Option<UnixListener>> {
    let path = paths::ipc_endpoint();
    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
        return Ok(None);
    }
    let _ = std::fs::remove_file(&path);
    if let Some(dir) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let l = UnixListener::bind(&path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(Some(l))
}

pub async fn accept_loop(listener: UnixListener, engine: Engine) {
    loop {
        match listener.accept().await {
            Ok((s, _)) => {
                tokio::spawn(ipc::serve_connection(engine.clone(), s));
            }
            Err(e) => warn!("accept: {e}"),
        }
    }
}

pub async fn connect_ipc() -> io::Result<UnixStream> {
    UnixStream::connect(paths::ipc_endpoint()).await
}

pub struct NativePlatform;

impl NativePlatform {
    pub fn new() -> Arc<NativePlatform> {
        Arc::new(NativePlatform)
    }
    pub fn attach(self: &Arc<Self>, _engine: Engine, _rt: tokio::runtime::Handle) {}
    pub fn shutdown(&self) {}
}

impl Platform for NativePlatform {
    fn open_url(&self, url: &str) {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(url).spawn();
    }
    fn notify(&self, n: &Notification) {
        info!(account = %n.account_email, total = n.total, "new mail");
    }
    fn retract(&self, _a: AccountId, _ids: &[MessageId]) {}
    fn unread_changed(&self, _total: u32, _problem: Option<&str>) {}
    fn set_autostart(&self, _enabled: bool, _user: bool) -> Result<()> {
        Ok(())
    }
    fn show_ui(&self, _m: Option<MessageId>, _c: bool) {}
}
