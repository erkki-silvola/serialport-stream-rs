# serialport-stream-rs

A runtime-agnostic async stream wrapper for [serialport-rs](https://github.com/serialport/serialport-rs) that implements `futures::Stream` using platform-specific I/O mechanisms.

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
serialport-stream = "0.1"
futures-lite = "2.0"
```

## Usage

### Basic Streaming

```rust
use serialport_stream::{new, SerialPortStream};
use futures_lite::stream;

fn read_serial() -> std::io::Result<()> {
    // Create a serial port stream using the builder API
    let stream = serialport_stream::new("COM3", 115200)
        .timeout(std::time::Duration::from_secs(1))
        .dtr_on_open(true)
        .open()?;

    for event in stream::block_on(stream) {
        let bytes = event?;
        println!("bytes {bytes:?}");
    }

    Ok(())
}
```

### Synchronous Read/Write

```rust
use std::io::{Read, Write};

let mut stream = new("/dev/ttyUSB0", 9600).open()?;

// Write data synchronously
stream.write_all(b"Hello, serial port!\n")?;
stream.flush()?;

// Read data synchronously
let mut buffer = [0u8; 1024];
let n = stream.read(&mut buffer)?;
println!("Read {} bytes", n);
```

## API Overview

### Builder Functions

- `new(path, baud_rate)` - Create a new builder
- `.data_bits(DataBits)` - Set data bits (5, 6, 7, 8)
- `.flow_control(FlowControl)` - Set flow control (None, Software, Hardware)
- `.parity(Parity)` - Set parity (None, Odd, Even)
- `.stop_bits(StopBits)` - Set stop bits (One, Two)
- `.timeout(Duration)` - Set read/write timeout
- `.dtr_on_open(bool)` - Control DTR signal on open
- `.open()` - Open the port and create the stream

### SerialPortStream Methods

- Implements `std::io::Read` - Synchronous reading
- Implements `std::io::Write` - Synchronous writing
- Implements `futures::Stream` - Asynchronous streaming
- Item type: `Result<Vec<u8>, std::io::Error>`

## License

This project is licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## Acknowledgments

This crate builds upon [serialport-rs](https://github.com/serialport/serialport-rs) for cross-platform serial port access.
