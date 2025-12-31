use std::thread::JoinHandle;

use clap::{Arg, Command};
use futures::stream::AbortHandle;
use futures::stream::Abortable;
use futures_lite::stream;

fn main() -> anyhow::Result<()> {
    let matches = Command::new("Serialport Example - Receive Data")
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

    let stream = serialport_stream::new(port_name, baud_rate)
        .timeout(std::time::Duration::from_secs(1))
        .dtr_on_open(true)
        .open()?;

    let (abort_handle, abort_registration) = AbortHandle::new_pair();
    let stream = Abortable::new(stream, abort_registration);

    ctrlc::set_handler(move || {
        println!("ctrlc abort stream");
        abort_handle.abort();
    })
    .expect("Error setting Ctrl-C handler");

    let handle: JoinHandle<anyhow::Result<()>> = std::thread::spawn(move || {
        let mut total = 0;
        for event in stream::block_on(stream) {
            let bytes = event?;
            total += bytes.len();
            println!("bytes len {} total {total}", bytes.len());
        }

        Ok(())
    });

    handle.join().unwrap()?;

    Ok(())
}
