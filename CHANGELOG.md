# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-xx-xx

##### Changed

* `AsyncWrite::poll_write` writes directly from the caller's buffer (no intermediate copy). On Unix, the background thread only wakes the task when the port is writable again after `EAGAIN`; on Windows, overlapped `WriteFile` starts during the poll and the thread awaits completion.
* Windows serial handle ownership refactored to RAII (`OwnedHandle` / `Arc`) so drop no longer double-closes handles.

##### Fixed

* Unix read/write retry on `EINTR`.
* Windows in-flight write cancellation uses `CancelIoEx`.
* Propagated read/write errors preserve the original raw OS error code when available.

## [0.3.0] - 2026-06-06

##### Fixed

* linux musl build.

## [0.3.0-beta.1] - 2026-06-05

##### Added

* `futures::io::AsyncWrite` with a dedicated background write thread; re-export `AsyncWriteExt`.

##### Changed

* Removed the `serialport` dependency; ports are opened with native POSIX (`termios`) and Win32 (COMM/DCB) APIs.
* Configuration types (`DataBits`, `Parity`, `StopBits`, `FlowControl`, `ClearBuffer`) are defined in this crate.

##### Removed

* `pub use serialport` — import configuration types from `serialport_stream`.
* `SerialPortStream` control-line helpers (`clear`, `set_break`, modem status reads, `bytes_to_read`, etc.). Use `.clear(ClearBuffer)` on the builder before `.open()` to purge buffers at open.

## [0.2.0] - 2026-05-27

##### Added

* `futures::io::AsyncRead` for `SerialPortStream`, sharing the receive thread buffer with `futures::Stream`.
* Re-export `futures::io::AsyncRead` and `AsyncReadExt`.

##### Removed

* Builder `.timeout` on `SerialPortStreamBuilder`; read timeout is no longer configured when opening the port (serialport-rs defaults apply on Unix).
* `std::io::Read` and `std::io::Write` / `flush` on `SerialPortStream`. Incoming data is via `futures::Stream` or `futures::io::AsyncRead` only.

## [0.1.8] - 2026-04-22

##### Added

* `line_settings` module with helpers for mapping data bits, stop bits, parity, and flow control from primitive values.

##### Fixed

* Windows error handling improvements.
* Windows build fix.

## [0.1.7] - 2026-04-06

##### Changed

* serialport-rs 4.9.0.
* Windows serial I/O implementation updated for current serialport and `windows-sys` APIs.

## [0.1.6] - 2025-02-21

##### Changed

* Docs updated.
* Send and Sync impl.

## [0.1.5] - 2025-02-13

##### Changed

* Docs updated.
* serialport-rs 4.7.3, for better dependency graph.

## [0.1.4] - 2025-01-31

##### Changed

* Docs updated.
* Exposed rest of serialport functionality.

## [0.1.3] - 2025-01-23

##### Changed

* Added derive debug.
* Fixed url's.

## [0.1.2] - 2025-01-18

##### Changed

* Updated samples.
* Simplified windows code.

## [0.1.1] - 2025-01-06

##### Fixed

* Fixed error output on windows from comm event.
* Fixed jlinkcdc driver support on windows.

## [0.1.0] - 2025-01-02
