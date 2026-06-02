//! Async writes using [`futures::io::AsyncWrite`] with Tokio (`#[tokio::main]`).
//!
//! Multiplexes Ctrl+C, incoming stream packets (`try_next`), and periodic writes in one loop.
//!
//! Run:
//! ```text
//! cargo run --example tokio_async_write -- /dev/ttyUSB0 115200
//! cargo run --example tokio_async_write -- /dev/ttyUSB0 115200 --trace
//! ```

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use clap::{Arg, Command};
use serialport_stream::{new, AsyncWriteExt, TryStreamExt};
use tokio::signal::ctrl_c;
use tokio::sync::Mutex;
use tokio::time;

const WRITE_PAYLOAD: &[u8] = &[0x0a, 0xC0];

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("Serialport AsyncWrite example (Tokio)")
        .about("Write to the serial port with futures AsyncWrite under Tokio")
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

    let stream = Arc::new(Mutex::new(
        new(port_name, baud_rate).dtr_on_open(true).open()?,
    ));

    println!("Read / write with AsyncWrite + Stream + Tokio (Ctrl+C to stop)");
    println!("--------------------------------------------------------------------------------");

    let ctrl_c = ctrl_c();
    tokio::pin!(ctrl_c);

    let mut write_interval = time::interval(Duration::from_secs(1));
    write_interval.tick().await;

    loop {
        tokio::select! {
            biased;

            _ = &mut ctrl_c => {
                println!("\nCtrl+C — exiting");
                break;
            }

            res = async {
                stream.lock().await.try_next().await
            } => {
                match res {
                    Ok(Some(data)) => println!("received {} bytes: {:02X?}", data.len(), data),
                    Ok(None) => {
                        println!("stream ended");
                        break;
                    }
                    Err(e) => {
                        eprintln!("read error: {e}");
                        break;
                    }
                }
            }

            _ = write_interval.tick() => {
                match stream.lock().await.write(WRITE_PAYLOAD).await {
                    Ok(n) => println!("wrote {n} bytes"),
                    Err(e) => {
                        eprintln!("write error: {e}");
                        break;
                    }
                }
            }
        }
    }

    Ok(())
}
