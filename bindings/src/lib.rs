use std::future::Future;
use std::sync::Arc;

use napi::bindgen_prelude::{spawn, Buffer};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Error, Result};
use napi_derive::napi;
use serialport_stream::{
    AsyncReadExt, AsyncWriteExt, DataBits, SerialPortReader, SerialPortWriter, StopBits,
    TryStreamExt,
};
use tokio::sync::{oneshot, watch, Mutex};

const DEFAULT_READ_LEN: u32 = 256;

#[napi(string_enum = "lowercase")]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[napi(string_enum = "lowercase")]
pub enum FlowControl {
    None,
    Software,
    Hardware,
}

#[napi(string_enum = "lowercase")]
pub enum ClearBuffer {
    Input,
    Output,
    All,
}

#[napi(object)]
pub struct OpenOptions {
    #[napi(ts_type = "5 | 6 | 7 | 8")]
    pub data_bits: Option<u32>,
    #[napi(ts_type = "'none' | 'odd' | 'even'")]
    pub parity: Option<Parity>,
    #[napi(ts_type = "1 | 2")]
    pub stop_bits: Option<u32>,
    #[napi(ts_type = "'none' | 'software' | 'hardware'")]
    pub flow_control: Option<FlowControl>,
    pub dtr_on_open: Option<bool>,
    #[napi(ts_type = "'input' | 'output' | 'all'")]
    pub clear: Option<ClearBuffer>,
}

fn io_err(e: std::io::Error) -> Error {
    Error::from_reason(e.to_string())
}

fn closed_err() -> Error {
    Error::from_reason("port closed")
}

fn bits<T: TryFrom<u8>>(value: u32, name: &str) -> Result<T> {
    u8::try_from(value)
        .ok()
        .and_then(|v| T::try_from(v).ok())
        .ok_or_else(|| Error::from_reason(format!("invalid {name}: {value}")))
}

/// Opens a serial port, e.g. `open("/dev/ttyUSB0", 115200)` or `open("COM3", 9600, { parity: "even" })`.
#[napi]
pub fn open(path: String, baud_rate: u32, options: Option<OpenOptions>) -> Result<SerialPort> {
    let mut builder = serialport_stream::new(path, baud_rate);
    if let Some(o) = options {
        if let Some(v) = o.data_bits {
            builder = builder.data_bits(bits::<DataBits>(v, "dataBits")?);
        }
        if let Some(v) = o.stop_bits {
            builder = builder.stop_bits(bits::<StopBits>(v, "stopBits")?);
        }
        if let Some(v) = o.parity {
            builder = builder.parity(match v {
                Parity::None => serialport_stream::Parity::None,
                Parity::Odd => serialport_stream::Parity::Odd,
                Parity::Even => serialport_stream::Parity::Even,
            });
        }
        if let Some(v) = o.flow_control {
            builder = builder.flow_control(match v {
                FlowControl::None => serialport_stream::FlowControl::None,
                FlowControl::Software => serialport_stream::FlowControl::Software,
                FlowControl::Hardware => serialport_stream::FlowControl::Hardware,
            });
        }
        if let Some(v) = o.dtr_on_open {
            builder = builder.dtr_on_open(v);
        }
        if let Some(v) = o.clear {
            builder = builder.clear(match v {
                ClearBuffer::Input => serialport_stream::ClearBuffer::Input,
                ClearBuffer::Output => serialport_stream::ClearBuffer::Output,
                ClearBuffer::All => serialport_stream::ClearBuffer::All,
            });
        }
    }
    let (reader, writer, _settings) = builder.open().map_err(io_err)?.split();
    Ok(SerialPort {
        reader: Arc::new(Mutex::new(Some(reader))),
        writer: Arc::new(Mutex::new(Some(writer))),
        closed: watch::channel(false).0,
        data_stop: std::sync::Mutex::new(None),
    })
}

/// Native port handle; `wrapper.js` exposes it as an EventEmitter-based `SerialPort`.
#[napi]
pub struct SerialPort {
    reader: Arc<Mutex<Option<SerialPortReader>>>,
    writer: Arc<Mutex<Option<SerialPortWriter>>>,
    closed: watch::Sender<bool>,
    data_stop: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

impl SerialPort {
    fn data_stop(&self) -> Result<std::sync::MutexGuard<'_, Option<oneshot::Sender<()>>>> {
        self.data_stop
            .lock()
            .map_err(|_| Error::from_reason("data lock poisoned"))
    }
}

async fn until_closed<T>(
    mut closed: watch::Receiver<bool>,
    op: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        r = op => r,
        _ = closed.wait_for(|c| *c) => Err(closed_err()),
    }
}

#[napi]
impl SerialPort {
    /// Writes all bytes and waits until they are transmitted.
    #[napi]
    pub async fn write(&self, data: Buffer) -> Result<()> {
        let writer = self.writer.clone();
        until_closed(self.closed.subscribe(), async move {
            let mut guard = writer.lock().await;
            let w = guard.as_mut().ok_or_else(closed_err)?;
            w.write_all(&data).await.map_err(io_err)?;
            w.flush().await.map_err(io_err)
        })
        .await
    }

    /// Resolves with the next received bytes, at most `maxLen` (default 256).
    #[napi]
    pub async fn read(&self, max_len: Option<u32>) -> Result<Buffer> {
        if self.data_stop()?.is_some() {
            return Err(Error::from_reason("port has 'data' listeners"));
        }
        let len = max_len.unwrap_or(DEFAULT_READ_LEN);
        if len == 0 {
            return Err(Error::from_reason("maxLen must be greater than 0"));
        }
        let reader = self.reader.clone();
        until_closed(self.closed.subscribe(), async move {
            let mut guard = reader.lock().await;
            let r = guard.as_mut().ok_or_else(closed_err)?;
            let mut buf = vec![0u8; len as usize];
            let n = r.read(&mut buf).await.map_err(io_err)?;
            buf.truncate(n);
            Ok(buf.into())
        })
        .await
    }

    /// Starts the receive loop, calling `callback(err, data)` per chunk until stopped, closed, or failed.
    #[napi(ts_args_type = "callback: (err: Error | null, data: Buffer) => void")]
    pub fn start_data(&self, callback: ThreadsafeFunction<Buffer>) -> Result<()> {
        if *self.closed.borrow() {
            return Err(closed_err());
        }
        let mut data_stop = self.data_stop()?;
        if data_stop.is_some() {
            return Err(Error::from_reason("receive loop already running"));
        }
        let (stop_tx, mut stop_rx) = oneshot::channel();
        *data_stop = Some(stop_tx);
        let reader = self.reader.clone();
        let mut closed = self.closed.subscribe();
        spawn(async move {
            let mut guard = reader.lock().await;
            let Some(r) = guard.as_mut() else { return };
            loop {
                let item = tokio::select! {
                    item = r.try_next() => item,
                    _ = closed.wait_for(|c| *c) => return,
                    _ = &mut stop_rx => return,
                };
                match item {
                    Ok(Some(data)) => {
                        callback.call(Ok(data.into()), ThreadsafeFunctionCallMode::NonBlocking);
                    }
                    Err(e) => {
                        callback.call(Err(io_err(e)), ThreadsafeFunctionCallMode::NonBlocking);
                        return;
                    }
                    Ok(None) => return,
                }
            }
        });
        Ok(())
    }

    /// Stops the receive loop; unread bytes stay buffered for `read()` or a later `startData`.
    #[napi]
    pub fn stop_data(&self) -> Result<()> {
        if let Some(stop) = self.data_stop()?.take() {
            let _ = stop.send(());
        }
        Ok(())
    }

    /// Stops pending operations and closes the port. Further calls reject with "port closed".
    #[napi]
    pub async fn close(&self) -> Result<()> {
        self.closed.send_replace(true);
        self.stop_data()?;
        drop(self.reader.lock().await.take());
        drop(self.writer.lock().await.take());
        Ok(())
    }
}
