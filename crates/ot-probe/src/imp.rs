//! Selects the platform backend. The only `cfg`-dispatch point in the crate.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::WindowsProbe as PlatformProbe;

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::StubProbe as PlatformProbe;
