use anyhow::Result;
use clap::Parser;
use futures_lite::stream::StreamExt;
use serialport_stream::new;
use std::time::Duration;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Serial port path (e.g., /dev/ttyUSB0 or COM3)
    #[clap(short, long)]
    port: String,

    /// Baud rate
    #[clap(short, long, default_value = "115200")]
    baud: u32,

    /// Timeout in milliseconds
    #[clap(short, long, default_value = "1000")]
    timeout: u64,

    /// Enable DTR on open
    #[clap(short, long)]
    dtr: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    println!("Opening serial port {} at {} baud", args.port, args.baud);

    // Create serial port stream with tokio runtime
    let mut stream = new(&args.port, args.baud)
        .timeout(Duration::from_millis(args.timeout))
        .dtr_on_open(args.dtr)
        .open()?;

    println!("Serial port opened successfully");

    // Set up Ctrl+C handler
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    println!("Stream from serial port... (Press Ctrl+C to stop)");
    println!("----------------------------------------");

    // Read data asynchronously using tokio
    loop {
        tokio::select! {
            // Handle incoming data
            result = stream.next() => {
                match result {
                    Some(Ok(data)) => {
                        println!("Received {} bytes", data.len());
                    }
                    Some(Err(e)) => {
                        eprintln!("poll next error: {}", e);
                        break;
                    }
                    None => {
                        println!("Stream ended");
                        break;
                    }
                }
            }
            // Handle Ctrl+C
            _ = &mut ctrl_c => {
                println!("\nCtrl+C received, closing...");
                break;
            }
        }
    }

    println!("Closing serial port");
    Ok(())
}
