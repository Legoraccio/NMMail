//! Named-pipe IPC endpoint restricted to the current user.

use std::ffi::c_void;
use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use nmmail_core::{Engine, ipc, paths};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tracing::warn;
use windows::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

/// SECURITY_ATTRIBUTES granting access to the current user only.
/// Leaked on purpose: lives as long as the process.
struct Sa(*mut SECURITY_ATTRIBUTES);
unsafe impl Send for Sa {}
unsafe impl Sync for Sa {}

fn current_user_sid() -> Result<String> {
    // SAFETY: standard token query sequence; buffers sized by the first call.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            len,
            &mut len,
        )?;
        let _ = windows::Win32::Foundation::CloseHandle(token);
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s)?;
        let out = s.to_string()?;
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut c_void)));
        Ok(out)
    }
}

fn security_attributes() -> Result<&'static Sa> {
    static SA: std::sync::OnceLock<Sa> = std::sync::OnceLock::new();
    if let Some(sa) = SA.get() {
        return Ok(sa);
    }
    let sid = current_user_sid()?;
    let sddl = format!("D:P(A;;GA;;;{sid})");
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: valid SDDL string; descriptor intentionally leaked.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &HSTRING::from(sddl),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )
        .context("building pipe security descriptor")?;
    }
    let sa = Box::new(SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    });
    Ok(SA.get_or_init(|| Sa(Box::into_raw(sa))))
}

fn create(first: bool) -> io::Result<NamedPipeServer> {
    let sa = security_attributes().map_err(io::Error::other)?;
    let mut opts = ServerOptions::new();
    opts.first_pipe_instance(first).reject_remote_clients(true);
    // SAFETY: `sa` points to a valid, leaked SECURITY_ATTRIBUTES.
    unsafe { opts.create_with_security_attributes_raw(paths::ipc_endpoint(), sa.0 as *mut c_void) }
}

/// Creates the first pipe instance. `Ok(None)` means another daemon owns it.
pub fn bind_ipc() -> Result<Option<NamedPipeServer>> {
    match create(true) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.raw_os_error() == Some(5) || e.raw_os_error() == Some(231) => Ok(None),
        Err(e) => Err(e).context("creating IPC pipe"),
    }
}

pub async fn accept_loop(mut server: NamedPipeServer, engine: Engine) {
    loop {
        if let Err(e) = server.connect().await {
            warn!("pipe connect: {e}");
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        let next = match create(false) {
            Ok(n) => n,
            Err(e) => {
                warn!("pipe create: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let connected = std::mem::replace(&mut server, next);
        tokio::spawn(ipc::serve_connection(engine.clone(), connected));
    }
}

pub async fn connect_ipc() -> io::Result<NamedPipeClient> {
    let name = paths::ipc_endpoint();
    for _ in 0..20 {
        match ClientOptions::new().open(&name) {
            Ok(c) => return Ok(c),
            Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("pipe busy"))
}
