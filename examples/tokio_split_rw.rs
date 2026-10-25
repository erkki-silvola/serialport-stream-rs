//! Concurrent async read/write using split reader and writer halves.
//!
//! With `stream`, the read task uses [`TryStreamExt::try_next`] on the reader half.
//!
//! Run:
//! ```text
//! cargo run --example tokio_split_rw -- /dev/ttyUSB0 115200
//! cargo run --example tokio_split_rw --features stream -- /dev/ttyUSB0 115200
//! cargo run --example tokio_split_rw --features tracing -- /dev/ttyUSB0 115200 --trace
//! ```

use anyhow::Result;
use clap::{Arg, Command};
use serialport_stream::{new, AsyncWriteExt};
#[cfg(not(feature = "stream"))]
use serialport_stream::AsyncReadExt;
#[cfg(feature = "stream")]
use serialport_stream::TryStreamExt;
use tokio::signal::ctrl_c;

const WRITE_PAYLOAD: &[u8] = &[0x0a, 0xC0];

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("Serialport split read/write example (Tokio)")
        .about("Concurrent read and AsyncWrite on split halves")
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
    #[cfg(feature = "stream")]
    println!("Read path: Stream try_next on reader half");
    #[cfg(not(feature = "stream"))]
    println!("Read path: AsyncRead on reader half");

    let stream = new(port_name, baud_rate).dtr_on_open(true).open()?;
    let (mut reader, mut writer) = stream.split();

    let read_task = tokio::spawn(async move {
        loop {
            #[cfg(not(feature = "stream"))]
            {
                let mut buf = [0u8; 256];
                match reader.read(&mut buf).await {
                    Ok(len) => println!("received {len}"),
                    Err(e) => {
                        eprintln!("read error: {e}");
                        break;
                    }
                }
            }
            #[cfg(feature = "stream")]
            {
                match reader.try_next().await {
                    Ok(Some(bytes)) => println!("received {}", bytes.len()),
                    Ok(None) => panic!("unexpected output"),
                    Err(e) => {
                        eprintln!("read error: {e}");
                        break;
                    }
                }
            }
        }
    });

    let write_task = tokio::spawn(async move {
        loop {
            if let Err(e) = writer.write_all(WRITE_PAYLOAD).await {
                eprintln!("write error: {e}");
                break;
            }
            if let Err(e) = writer.flush().await {
                eprintln!("flush error: {e}");
                break;
            }
            println!("Written and flushed");
        }
    });

    let ctrl_c = ctrl_c();
    tokio::pin!(ctrl_c);

    println!("Split read/write - Ctrl+C to exit ");

    tokio::select! {
        biased;

        _ = &mut ctrl_c => {
            println!("ctrl-c");
        }
        _ = read_task => {}
        _ = write_task => {}
    }

    Ok(())
}
