# serialport-stream

Async serial I/O as [`futures::AsyncRead`](https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html) / [`AsyncWrite`](https://docs.rs/futures/latest/futures/io/trait.AsyncWrite.html), with optional [`Stream`](https://docs.rs/futures/latest/futures/stream/trait.Stream.html). Unix uses termios + async-io; Windows uses overlapped COMM I/O. No Tokio/async-std dependency — any executor that polls `futures` traits works.

## Installation

```toml
serialport-stream = "0.5"
# optional: features = ["stream"] or ["tracing"]
```

| Feature | Purpose |
| --- | --- |
| `stream` | Background receive FIFO; [`Stream`](https://docs.rs/futures/latest/futures/stream/trait.Stream.html) / `try_next` |
| `tracing` | Diagnostic logs |

## Read paths

**Default (no `stream`):** [`AsyncRead`](https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html) polls the open port directly. Each read returns up to your buffer length; leftover bytes stay in the OS/driver buffer until the next read. Writes always use direct async I/O (async-io on Unix, overlapped COMM on Windows).

**With `stream`:** A platform background thread continuously reads the port into an in-memory **FIFO**. Your task consumes from that buffer instead of polling the device:

| API | Each ready poll / await returns |
| --- | --- |
| [`Stream`](https://docs.rs/futures/latest/futures/stream/trait.Stream.html) / `try_next` | Every byte currently in the FIFO (buffer drained) |
| [`AsyncRead`](https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html) | Up to your buffer length; remainder stays in the FIFO |

Use **one** read style per port — do not mix `Stream` / `try_next` and `AsyncRead`, or data can be split unpredictably across calls.

The FIFO has **no backpressure**: if the app reads slower than the device sends, memory use can grow without bound.

| Platform | Direct `AsyncRead` | Background pump (`stream`) |
| --- | --- | --- |
| Unix | async-io poll on the fd | Poll-based read thread → FIFO |
| Windows | Overlapped `ReadFile` + thread pool | `WaitCommEvent` read thread → FIFO |

## Quick start

```rust
use serialport_stream::{new, AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut port = new("/dev/ttyUSB0", 115200).open()?;
    port.write_all(b"PING\r\n").await?;
    let mut buf = [0u8; 256];
    let n = port.read(&mut buf).await?;
    println!("read {n} bytes");
    Ok(())
}
```

Open with [`new(path, baud)`](https://docs.rs/serialport-stream/latest/serialport_stream/fn.new.html), chain `.data_bits`, `.parity`, `.stop_bits`, `.flow_control`, `.dtr_on_open`, `.clear`, then `.open()`. Use [`.split()`](https://docs.rs/serialport-stream/latest/serialport_stream/struct.SerialPortStream.html#method.split) for concurrent read/write tasks.

For native `tokio::io` traits, bridge via [`tokio_util::compat`](https://docs.rs/tokio-util/latest/tokio_util/compat/index.html).

## Stream (`stream` feature)

Enable in `Cargo.toml`:

```toml
serialport-stream = { version = "0.5", features = ["stream"] }
```

See [Read paths](#read-paths) for FIFO semantics. Example:

**Tokio:**

```rust
use serialport_stream::{new, TryStreamExt};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut stream = new("/dev/ttyUSB0", 115200).dtr_on_open(true).open()?;

    while let Some(bytes) = stream.try_next().await? {
        println!("Received: {bytes:?}");
    }

    Ok(())
}
```

**Blocking** (`futures-lite`):

```rust
use serialport_stream::new;
use futures_lite::stream;

fn main() -> std::io::Result<()> {
    let stream = new("COM3", 115200).open()?;

    for chunk in stream::block_on(stream) {
        println!("{:?}", chunk?);
    }

    Ok(())
}
```

## Examples

| Example | Features |
| --- | --- |
| `tokio_async_read` | default |
| `tokio_async_rw` | default |
| `tokio_split_rw` | default |
| `read_stream` | `stream` |
| `tokio_read_stream` | `stream` |

```bash
cargo run --example tokio_async_read -- /dev/ttyUSB0 115200
cargo run --example tokio_read_stream --features stream -- /dev/ttyUSB0 115200
```

Several examples accept `--trace` when built with `--features tracing`.

## License

Dual-licensed under [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT).

Platform I/O is partly inspired by [serialport-rs](https://github.com/serialport/serialport-rs).
