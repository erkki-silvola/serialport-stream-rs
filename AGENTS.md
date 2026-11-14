# AGENTS.md

Guidance for AI agents working on **serialport-stream** (async `futures` serial I/O, Unix + Windows).

## Layout

```
src/
  lib.rs              # Public API, AsyncRead/Write/Stream, split halves
  types.rs            # DataBits, Parity, StopBits, FlowControl, ClearBuffer
  line_settings.rs    # Shared line-setting helpers
  platform.rs         # cfg unix/windows re-exports
  platform/unix/      # termios, async-io poll read/write
  platform/windows/   # overlapped COMM, optional WaitCommEvent pump
examples/             # Tokio / futures-lite demos (need a real port)
```

Platform code lives behind `PlatformStream` / `PlatformRead` / `PlatformWrite` — keep Unix and Windows behavior aligned at the `lib.rs` API level.

## Cargo features

| Feature | Effect |
| --- | --- |
| `stream` | Background receive thread + FIFO; `Stream`, `try_poll_next`; `AsyncRead` uses FIFO when enabled |
| `tracing` | Optional `tracing` dependency and log points |

Default features are empty. Test and doc with `--all-features` when touching either path.

## Conventions

- Match existing style: compact code, minimal comments (no narration in function bodies).
- Public items in `lib.rs` get brief `///` docs; avoid duplicating README/platform tables in rustdoc.
- Errors: `std::io::Result` at the boundary; no `.unwrap()` in library code.
- Do not add trivial unit tests; examples and manual serial hardware cover integration behavior.
- User-facing usage belongs in `README.md`; crate-level `//!` stays short.

## Win32 `HANDLE` and `OVERLAPPED` safety

Overlapped I/O hands the kernel raw pointers that it may write to **after** the call returns. Be extra careful here: a moved or freed address is silent memory corruption, not a compile error.

- **Stable addresses:** the `OVERLAPPED`, its event `HANDLE`, and every buffer passed to `ReadFile` / `WriteFile` / `WaitCommEvent` (including the `lpEvtMask` out-param) must stay at the same address until the operation completes. Heap-allocate them (`Box`, `Arc`, `Vec` that is never resized) or pin them; never pass pointers to stack locals or to fields of a struct that can be moved (e.g. a future or a value returned by value).
- **No reallocation while pending:** do not push to, resize, swap, or `mem::replace` a buffer or struct that holds an in-flight `OVERLAPPED`.
- **Completion before release:** before dropping or reusing an `OVERLAPPED` or buffer, make sure the operation finished: `GetOverlappedResult(.., TRUE)` after `ERROR_IO_PENDING`, or `CancelIoEx` and then wait for the result (`ERROR_OPERATION_ABORTED` is expected). `Drop` impls must do this too.
- **One operation per `OVERLAPPED`:** never share an `OVERLAPPED` or event between concurrent reads, writes, or comm-event waits; reset the event and zero the struct before each reuse.
- **Handle ownership:** each `HANDLE` has exactly one owner that calls `CloseHandle` once, after all pending I/O on it has completed or been cancelled. Check for `INVALID_HANDLE_VALUE` / null at creation and turn failures into `io::Error::last_os_error()`. Do not `Copy` a handle into a place that outlives its owner.
- **Threads:** when a handle or `OVERLAPPED` crosses threads (e.g. the `stream` receive pump), the owning thread must be joined or signalled to stop and finish its I/O before the handle closes.
- **`unsafe` blocks:** keep them minimal and add a `// SAFETY:` comment that states why the pointers stay valid for the whole operation.

## Commands

```bash
cargo fmt
cargo clippy --all-features -- -D warnings
cargo test --all-features
cargo doc --all-features --no-deps
```

Examples require a device path, e.g. `cargo run --example tokio_async_read -- /dev/ttyUSB0 115200`.

## Change checklist

- [ ] Library compiles on Windows, macOS, and Linux (GNU and musl) when platform I/O changed
- [ ] `stream` on/off paths still compile
- [ ] Windows overlapped changes follow the `HANDLE` / `OVERLAPPED` safety rules (stable addresses, completion before drop or close)
- [ ] README / `//!` updated only if behavior or public API changed
