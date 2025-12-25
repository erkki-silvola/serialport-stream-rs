// Platform-specific implementations

#[cfg(unix)]
pub mod unix;

#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
pub use unix::PlatformStream;

#[cfg(windows)]
pub use windows::PlatformStream;
