//! Secret storage: OS credential store (Windows Credential Manager today).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn delete(&self, key: &str) -> Result<()>;
}

/// In-memory store (tests, and platforms without an implementation yet).
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
}

/// The platform's default secret store.
pub fn platform_store() -> Arc<dyn SecretStore> {
    #[cfg(windows)]
    {
        Arc::new(windows::CredentialManager)
    }
    #[cfg(not(windows))]
    {
        // TODO(macOS/Linux): Keychain / Secret Service.
        Arc::new(MemoryStore::default())
    }
}

#[cfg(windows)]
mod windows {
    use super::SecretStore;
    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::{ERROR_NOT_FOUND, GetLastError};
    use windows_sys::Win32::Security::Credentials::{
        CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
        CredReadW, CredWriteW,
    };

    /// Max blob size accepted by CredWriteW for generic credentials.
    const CHUNK: usize = 2048;

    pub struct CredentialManager;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn target(key: &str, part: usize) -> String {
        if part == 0 {
            format!("NMMail:{key}")
        } else {
            format!("NMMail:{key}#{part}")
        }
    }

    fn read_raw(target: &str) -> Result<Option<Vec<u8>>> {
        let t = wide(target);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: valid NUL-terminated target, out-pointer freed with CredFree.
        unsafe {
            if CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut cred) == 0 {
                let err = GetLastError();
                if err == ERROR_NOT_FOUND {
                    return Ok(None);
                }
                bail!("CredReadW failed: {err}");
            }
            let c = &*cred;
            let data = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize)
                .to_vec();
            CredFree(cred as *const _);
            Ok(Some(data))
        }
    }

    fn write_raw(target: &str, data: &[u8]) -> Result<()> {
        let mut t = wide(target);
        let mut user = wide("NM Mail");
        let cred = CREDENTIALW {
            Flags: 0,
            Type: CRED_TYPE_GENERIC,
            TargetName: t.as_mut_ptr(),
            Comment: std::ptr::null_mut(),
            LastWritten: unsafe { std::mem::zeroed() },
            CredentialBlobSize: data.len() as u32,
            CredentialBlob: data.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: std::ptr::null_mut(),
            UserName: user.as_mut_ptr(),
        };
        // SAFETY: all pointers are valid for the duration of the call.
        unsafe {
            if CredWriteW(&cred, 0) == 0 {
                bail!("CredWriteW failed: {}", GetLastError());
            }
        }
        Ok(())
    }

    fn delete_raw(target: &str) {
        let t = wide(target);
        // SAFETY: valid NUL-terminated string.
        unsafe {
            CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0);
        }
    }

    impl SecretStore for CredentialManager {
        fn get(&self, key: &str) -> Result<Option<String>> {
            let Some(first) = read_raw(&target(key, 0))? else {
                return Ok(None);
            };
            // Long values are split: the first blob is "chunks:N".
            let mut data = first;
            if let Some(n) = std::str::from_utf8(&data)
                .ok()
                .and_then(|s| s.strip_prefix("chunks:"))
                .and_then(|n| n.parse::<usize>().ok())
            {
                data = Vec::new();
                for i in 1..=n {
                    match read_raw(&target(key, i))? {
                        Some(part) => data.extend_from_slice(&part),
                        None => return Ok(None),
                    }
                }
            }
            Ok(Some(String::from_utf8_lossy(&data).into_owned()))
        }

        fn set(&self, key: &str, value: &str) -> Result<()> {
            self.delete(key)?;
            let bytes = value.as_bytes();
            if bytes.len() <= CHUNK {
                return write_raw(&target(key, 0), bytes);
            }
            let chunks: Vec<&[u8]> = bytes.chunks(CHUNK).collect();
            for (i, c) in chunks.iter().enumerate() {
                write_raw(&target(key, i + 1), c)?;
            }
            write_raw(
                &target(key, 0),
                format!("chunks:{}", chunks.len()).as_bytes(),
            )
        }

        fn delete(&self, key: &str) -> Result<()> {
            if let Some(first) = read_raw(&target(key, 0))?
                && let Some(n) = std::str::from_utf8(&first)
                    .ok()
                    .and_then(|s| s.strip_prefix("chunks:"))
                    .and_then(|n| n.parse::<usize>().ok())
            {
                for i in 1..=n {
                    delete_raw(&target(key, i));
                }
            }
            delete_raw(&target(key, 0));
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn roundtrip_short_and_long() {
            let s = CredentialManager;
            let key = format!("test-{}", std::process::id());
            s.set(&key, "segreto").unwrap();
            assert_eq!(s.get(&key).unwrap().as_deref(), Some("segreto"));
            let long = "x".repeat(5000);
            s.set(&key, &long).unwrap();
            assert_eq!(s.get(&key).unwrap().as_deref(), Some(long.as_str()));
            s.delete(&key).unwrap();
            assert_eq!(s.get(&key).unwrap(), None);
        }
    }
}
