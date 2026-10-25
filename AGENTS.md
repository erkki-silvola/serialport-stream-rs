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
- [ ] README / `//!` updated only if behavior or public API changed
