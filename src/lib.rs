//! # serialport-stream
//!
//! A runtime-agnostic async stream wrapper for `serialport-rs` that provides efficient
//! asynchronous serial port using platform-specific I/O  mechanism. Produces 1-N amount of bytes depending on polling interval.
//! Poll next will indefinitely wait for data, cancel or drop.
//!

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

mod platform;

use crate::platform::PlatformStream;
use futures::task::AtomicWaker;

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
/// use std::time::Duration;
///
/// # fn example() -> std::io::Result<()> {
/// let stream = new("/dev/ttyUSB0", 115200)
///     .data_bits(DataBits::Eight)
///     .parity(Parity::None)
///     .stop_bits(StopBits::One)
///     .flow_control(FlowControl::None)
///     .timeout(Duration::from_millis(100))
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
    pub(crate) timeout: Duration,
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

    /// Sets the timeout for read and write operations.
    ///
    /// Default: `Duration::from_millis(0)` (non-blocking)
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
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
        timeout: Duration::from_millis(0),
        dtr_on_open: None,
    }
}

/// An async stream for reading from a serial port.
///
/// This struct provides both synchronous and asynchronous I/O on a serial port:
/// - Implements `std::io::Read` and `std::io::Write` for synchronous operations
/// - Implements `futures::Stream` for asynchronous streaming of incoming data
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

    pub fn try_poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Vec<u8>, std::io::Error>>> {
        self.inner.waker.register(cx.waker());

        if let Some(err) = self.inner.stream_error.lock().unwrap().as_ref() {
            return Poll::Ready(Some(Err(std::io::Error::new(err.kind(), err.to_string()))));
        }

        if !self.platform.is_thread_started() {
            self.platform.start_thread();
            return Poll::Pending;
        }

        let mut buffer = self.inner.in_buffer.lock().unwrap();
        if !buffer.is_empty() {
            // Drain all available data
            let data = buffer.drain(..).collect();
            return Poll::Ready(Some(Ok(data)));
        }

        Poll::Pending
    }
}

impl std::io::Read for SerialPortStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.platform.read(buf)
    }
}

impl std::io::Write for SerialPortStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.platform.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.platform.flush()
    }
}

impl Stream for SerialPortStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.try_poll_next(cx)
    }
}
