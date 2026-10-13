//! Selects the platform backend. The only `cfg`-dispatch point in the crate.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    is_elevated, WindowsControl as PlatformControl, WindowsProbe as PlatformProbe,
    WindowsSampler as PlatformSampler,
};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    is_elevated, LinuxControl as PlatformControl, LinuxProbe as PlatformProbe,
    LinuxSampler as PlatformSampler,
};

#[cfg(not(any(windows, target_os = "linux")))]
mod stub;
#[cfg(not(any(windows, target_os = "linux")))]
pub use stub::{
    is_elevated, StubControl as PlatformControl, StubProbe as PlatformProbe,
    StubSampler as PlatformSampler,
};
