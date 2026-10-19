// Platform-specific implementations

#[cfg(unix)]
pub mod unix;

#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub use unix::{PlatformRead, PlatformSettings, PlatformStream, PlatformWrite};

#[cfg(windows)]
pub use windows::{PlatformRead, PlatformSettings, PlatformStream, PlatformWrite};
