# serialport-stream-rs

Pure event driven implementation of futures::Stream for reading data from serialport utilizing [serialport-rs](https://github.com/serialport/serialport-rs).
Produces 1-N amount of bytes depending on polling interval. Initial poll starts background thread which will indefinitely wait for data in event, error or drop. Incoming data is only available via `futures::Stream` or `futures::io::AsyncRead` (no `std::io::Read` on this stream). To transmit payloads, open a separate handle with `serialport` or split your design accordingly. There is no backpressure handling; the crate will buffer incoming data indefinitely.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
serialport-stream = "0.2.0"
futures-lite = "2.0"
```

## Usage

### Basic Usage

```rust
use serialport_stream::{new, SerialPortStream};
use futures_lite::stream;

fn read_serial() -> std::io::Result<()> {
    // Create a serial port stream using the builder API
    let stream = new("COM3", 115200)
        .dtr_on_open(true)
        .open()?;

    for event in stream::block_on(stream) {
        let bytes = event?;
        println!("bytes {bytes:?}");
    }

    Ok(())
}
```

### Using with Tokio

```rust
use serialport_stream::new;
use futures_lite::stream::StreamExt;

#[tokio::main]
async fn main() -> std::io::Result<()> {
     let mut stream = new("/dev/ttyUSB0", 9600)
        .open()?;

    while let Ok(Some(result)) = stream.try_next().await {
        println!("Received: {:?}", result);
    }

    Ok(())
}
```

`AsyncRead` works the same way: `cargo run --example tokio_async_read -- /dev/ttyUSB0 115200` ([`examples/tokio_async_read.rs`](examples/tokio_async_read.rs)).

### Asynchronous byte reads (`AsyncRead`)

The stream implements [`futures::io::AsyncRead`](https://docs.rs/futures/latest/futures/io/trait.AsyncRead.html). Combine it with [`AsyncReadExt`](https://docs.rs/futures/latest/futures/io/trait.AsyncReadExt.html) for helpers such as `read` and `read_to_end`. Data comes from the same internal buffer as `futures::Stream`; use one primary read style per open stream.

For Tokio’s `tokio::io::AsyncRead`, bridge via [`tokio_util::compat`](https://docs.rs/tokio-util/latest/tokio_util/compat/index.html) (add `tokio-util` with the `compat` feature to your crate).

## API Overview

### Builder Functions

- `new(path, baud_rate)` - Create a new builder
- `.data_bits(DataBits)` - Set data bits (5, 6, 7, 8)
- `.flow_control(FlowControl)` - Set flow control (None, Software, Hardware)
- `.parity(Parity)` - Set parity (None, Odd, Even)
- `.stop_bits(StopBits)` - Set stop bits (One, Two)
- `.dtr_on_open(bool)` - Control DTR signal on open
- `.open()` - Open the port and create the stream

### SerialPortStream Methods

- Implements `futures::Stream` — asynchronous streaming (`Result<Vec<u8>, io::Error>` items)
- Implements `futures::io::AsyncRead` — partial reads from the same receive FIFO; re-exported with `AsyncReadExt`
- Other methods expose serial control lines (`write_request_to_send`, modem status reads, buffers, breaks, etc.)

## License

This project is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Acknowledgments

This crate builds upon [serialport-rs](https://github.com/serialport/serialport-rs) for cross-platform serial port access.
