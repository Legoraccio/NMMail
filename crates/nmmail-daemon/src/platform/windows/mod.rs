//! Windows implementation of the daemon platform layer.

mod pipe;
mod registry;
mod toast;
mod tray;

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use nmmail_core::events::Event;
use nmmail_core::model::{AccountId, MessageId, now_ms};
use nmmail_core::{Engine, Notification, Platform};
use tokio::sync::mpsc;
use tracing::{info, warn};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::Globalization::GetUserDefaultUILanguage;
use windows::Win32::NetworkManagement::IpHelper::{
    MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE, NotifyIpInterfaceChange,
};
use windows::Win32::Networking::WinSock::AF_UNSPEC;
use windows::Win32::System::ProcessStatus::{
    EmptyWorkingSet, GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    ASFW_ANY, AllowSetForegroundWindow, PostMessageW, SW_SHOWNORMAL,
};
use windows::core::{HSTRING, w};

pub use pipe::{accept_loop, bind_ipc, connect_ipc};

use crate::i18n::Lang;

pub(crate) static PLATFORM: OnceLock<Arc<NativePlatform>> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct TrayState {
    pub unread: u32,
    pub problem: Option<String>,
    pub paused: bool,
    pub lang: Lang,
}

pub struct NativePlatform {
    pub(crate) tray: Mutex<TrayState>,
    pub(crate) icons: tray::Icons,
    pub(crate) engine: OnceLock<Engine>,
    hwnd: Mutex<Option<isize>>,
    ui_thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    last_ui_launch: Mutex<Option<Instant>>,
    net_tx: OnceLock<mpsc::UnboundedSender<()>>,
}

// HICONs are process-wide handles.
unsafe impl Send for NativePlatform {}
unsafe impl Sync for NativePlatform {}

/// One-time process setup: WinRT apartment and EcoQoS (Efficiency mode).
pub fn prepare_process() {
    // SAFETY: plain Win32/WinRT initialization calls.
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        };
        if SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const c_void,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
        .is_err()
        {
            warn!("EcoQoS not available");
        }
    }
}

pub fn system_lang() -> Lang {
    // SAFETY: no arguments.
    let langid = unsafe { GetUserDefaultUILanguage() };
    if langid & 0x3ff == 0x10 {
        Lang::It
    } else {
        Lang::En
    }
}

pub fn allow_foreground() {
    // SAFETY: no pointers involved.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("nmmaild.exe"))
}

fn icon_png() -> Option<String> {
    let exe = exe_path();
    let dir = exe.parent()?;
    let candidates = [
        dir.join("nmmail.png"),
        dir.join("assets").join("nmmail.png"),
        // Development layout: target/<profile>/nmmaild.exe
        dir.join("..")
            .join("..")
            .join("assets")
            .join("icon")
            .join("nmmail.png"),
    ];
    candidates
        .iter()
        .find(|p| p.exists())
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().trim_start_matches(r"\\?\").to_string())
}

/// Registers (or removes) protocol handler + notification identity.
pub fn register(enable: bool) -> Result<()> {
    if enable {
        registry::register_integration(&exe_path().to_string_lossy(), icon_png().as_deref())
    } else {
        registry::unregister_integration();
        Ok(())
    }
}

pub fn spawn_daemon() -> Result<()> {
    std::process::Command::new(exe_path())
        .arg("--background")
        .spawn()?;
    Ok(())
}

fn ui_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("NMMAIL_UI") {
        return Some(PathBuf::from(p));
    }
    let p = exe_path().parent()?.join("NMMail.exe");
    p.exists().then_some(p)
}

unsafe extern "system" fn net_changed(
    _ctx: *const c_void,
    _row: *const MIB_IPINTERFACE_ROW,
    _t: MIB_NOTIFICATION_TYPE,
) {
    if let Some(p) = PLATFORM.get()
        && let Some(tx) = p.net_tx.get()
    {
        let _ = tx.send(());
    }
}

impl NativePlatform {
    pub fn new() -> Arc<NativePlatform> {
        PLATFORM
            .get_or_init(|| {
                Arc::new(NativePlatform {
                    tray: Mutex::new(TrayState {
                        unread: 0,
                        problem: None,
                        paused: false,
                        lang: system_lang(),
                    }),
                    icons: tray::load_icons(),
                    engine: OnceLock::new(),
                    hwnd: Mutex::new(None),
                    ui_thread: Mutex::new(None),
                    last_ui_launch: Mutex::new(None),
                    net_tx: OnceLock::new(),
                })
            })
            .clone()
    }

    /// Connects the platform to the engine: tray thread, network watcher,
    /// settings listener, periodic working-set trimming.
    pub fn attach(self: &Arc<Self>, engine: Engine, rt: tokio::runtime::Handle) {
        let _ = self.engine.set(engine.clone());
        {
            let s = engine.settings();
            let mut t = self.tray.lock().unwrap();
            t.lang = Lang::resolve(&s.language);
            t.paused = s.is_quiet(now_ms());
        }

        // Tray / power events thread.
        let me = self.clone();
        let handle = std::thread::Builder::new()
            .name("tray".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                tray::run(|hwnd| {
                    *me.hwnd.lock().unwrap() = Some(hwnd.0 as isize);
                });
            })
            .ok();
        *self.ui_thread.lock().unwrap() = handle;

        // Network changes, debounced.
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let _ = self.net_tx.set(tx);
        let mut h = HANDLE::default();
        // SAFETY: static callback, no context pointer.
        unsafe {
            let _ = NotifyIpInterfaceChange(AF_UNSPEC, Some(net_changed), None, false, &mut h);
        }
        let eng = engine.clone();
        rt.spawn(async move {
            while rx.recv().await.is_some() {
                // Coalesce bursts (adapters flapping, DHCP, VPN...).
                tokio::time::sleep(Duration::from_millis(1500)).await;
                while rx.try_recv().is_ok() {}
                info!("network change");
                eng.on_network_change();
            }
        });

        // Follow settings (language, pause).
        let me = self.clone();
        let mut events = engine.subscribe();
        rt.spawn(async move {
            loop {
                match events.recv().await {
                    Ok(Event::SettingsChanged(s)) => {
                        {
                            let mut t = me.tray.lock().unwrap();
                            t.lang = Lang::resolve(&s.language);
                            t.paused = s.is_quiet(now_ms());
                        }
                        me.refresh_tray();
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });

        // Give memory back after startup bursts, then periodically while idle.
        rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(90)).await;
            loop {
                // SAFETY: trims our own working set; pages fault back in on demand.
                unsafe {
                    let _ = EmptyWorkingSet(GetCurrentProcess());
                }
                tokio::time::sleep(Duration::from_secs(30 * 60)).await;
            }
        });
    }

    fn hwnd(&self) -> Option<HWND> {
        self.hwnd.lock().unwrap().map(|h| HWND(h as *mut c_void))
    }

    fn refresh_tray(&self) {
        if let Some(hwnd) = self.hwnd() {
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(Some(hwnd), tray::WM_REFRESH, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub(crate) fn on_resume(&self) {
        if let Some(e) = self.engine.get() {
            e.on_resume();
        }
    }

    pub(crate) fn on_network_change(&self) {
        if let Some(tx) = self.net_tx.get() {
            let _ = tx.send(());
        }
    }

    /// Removes the tray icon and stops the tray thread.
    pub fn shutdown(&self) {
        if let Some(hwnd) = self.hwnd() {
            // SAFETY: posting to our own window.
            unsafe {
                let _ = PostMessageW(Some(hwnd), tray::WM_QUIT_TRAY, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(h) = self.ui_thread.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

impl Platform for NativePlatform {
    fn open_url(&self, url: &str) {
        // Only web URLs are opened from the daemon.
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return;
        }
        // SAFETY: valid strings; ShellExecute launches the default browser.
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(url),
                None,
                None,
                SW_SHOWNORMAL,
            );
        }
    }

    fn notify(&self, n: &Notification) {
        toast::show(n, Lang::resolve(&n.language));
    }

    fn retract(&self, account_id: AccountId, ids: &[MessageId]) {
        toast::retract(account_id, ids);
    }

    fn unread_changed(&self, total: u32, problem: Option<&str>) {
        {
            let mut t = self.tray.lock().unwrap();
            t.unread = total;
            t.problem = problem.map(|p| p.to_string());
        }
        self.refresh_tray();
    }

    fn set_autostart(&self, enabled: bool, user_initiated: bool) -> Result<()> {
        // Development builds never register themselves to start at login.
        if cfg!(debug_assertions) && std::env::var_os("NMMAIL_FORCE_AUTOSTART").is_none() {
            return Ok(());
        }
        registry::set_autostart(&exe_path().to_string_lossy(), enabled, user_initiated)
    }

    fn show_ui(&self, message_id: Option<MessageId>, ui_connected: bool) {
        allow_foreground();
        if ui_connected {
            return;
        }
        {
            let mut last = self.last_ui_launch.lock().unwrap();
            if last.is_some_and(|t| t.elapsed() < Duration::from_secs(4)) {
                return;
            }
            *last = Some(Instant::now());
        }
        let Some(ui) = ui_path() else {
            warn!(
                "UI executable not found (set NMMAIL_UI or install NMMail.exe next to nmmaild.exe)"
            );
            return;
        };
        let mut cmd = std::process::Command::new(&ui);
        if let Some(id) = message_id {
            cmd.arg("--open").arg(id.to_string());
        }
        if let Err(e) = cmd.spawn() {
            warn!("cannot start UI {}: {e}", ui.display());
        }
    }

    fn memory_bytes(&self) -> u64 {
        let mut c = PROCESS_MEMORY_COUNTERS_EX::default();
        // SAFETY: correctly sized out-struct.
        unsafe {
            if GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            )
            .is_ok()
            {
                return c.PrivateUsage as u64;
            }
        }
        0
    }
}
