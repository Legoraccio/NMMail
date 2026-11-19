//! OS integration layer. Each OS module exposes the same surface:
//!
//! * `NativePlatform` implementing `nmmail_core::Platform` (+ `new`, `attach`, `shutdown`)
//! * `prepare_process`, `bind_ipc`, `accept_loop`, `connect_ipc`, `spawn_daemon`
//! * `register`, `allow_foreground`, `system_lang`

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(not(windows))]
mod other;
#[cfg(not(windows))]
pub use self::other::*;
