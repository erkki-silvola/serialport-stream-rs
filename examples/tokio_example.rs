use anyhow::Result;
use clap::{Arg, Command};
use futures_lite::stream::StreamExt;
use serialport_stream::new;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    let matches = Command::new("Serialport stream example using tokio")
        .disable_version_flag(true)
        .arg(
            Arg::new("port")
                .help("The device path to a serial port")
                .use_value_delimiter(false)
                .required(true),
        )
        .arg(Arg::new("baud").use_value_delimiter(false).required(true))
        .get_matches();

    let port_name = matches.value_of("port").unwrap();
    let baud_rate = matches.value_of("baud").unwrap().parse::<u32>().unwrap();

    println!("Opening serial port {} at {} baud", port_name, baud_rate);

    // Create serial port stream with tokio runtime
    let mut stream = new(port_name, baud_rate)
        .timeout(Duration::from_millis(100))
        .dtr_on_open(true)
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
            result = stream.try_next() => {
                match result {
                    Ok(Some(data)) => {
                        println!("Received {} bytes", data.len());
                    }
                    Ok(None) => {
                        println!("Stream ended");
                        break;
                    }
                    Err(e) => {
                        eprintln!("poll next error: {}", e);
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

    Ok(())
}
