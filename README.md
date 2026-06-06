# serialport-stream-rs

Async serial port I/O as [`futures::Stream`](https://docs.rs/futures/latest/futures/stream/trait.Stream.html), [`AsyncRead`](https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html), and [`AsyncWrite`](https://docs.rs/futures/latest/futures/io/trait.AsyncWrite.html). Uses POSIX termios on Unix and Win32 COMM APIs on Windows.

## Installation

```toml
[dependencies]
serialport-stream = "0.3"
```

Examples below also use `futures-lite` (blocking) or `tokio`.

## Usage

### Stream (blocking)

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

### Stream (Tokio)

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

### Reading

A background thread fills one in-memory FIFO. There is no backpressure; the buffer can grow without bound.

| API | Each call returns |
| --- | --- |
| `Stream` / `try_next` | All bytes in the FIFO (buffer drained) |
| `AsyncRead` | Up to your buffer length; remainder stays in the FIFO |

Use one read style per port. Mixing `Stream` and `AsyncRead` can split messages across calls.

[`AsyncReadExt`](https://docs.rs/futures/latest/futures/io/trait.AsyncReadExt.html) is re-exported (`read`, `read_to_end`, etc.). Example: `cargo run --example tokio_async_read -- /dev/ttyUSB0 115200`.

For `tokio::io::AsyncRead`, bridge with [`tokio_util::compat`](https://docs.rs/tokio-util/latest/tokio_util/compat/index.html) (`tokio-util` feature `compat`).

### Writing

[`AsyncWrite`](https://docs.rs/futures/latest/futures/io/trait.AsyncWrite.html) / [`AsyncWriteExt`](https://docs.rs/futures/latest/futures/io/trait.AsyncWriteExt.html) are re-exported. The first write starts a background thread.

```rust
use serialport_stream::{new, AsyncWriteExt, TryStreamExt};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut stream = new("/dev/ttyUSB0", 115200).open()?;

    stream.write_all(b"PING\r\n").await?;
    stream.flush().await?;

    if let Some(reply) = stream.try_next().await? {
        println!("Received: {reply:?}");
    }

    Ok(())
}
```

Example: `cargo run --example tokio_async_rw -- /dev/ttyUSB0 115200`.

For `tokio::io::AsyncWrite`, use [`tokio_util::compat`](https://docs.rs/tokio-util/latest/tokio_util/compat/index.html) as above.

## Builder

Open with [`new(path, baud_rate)`](https://docs.rs/serialport-stream/latest/serialport_stream/fn.new.html), then chain options and call `.open()`:

- `.data_bits`, `.parity`, `.stop_bits`, `.flow_control` — default is 8N1, no flow control
- `.dtr_on_open(bool)` — drive DTR on open: `true` asserts, `false` clears (default: `false`)
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
