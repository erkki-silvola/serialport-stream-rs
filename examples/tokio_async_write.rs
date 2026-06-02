//! Async writes using [`futures::io::AsyncWrite`] with Tokio (`#[tokio::main]`).
//!
//! Run:
//! ```text
//! cargo run --example tokio_async_write -- /dev/ttyUSB0 115200
//! cargo run --example tokio_async_write -- /dev/ttyUSB0 115200 --trace
//! ```

use anyhow::Result;
use clap::{Arg, Command};
use serialport_stream::{new, AsyncWriteExt};
use tokio::signal::ctrl_c;

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

    println!("Writing with AsyncWrite + Tokio (Ctrl+C to stop)");
    println!("--------------------------------------------------------------------------------");

    let ctrl_c = ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            biased;

            _ = &mut ctrl_c => {
                println!("\nCtrl+C — exiting");
                break;
            }

            res = async {
                let n = stream.write(&[0xC0]).await?;
                Ok::<_, std::io::Error>(n)
            } => {
                match res {
                    Ok(n) => {
                        println!("wrote {n} bytes");
                        assert_eq!(n,1);
                    }
                    Err(e) => {
                        eprintln!("write error: {e}");
                        break;
                    }
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    Ok(())
}
