//! Async read/write.
//!
//! Run:
//! ```text
//! cargo run --example tokio_async_rw -- /dev/ttyUSB0 115200
//! cargo run --example tokio_async_rw -- /dev/ttyUSB0 115200 --trace
//! ```

use anyhow::Result;
use clap::{Arg, Command};
use serialport_stream::{new, AsyncWriteExt, TryStreamExt};
use tokio::signal::ctrl_c;

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

    let mut stream = new(port_name, baud_rate).dtr_on_open(true).open()?;

    let ctrl_c = ctrl_c();
    tokio::pin!(ctrl_c);

    println!("Read / write with Async + Stream");
    println!("--------------------------------------------------------------------------------");

    loop {
        tokio::select! {
            biased;

            _ = &mut ctrl_c => {
                println!("\nCtrl+C — exiting");
                break;
            }

            res = async {
                stream.write_all(WRITE_PAYLOAD).await?;
                stream.flush().await?;
                println!("payload {:02X?} written and flushed", WRITE_PAYLOAD);
                if let Some(out) = stream.try_next().await? {
                    println!("received {} bytes: {:02X?}", out.len(), out);
                }
                Ok::<(), anyhow::Error>(())
            } => {
                if let Err(e) = res {
                    eprintln!("read/write error: {e}");
                    break;
                }
            }
        }
    }

    Ok(())
}
