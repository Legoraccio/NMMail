//! Hidden window: tray icon + menu, power/session/display events.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    POWERBROADCAST_SETTING, RegisterPowerSettingNotification, RegisterSuspendResumeNotification,
};
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
};
use windows::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use super::{PLATFORM, TrayState};
use crate::i18n::{S, t};

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_REFRESH: u32 = WM_APP + 2;
pub const WM_QUIT_TRAY: u32 = WM_APP + 3;

const ID_OPEN: usize = 1;
const ID_CHECK: usize = 2;
const ID_PAUSE_1H: usize = 10;
const ID_PAUSE_TOMORROW: usize = 11;
const ID_RESUME: usize = 12;
const ID_EXIT: usize = 99;

const PBT_APMSUSPEND: u32 = 0x4;
const PBT_APMRESUMESUSPEND: u32 = 0x7;
const PBT_APMRESUMEAUTOMATIC: u32 = 0x12;
const PBT_POWERSETTINGCHANGE: u32 = 0x8013;
const WTS_SESSION_UNLOCK: u32 = 0x8;

static TASKBAR_CREATED: AtomicIsize = AtomicIsize::new(0);

#[derive(Clone, Copy)]
pub struct Icons {
    pub normal: HICON,
    pub unread: HICON,
    pub offline: HICON,
}

fn load_icon(hinst: HINSTANCE, id: u16) -> Option<HICON> {
    // SAFETY: MAKEINTRESOURCE-style id; system metrics give the DPI-aware small size.
    unsafe {
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        LoadImageW(
            Some(hinst),
            PCWSTR(id as usize as *const u16),
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        )
        .ok()
        .map(|h| HICON(h.0))
    }
}

pub fn load_icons() -> Icons {
    // SAFETY: module handle of our own executable.
    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default() };
    let fallback = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() };
    let normal = load_icon(hinst, 1).unwrap_or(fallback);
    Icons {
        normal,
        unread: load_icon(hinst, 2).unwrap_or(normal),
        offline: load_icon(hinst, 3).unwrap_or(normal),
    }
}

fn tip_wide(text: &str) -> [u16; 128] {
    let mut buf = [0u16; 128];
    for (i, c) in text.encode_utf16().take(127).enumerate() {
        buf[i] = c;
    }
    buf
}

fn nid(hwnd: HWND, state: &TrayState, icons: &Icons) -> NOTIFYICONDATAW {
    let icon = if state.problem.is_some() {
        icons.offline
    } else if state.unread > 0 {
        icons.unread
    } else {
        icons.normal
    };
    let lang = state.lang;
    let mut tip = if state.unread > 0 {
        format!("NM Mail — {} {}", state.unread, t(lang, S::Unread))
    } else {
        format!("NM Mail — {}", t(lang, S::NoUnread))
    };
    if state.paused {
        tip.push_str(&format!("\n{}", t(lang, S::Paused)));
    }
    if let Some(p) = &state.problem {
        let p: String = p.chars().take(60).collect();
        tip.push_str(&format!("\n{p}"));
    }
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        szTip: tip_wide(&tip),
        ..Default::default()
    }
}

fn update_icon(hwnd: HWND, add: bool) {
    let Some(p) = PLATFORM.get() else { return };
    let state = p.tray.lock().unwrap().clone();
    let data = nid(hwnd, &state, &p.icons);
    // SAFETY: data fully initialized for our window.
    unsafe {
        if add || !Shell_NotifyIconW(NIM_MODIFY, &data).as_bool() {
            let _ = Shell_NotifyIconW(NIM_ADD, &data);
        }
    }
}

fn show_menu(hwnd: HWND) {
    let Some(p) = PLATFORM.get() else { return };
    let state = p.tray.lock().unwrap().clone();
    let lang = state.lang;
    // SAFETY: standard popup menu sequence on our own window.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let Ok(pause) = CreatePopupMenu() else { return };
        let add = |m: HMENU, id: usize, text: &str, flags: MENU_ITEM_FLAGS| {
            let _ = AppendMenuW(m, flags, id, &HSTRING::from(text));
        };
        add(menu, ID_OPEN, t(lang, S::Open), MF_STRING);
        add(menu, ID_CHECK, t(lang, S::CheckNow), MF_STRING);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(pause, ID_PAUSE_1H, t(lang, S::Pause1h), MF_STRING);
        add(
            pause,
            ID_PAUSE_TOMORROW,
            t(lang, S::PauseTomorrow),
            MF_STRING,
        );
        add(
            pause,
            ID_RESUME,
            t(lang, S::Resume),
            if state.paused {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            },
        );
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            pause.0 as usize,
            &HSTRING::from(t(lang, S::Pause)),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(menu, ID_EXIT, t(lang, S::Exit), MF_STRING);
        let _ = SetMenuDefaultItem(menu, ID_OPEN as u32, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        handle_command(cmd.0 as usize);
    }
}

fn handle_command(cmd: usize) {
    let Some(p) = PLATFORM.get() else { return };
    let Some(engine) = p.engine.get().cloned() else {
        return;
    };
    let now = nmmail_core::model::now_ms();
    match cmd {
        ID_OPEN => engine.show_ui(None),
        ID_CHECK => engine.sync_now(None),
        ID_PAUSE_1H => {
            let _ = engine.pause_notifications(Some(now + 3_600_000));
        }
        ID_PAUSE_TOMORROW => {
            use chrono::{Duration, Local, TimeZone};
            let tomorrow = (Local::now() + Duration::days(1))
                .date_naive()
                .and_hms_opt(8, 0, 0);
            let until = tomorrow
                .and_then(|t| Local.from_local_datetime(&t).single())
                .map(|t| t.timestamp_millis())
                .unwrap_or(now + 12 * 3_600_000);
            let _ = engine.pause_notifications(Some(until));
        }
        ID_RESUME => {
            let _ = engine.pause_notifications(None);
        }
        ID_EXIT => engine.request_shutdown(),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed) as u32;
    match msg {
        WM_TRAY => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => handle_command(ID_OPEN),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_REFRESH => {
            update_icon(hwnd, false);
            LRESULT(0)
        }
        WM_QUIT_TRAY => {
            // SAFETY: our own window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if let Some(p) = PLATFORM.get() {
                match wparam.0 as u32 {
                    PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => p.on_resume(),
                    PBT_APMSUSPEND => tracing::info!("system suspending"),
                    PBT_POWERSETTINGCHANGE => {
                        // SAFETY: lParam points to a POWERBROADCAST_SETTING for this message.
                        let s = unsafe { &*(lparam.0 as *const POWERBROADCAST_SETTING) };
                        if s.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && s.Data[0] == 1 {
                            p.on_network_change();
                        }
                    }
                    _ => {}
                }
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE => {
            if wparam.0 as u32 == WTS_SESSION_UNLOCK
                && let Some(p) = PLATFORM.get()
            {
                p.on_network_change();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: 1,
                ..Default::default()
            };
            // SAFETY: removing our own icon.
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        m if taskbar_created != 0 && m == taskbar_created => {
            // Explorer restarted: re-add the icon.
            update_icon(hwnd, true);
            LRESULT(0)
        }
        // SAFETY: default processing.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Creates the hidden window and runs its message loop (blocking).
pub fn run(on_ready: impl FnOnce(HWND)) {
    // SAFETY: standard window class registration + message loop on this thread.
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
        let class = w!("NMMailTrayWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")) as isize,
            Ordering::Relaxed,
        );
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            w!("NM Mail"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            tracing::error!("cannot create tray window");
            return;
        };
        let h = HANDLE(hwnd.0);
        let _ = RegisterSuspendResumeNotification(h, DEVICE_NOTIFY_WINDOW_HANDLE);
        let _ = RegisterPowerSettingNotification(
            h,
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        );
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        update_icon(hwnd, true);
        on_ready(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
