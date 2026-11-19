//! Per-user registry integration (no admin rights needed).

use anyhow::{Result, bail};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

pub const AUMID: &str = "NMMail.Mail";
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_APPROVED: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
pub const RUN_VALUE: &str = "NM Mail";

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: handle obtained from RegCreateKeyExW / RegOpenKeyExW.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn create(path: &str) -> Result<Key> {
    let mut key = HKEY::default();
    // SAFETY: valid strings and out-pointer.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE | KEY_READ,
            None,
            &mut key,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        bail!("RegCreateKeyExW({path}) failed: {rc:?}");
    }
    Ok(Key(key))
}

pub fn set_string(path: &str, name: Option<&str>, value: &str) -> Result<()> {
    let key = create(path)?;
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2) };
    let name_h = name.map(HSTRING::from);
    let name_p = name_h
        .as_ref()
        .map(|h| PCWSTR(h.as_ptr()))
        .unwrap_or(PCWSTR::null());
    // SAFETY: open key, valid buffers.
    let rc = unsafe { RegSetValueExW(key.0, name_p, None, REG_SZ, Some(bytes)) };
    if rc != ERROR_SUCCESS {
        bail!("RegSetValueExW({path}) failed: {rc:?}");
    }
    Ok(())
}

pub fn delete_value(path: &str, name: &str) {
    let mut key = HKEY::default();
    // SAFETY: valid strings and out-pointer.
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            KEY_WRITE,
            &mut key,
        )
    };
    if rc == ERROR_SUCCESS {
        let key = Key(key);
        // SAFETY: open key.
        unsafe {
            let _ = RegDeleteValueW(key.0, &HSTRING::from(name));
        }
    }
}

pub fn delete_tree(path: &str) {
    // SAFETY: valid string.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path));
    }
}

/// Registers the `nmmail:` protocol and the notification identity.
pub fn register_integration(exe: &str, icon_png: Option<&str>) -> Result<()> {
    let base = r"Software\Classes\nmmail";
    set_string(base, None, "URL:NM Mail")?;
    set_string(base, Some("URL Protocol"), "")?;
    set_string(
        &format!(r"{base}\DefaultIcon"),
        None,
        &format!("\"{exe}\",0"),
    )?;
    set_string(
        &format!(r"{base}\shell\open\command"),
        None,
        &format!("\"{exe}\" --activate \"%1\""),
    )?;
    let aumid = format!(r"Software\Classes\AppUserModelId\{AUMID}");
    set_string(&aumid, Some("DisplayName"), "NM Mail")?;
    if let Some(png) = icon_png {
        set_string(&aumid, Some("IconUri"), png)?;
    }
    Ok(())
}

/// Offers NM Mail as a `mailto:` handler. The user picks it in Settings →
/// Default apps (Windows does not let apps take over defaults silently).
pub fn register_mailto(ui_exe: &str) -> Result<()> {
    let client = r"Software\Clients\Mail\NM Mail";
    let caps = format!(r"{client}\Capabilities");
    set_string(client, None, "NM Mail")?;
    set_string(&caps, Some("ApplicationName"), "NM Mail")?;
    set_string(
        &caps,
        Some("ApplicationDescription"),
        "Client di posta leggero con notifiche istantanee",
    )?;
    set_string(
        &format!(r"{caps}\URLAssociations"),
        Some("mailto"),
        "NMMail.Url.mailto",
    )?;
    set_string(r"Software\RegisteredApplications", Some("NM Mail"), &caps)?;
    let prog = r"Software\Classes\NMMail.Url.mailto";
    set_string(prog, None, "URL:MailTo Protocol")?;
    set_string(prog, Some("URL Protocol"), "")?;
    set_string(
        &format!(r"{prog}\DefaultIcon"),
        None,
        &format!("\"{ui_exe}\",0"),
    )?;
    set_string(
        &format!(r"{prog}\shell\open\command"),
        None,
        &format!("\"{ui_exe}\" --compose \"%1\""),
    )?;
    Ok(())
}

pub fn unregister_integration() {
    delete_tree(r"Software\Clients\Mail\NM Mail");
    delete_tree(r"Software\Classes\NMMail.Url.mailto");
    delete_value(r"Software\RegisteredApplications", "NM Mail");
    delete_tree(r"Software\Classes\nmmail");
    delete_tree(&format!(r"Software\Classes\AppUserModelId\{AUMID}"));
    delete_value(RUN_KEY, RUN_VALUE);
}

pub fn set_autostart(exe: &str, enabled: bool, user_initiated: bool) -> Result<()> {
    if enabled {
        set_string(RUN_KEY, Some(RUN_VALUE), &format!("\"{exe}\" --background"))?;
        if user_initiated {
            // Re-enable if the user had disabled us in Task Manager → Startup.
            delete_value(STARTUP_APPROVED, RUN_VALUE);
        }
    } else {
        delete_value(RUN_KEY, RUN_VALUE);
    }
    Ok(())
}
