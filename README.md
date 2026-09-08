# serialport-stream-rs

Async serial port I/O as [`futures::AsyncRead`](https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html) and [`AsyncWrite`](https://docs.rs/futures/latest/futures/io/trait.AsyncWrite.html), with optional [`Stream`](https://docs.rs/futures/latest/futures/stream/trait.Stream.html) support. Uses POSIX termios on Unix and Win32 COMM APIs on Windows.

**Async runtime agnostic** — implements [`futures`](https://docs.rs/futures) traits only; no Tokio/async-std dependency. Works with any executor that polls those futures (Tokio, async-std, `futures_lite::future::block_on`, etc.).

## Installation

```toml
[dependencies]
serialport-stream = "0.3"
```

Optional features:

```toml
# Stream / try_next / receive FIFO pump (Unix); Stream API on Windows
serialport-stream = { version = "0.3", features = ["stream"] }

# Diagnostic logs (EAGAIN retries, receive-buffer diagnostics)
serialport-stream = { version = "0.3", features = ["tracing"] }
```

Examples below also use `futures-lite` (blocking) or `tokio`.

## Read behavior

| Platform | `AsyncRead` | `Stream` / `try_next` (`stream` feature) |
| --- | --- | --- |
| **Unix** | Direct [`async-io`](https://docs.rs/async-io) poll on the port | Poll-based background read thread → FIFO |
| **Windows** | Background read thread → FIFO | Same FIFO; `try_next` drains the full buffer per item |

On **Unix** with `stream` enabled, use either `AsyncRead` or `Stream` per open port — not both.

On **Windows**, `AsyncRead` and `Stream` (with `stream`) share the same FIFO. `AsyncRead` returns up to your buffer size and leaves the remainder cached; `try_next` drains everything at once.

There is no backpressure on FIFO paths; the buffer can grow without bound.

## Usage

### AsyncRead (default)

Works without extra features. On Unix this is the preferred path.

```rust
use serialport_stream::{new, AsyncReadExt};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut port = new("/dev/ttyUSB0", 115200).open()?;
    let mut buf = [0u8; 256];
    let n = port.read(&mut buf).await?;
    println!("read {n} bytes");
    Ok(())
}
```

Example: `cargo run --example tokio_async_read -- /dev/ttyUSB0 115200`

Add `--features tracing` and pass `--trace` in examples that support it for diagnostic logs.

For `tokio::io::AsyncRead`, bridge with [`tokio_util::compat`](https://docs.rs/tokio-util/latest/tokio_util/compat/index.html) (`tokio-util` feature `compat`).

### Stream (`stream` feature)

Requires `features = ["stream"]`.

**Blocking** (`futures_lite::stream::block_on`):

```rust
use serialport_stream::new;
use futures_lite::stream;

fn main() -> std::io::Result<()> {
    let stream = new("COM3", 115200).dtr_on_open(true).open()?;

    for chunk in stream::block_on(stream) {
        println!("{:?}", chunk?);
    }

    Ok(())
}
```

Example: `cargo run --example read_stream --features stream -- COM3 115200`

**Tokio**:

```rust
use serialport_stream::{new, TryStreamExt};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut stream = new("/dev/ttyUSB0", 9600).open()?;

    while let Some(bytes) = stream.try_next().await? {
        println!("Received: {bytes:?}");
    }

    Ok(())
}
```

Example: `cargo run --example tokio_read_stream --features stream -- /dev/ttyUSB0 115200`

### Writing

[`AsyncWriteExt`](https://docs.rs/futures/latest/futures/io/trait.AsyncWriteExt.html) is re-exported. On Unix, writes use async-io; on Windows, overlapped `WriteFile` with a background completion thread.

```rust
use serialport_stream::{new, AsyncWriteExt};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut port = new("/dev/ttyUSB0", 115200).open()?;
    port.write_all(b"PING\r\n").await?;
    port.flush().await?;
    Ok(())
}
```

Read + write example (uses `Stream` for the read side): `cargo run --example tokio_async_rw --features stream -- /dev/ttyUSB0 115200`

For `tokio::io::AsyncWrite`, use [`tokio_util::compat`](https://docs.rs/tokio-util/latest/tokio_util/compat/index.html) as above.

## Builder

Open with [`new(path, baud_rate)`](https://docs.rs/serialport-stream/latest/serialport_stream/fn.new.html), then chain options and call `.open()`:

- `.data_bits`, `.parity`, `.stop_bits`, `.flow_control` — default is 8N1, no flow control
- `.dtr_on_open(bool)` — drive DTR on open
- `.clear(ClearBuffer::Input | Output | All)` — purge driver buffers at open

Types `DataBits`, `Parity`, `StopBits`, `FlowControl`, and `ClearBuffer` are exported from `serialport_stream`.

## Acknowledgements

Some of the platform I/O code is inspired by [serialport-rs](https://github.com/serialport/serialport-rs).

## License

This project is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.
