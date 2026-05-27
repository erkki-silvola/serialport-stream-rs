//! Async byte reads using [`futures::io::AsyncRead`] with Tokio (`#[tokio::main]`).
//!
//! Run:
//! ```text
//! cargo run --example tokio_async_read -- /dev/ttyUSB0 115200
//! cargo run --example tokio_async_read -- /dev/ttyUSB0 115200 --trace
//! ```

use anyhow::Result;
use clap::{Arg, Command};
use serialport_stream::{new, AsyncReadExt};
use tokio::signal::ctrl_c;

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("Serialport AsyncRead example (Tokio)")
        .about("Read the serial port with futures AsyncRead under Tokio")
        .disable_version_flag(true)
        .arg(
            Arg::new("port")
                .help("Device path (e.g. /dev/ttyUSB0 or COM3)")
                .use_value_delimiter(false)
                .required(true),
        )
        .arg(Arg::new("baud").use_value_delimiter(false).required(true))
        .arg(
            Arg::new("trace")
                .long("trace")
                .help("Enable tracing INFO logs to stdout")
                .takes_value(false),
        )
        .get_matches();

    let port_name = matches.value_of("port").unwrap();
    let baud_rate = matches.value_of("baud").unwrap().parse::<u32>().unwrap();
    let enable_trace = matches.is_present("trace");

    if enable_trace {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::io::stdout)
            .with_max_level(tracing::Level::INFO)
            .with_target(false)
            .try_init();
        println!("Tracing enabled (INFO -> stdout)");
    }

    println!("Opening {} @ {} baud", port_name, baud_rate);

    let mut stream = new(port_name, baud_rate).dtr_on_open(true).open()?;

    println!("Reading with AsyncRead + Tokio (Ctrl+C to stop)");
    println!("--------------------------------------------------------------------------------");

    let ctrl_c = ctrl_c();
    tokio::pin!(ctrl_c);

    let mut buf = [0u8; 512];

    loop {
        tokio::select! {
            biased;

            _ = &mut ctrl_c => {
                println!("\nCtrl+C — exiting");
                break;
            }

            res = stream.read(&mut buf) => {
                match res {
                    Ok(n) =>  println!("read {} bytes: {:?}", n, &buf[..n]),
                    Err(e) => {
                        eprintln!("read error: {e}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
