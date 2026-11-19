//! NM Mail core engine (cross-platform).
//!
//! Storage, IMAP push sync, OAuth, SMTP, MIME/HTML handling and the IPC
//! protocol used by the native UI shells. OS-specific behaviour (tray,
//! notifications, autostart, power events) lives in the daemon crate behind
//! [`engine::Platform`].

pub mod auth;
pub mod compose;
pub mod engine;
pub mod error;
pub mod events;
pub mod html;
pub mod imap;
pub mod ipc;
pub mod mime;
pub mod model;
pub mod paths;
pub mod settings;
pub mod smtp;
pub mod store;

pub use engine::{Engine, Notification, NotifyItem, Platform};
