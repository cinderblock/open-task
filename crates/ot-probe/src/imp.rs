//! Selects the platform backend. The only `cfg`-dispatch point in the crate.

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{
    is_elevated, WindowsControl as PlatformControl, WindowsProbe as PlatformProbe,
    WindowsSampler as PlatformSampler,
};

#[cfg(not(windows))]
mod stub;
#[cfg(not(windows))]
pub use stub::{
    is_elevated, StubControl as PlatformControl, StubProbe as PlatformProbe,
    StubSampler as PlatformSampler,
};
