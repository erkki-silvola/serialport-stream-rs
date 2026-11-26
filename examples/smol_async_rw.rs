//! Async read/write (`AsyncRead` + `AsyncWrite`) on the smol runtime.
//!
//! Run:
//! ```text
//! cargo run --example smol_async_rw -- /dev/ttyUSB0 115200
//! cargo run --example smol_async_rw --features tracing -- /dev/ttyUSB0 115200 --trace
//! ```

use anyhow::Result;
use clap::{Arg, Command};
use serialport_stream::{new, AsyncReadExt, AsyncWriteExt};
use smol::future::FutureExt;

const WRITE_PAYLOAD: &[u8] = &[0x0a, 0xC0];

enum Event {
    CtrlC,
    Io(Result<()>),
}

fn main() -> Result<()> {
    let matches = Command::new("Serialport AsyncWrite example (smol)")
        .about("Write to the serial port with futures AsyncWrite under smol")
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

    let (ctrl_c_tx, ctrl_c_rx) = smol::channel::bounded::<()>(1);
    ctrlc::set_handler(move || {
        let _ = ctrl_c_tx.try_send(());
    })?;

    smol::block_on(async {
        println!("Opening {} @ {} baud", port_name, baud_rate);

        let mut stream = new(port_name, baud_rate).dtr_on_open(true).open()?;

        println!("Read / write with AsyncRead + AsyncWrite");
        println!(
            "--------------------------------------------------------------------------------"
        );

        loop {
            let ctrl_c = async {
                let _ = ctrl_c_rx.recv().await;
                Event::CtrlC
            };
            let io = async {
                stream.write_all(WRITE_PAYLOAD).await?;
                stream.flush().await?;
                println!("payload {:02X?} written and flushed", WRITE_PAYLOAD);
                let mut buf = [0u8; 256];
                let len = stream.read(&mut buf).await?;
                println!("received {len} bytes: {:02X?}", &buf[..len]);

                Ok::<(), anyhow::Error>(())
            };

            match ctrl_c.or(async { Event::Io(io.await) }).await {
                Event::CtrlC => {
                    println!("\nCtrl+C — exiting");
                    break;
                }
                Event::Io(Err(e)) => {
                    eprintln!("read/write error: {e}");
                    break;
                }
                Event::Io(Ok(())) => {}
            }
        }

        Ok(())
    })
}
