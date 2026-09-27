// Platform-specific implementations

#[cfg(unix)]
pub mod unix;

#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub use unix::{PlatformReadHalf, PlatformStream, PlatformWriteHalf};

#[cfg(windows)]
pub use windows::{PlatformReadHalf, PlatformStream, PlatformWriteHalf};
