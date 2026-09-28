//! Platform code: HTTP, where the installer put the app, running Setup.

#[cfg(not(windows))]
mod other;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use self::windows::{installations, native};
#[cfg(not(windows))]
pub use other::{installations, native};
