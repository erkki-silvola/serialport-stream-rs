//! # serialport-stream
//!
//! Pure event driven implementation of futures::Stream for reading data from serialport utilizing [serialport-rs](https://github.com/serialport/serialport-rs).
//! Produces 1-N amount of bytes depending on polling interval. Initial poll starts background thread which will indefinitely wait for data in event, error or drop.
//! Bytes are read only via `futures::Stream` or `futures::io::AsyncRead`, not `std::io::Read`.
//!

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

mod platform;

pub mod line_settings;

use crate::platform::PlatformStream;
use futures::task::AtomicWaker;

pub use futures::io::{AsyncRead, AsyncReadExt};
pub use futures::stream::{Stream, TryStreamExt};
pub use serialport;
use serialport::{DataBits, FlowControl, Parity, StopBits};

#[derive(Debug)]
pub(crate) struct EventsInner {
    pub(crate) in_buffer: Mutex<Vec<u8>>,
    pub(crate) stream_error: Mutex<Option<std::io::Error>>,
    pub(crate) waker: AtomicWaker,
}

impl EventsInner {
    pub(crate) fn new() -> Self {
        Self {
            in_buffer: Mutex::new(Vec::new()),
            stream_error: Mutex::new(None),
            waker: AtomicWaker::new(),
        }
    }
}

/// Builder for configuring and opening a serial port stream.
///
/// Use the [`new()`] function to create a builder, then chain configuration
/// methods before calling [`open()`](SerialPortStreamBuilder::open).
///
/// # Example
///
/// ```no_run
/// use serialport_stream::new;
/// use serialport::{DataBits, Parity, StopBits, FlowControl};
///
/// # fn example() -> std::io::Result<()> {
/// let stream = new("/dev/ttyUSB0", 115200)
///     .data_bits(DataBits::Eight)
///     .parity(Parity::None)
///     .stop_bits(StopBits::One)
///     .flow_control(FlowControl::None)
///     .open()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialPortStreamBuilder {
    pub(crate) path: String,
    pub(crate) baud_rate: u32,
    pub(crate) data_bits: DataBits,
    pub(crate) flow_control: FlowControl,
    pub(crate) parity: Parity,
    pub(crate) stop_bits: StopBits,
    pub(crate) dtr_on_open: Option<bool>,
}

impl SerialPortStreamBuilder {
    /// Sets the path to the serial port device.
    ///
    /// # Examples
    /// - Unix: `"/dev/ttyUSB0"`, `"/dev/ttyACM0"`
    /// - Windows: `"COM3"`, `"COM10"`
    #[allow(clippy::assigning_clones)]
    #[must_use]
    pub fn path<'a>(mut self, path: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        self.path = path.into().as_ref().to_owned();
        self
    }

    /// Sets the baud rate (bits per second).
    ///
    /// Common values: 9600, 19200, 38400, 57600, 115200
    #[must_use]
    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = baud_rate;
        self
    }

    /// Sets the number of data bits per character.
    ///
    /// Default: `DataBits::Eight`
    #[must_use]
    pub fn data_bits(mut self, data_bits: DataBits) -> Self {
        self.data_bits = data_bits;
        self
    }

    /// Sets the flow control mode.
    ///
    /// Default: `FlowControl::None`
    #[must_use]
    pub fn flow_control(mut self, flow_control: FlowControl) -> Self {
        self.flow_control = flow_control;
        self
    }

    /// Sets the parity checking mode.
    ///
    /// Default: `Parity::None`
    #[must_use]
    pub fn parity(mut self, parity: Parity) -> Self {
        self.parity = parity;
        self
    }

    /// Sets the number of stop bits.
    ///
    /// Default: `StopBits::One`
    #[must_use]
    pub fn stop_bits(mut self, stop_bits: StopBits) -> Self {
        self.stop_bits = stop_bits;
        self
    }

    /// Sets the DTR (Data Terminal Ready) signal state when opening the port.
    ///
    /// If not called, the DTR state is preserved from the previous port state.
    #[must_use]
    pub fn dtr_on_open(mut self, state: bool) -> Self {
        self.dtr_on_open = Some(state);
        self
    }

    /// Preserves the current DTR state when opening the port.
    ///
    /// This is the default behavior.
    #[must_use]
    pub fn preserve_dtr_on_open(mut self) -> Self {
        self.dtr_on_open = None;
        self
    }

    /// Opens the serial port and creates the stream.
    ///
    pub fn open(self) -> std::io::Result<SerialPortStream> {
        let inner = Arc::new(EventsInner::new());
        Ok(SerialPortStream {
            platform: PlatformStream::new(self, inner.clone())?,
            inner,
        })
    }
}

/// Creates a new serial port stream builder.
///
/// This is the main entry point for creating a serial port stream. After creating
/// the builder, you can chain configuration methods and call `.open()` to create
/// the stream.
///
pub fn new<'a>(
    path: impl Into<std::borrow::Cow<'a, str>>,
    baud_rate: u32,
) -> SerialPortStreamBuilder {
    SerialPortStreamBuilder {
        path: path.into().into_owned(),
        baud_rate,
        data_bits: DataBits::Eight,
        flow_control: FlowControl::None,
        parity: Parity::None,
        stop_bits: StopBits::One,
        dtr_on_open: None,
    }
}

/// An async stream for reading from a serial port.
///
/// This struct exposes asynchronous ingress and synchronous control lines on the port:
/// - Implements `futures::Stream` for asynchronous streaming of incoming data
/// - Implements `futures::io::AsyncRead` for byte-oriented async reads from the same receive buffer
///
/// `Stream` and `AsyncRead` both consume the same FIFO; pick one primary mode for a given stream.
///
#[derive(Debug)]
pub struct SerialPortStream {
    platform: PlatformStream,
    inner: Arc<EventsInner>,
}

impl SerialPortStream {
    pub fn clear(&mut self, buffer_to_clear: serialport::ClearBuffer) -> std::io::Result<()> {
        self.platform.clear(buffer_to_clear)
    }

    pub fn set_break(&mut self) -> std::io::Result<()> {
        self.platform.set_break()
    }

    pub fn clear_break(&mut self) -> std::io::Result<()> {
        self.platform.clear_break()
    }

    pub fn write_request_to_send(&mut self, level: bool) -> std::io::Result<()> {
        self.platform.write_request_to_send(level)
    }

    pub fn write_data_terminal_ready(&mut self, level: bool) -> std::io::Result<()> {
        self.platform.write_data_terminal_ready(level)
    }

    pub fn read_clear_to_send(&mut self) -> std::io::Result<bool> {
        self.platform.read_clear_to_send()
    }

    pub fn read_data_set_ready(&mut self) -> std::io::Result<bool> {
        self.platform.read_data_set_ready()
    }

    pub fn read_ring_indicator(&mut self) -> std::io::Result<bool> {
        self.platform.read_ring_indicator()
    }

    pub fn read_carrier_detect(&mut self) -> std::io::Result<bool> {
        self.platform.read_carrier_detect()
    }

    pub fn bytes_to_read(&self) -> std::io::Result<u32> {
        self.platform.bytes_to_read()
    }

    fn poll_receiver_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.inner.waker.register(cx.waker());

        if let Some(err) = self.inner.stream_error.lock().unwrap().as_ref() {
            return Poll::Ready(Err(std::io::Error::new(err.kind(), err.to_string())));
        }

        if !self.platform.is_thread_started() {
            self.platform.start_thread();
            return Poll::Pending;
        }

        Poll::Ready(Ok(()))
    }

    pub fn try_poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Vec<u8>, std::io::Error>>> {
        match self.poll_receiver_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let mut buffer = self.inner.in_buffer.lock().unwrap();
                if !buffer.is_empty() {
                    // Drain all available data
                    let data = buffer.drain(..).collect();
                    return Poll::Ready(Some(Ok(data)));
                }

                Poll::Pending
            }
        }
    }
}

unsafe impl Send for SerialPortStream {}

unsafe impl Sync for SerialPortStream {}

impl AsyncRead for SerialPortStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        let this = self.as_mut().get_mut();
        match this.poll_receiver_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                let mut buffer = this.inner.in_buffer.lock().unwrap();
                if buffer.is_empty() {
                    return Poll::Pending;
                }
                let n = buf.len().min(buffer.len());
                buf[..n].copy_from_slice(&buffer[..n]);
                buffer.drain(..n);
                let cached_bytes = buffer.len();
                if cached_bytes > 0 {
                    tracing::info!(
                        read_bytes = n,
                        cached_bytes,
                        "serialport-stream receive buffer after AsyncRead read"
                    );
                }
                Poll::Ready(Ok(n))
            }
        }
    }
}

impl Stream for SerialPortStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.try_poll_next(cx)
    }
}
