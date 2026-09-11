//! Async serial port I/O as [`AsyncRead`] and [`AsyncWrite`], with optional [`Stream`] support.
//!
//! Async runtime agnostic: this crate implements [`futures`] traits only and does not depend on
//! Tokio, async-std, or any other executor. Use it with any runtime that polls those futures
//! (Tokio, async-std, [`futures_lite::future::block_on`](https://docs.rs/futures-lite/latest/futures_lite/future/fn.block_on.html), etc.).
//!
//! Configure and open ports with [`new`] → [`SerialPortStreamBuilder`] → [`.open()`](SerialPortStreamBuilder::open).
//! Line settings, DTR, and buffer clearing are applied at open time.
//!
//! ## Features
//!
//! - **`stream`** (optional): [`Stream`], [`TryStreamExt`], and [`SerialPortStream::try_poll_next`]. Starts a
//!   background receive pump into an in-memory FIFO (poll-based thread on Unix, `WaitCommEvent`
//!   thread on Windows). With `stream` enabled, [`AsyncRead`] and [`Stream`] share that FIFO.
//! - **`tracing`** (optional): diagnostic logs.
//!
//! ## Read paths
//!
//! | Platform | [`AsyncRead`] (no `stream`) | [`AsyncRead`] + [`Stream`] (`stream` feature) |
//! | --- | --- | --- |
//! | Unix | Direct [`async-io`](https://docs.rs/async-io) poll on the port fd | Shared poll-based background thread → FIFO |
//! | Windows | Overlapped `ReadFile` + thread-pool reactor | Shared background read thread → FIFO |
//!
//! When `stream` is enabled, [`AsyncRead`] and [`Stream`] share the same background receive FIFO
//! (Unix and Windows).
//!
//! ## Write path
//!
//! On Unix, [`AsyncWrite`] uses [`async-io`](https://docs.rs/async-io). On Windows, overlapped
//! `WriteFile` with a dedicated thread-pool completion reactor.
//!
//! [`AsyncReadExt`] and [`AsyncWriteExt`] are re-exported from `futures`.
//!
//! ```no_run
//! use serialport_stream::{new, AsyncReadExt, AsyncWriteExt};
//!
//! # async fn example() -> std::io::Result<()> {
//! let mut stream = new("/dev/ttyUSB0", 115200).open()?;
//! stream.write_all(b"PING\r\n").await?;
//! let mut buf = [0u8; 256];
//! let n = stream.read(&mut buf).await?;
//! # let _ = n;
//! # Ok(())
//! # }
//! ```

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

mod platform;
mod types;

pub mod line_settings;
pub use types::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};

use crate::platform::PlatformStream;

pub use futures::io::{AsyncRead, AsyncReadExt};
pub use futures::io::{AsyncWrite, AsyncWriteExt};
#[cfg(feature = "stream")]
pub use futures::stream::{Stream, TryStreamExt};

#[derive(Debug)]
#[cfg(feature = "stream")]
pub(crate) struct EventsInnerRead {
    pub(crate) in_buffer: Mutex<Vec<u8>>,
    pub(crate) stream_error: Mutex<Option<std::io::Error>>,
    pub(crate) waker: AtomicWaker,
}

#[cfg(feature = "stream")]
impl EventsInnerRead {
    pub(crate) fn new() -> Self {
        Self {
            in_buffer: Mutex::new(Vec::new()),
            stream_error: Mutex::new(None),
            waker: AtomicWaker::new(),
        }
    }
}

/// Builder for serial port path, line settings, and one-shot open options.
///
/// Created with [`new()`], configured with chained methods, then finalized with
/// [`open()`](SerialPortStreamBuilder::open).
///
/// # Example
///
/// ```no_run
/// use serialport_stream::new;
/// use serialport_stream::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};
///
/// # fn example() -> std::io::Result<()> {
/// let stream = new("/dev/ttyUSB0", 115200)
///     .data_bits(DataBits::Eight)
///     .parity(Parity::None)
///     .stop_bits(StopBits::One)
///     .flow_control(FlowControl::None)
///     .dtr_on_open(true)
///     .clear(ClearBuffer::All)
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
    pub(crate) dtr_on_open: bool,
    pub(crate) clear_buffer: Option<ClearBuffer>,
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

    /// Sets the DTR (Data Terminal Ready) signal state applied when opening the port.
    ///
    /// Default: `false`
    #[must_use]
    pub fn dtr_on_open(mut self, state: bool) -> Self {
        self.dtr_on_open = state;
        self
    }

    /// Clears RX and/or TX driver buffers when the port is opened, before async I/O starts.
    ///
    /// See [`ClearBuffer`] (`Input`, `Output`, or `All`).
    #[must_use]
    pub fn clear(mut self, buffer: ClearBuffer) -> Self {
        self.clear_buffer = Some(buffer);
        self
    }

    /// Opens the serial port and returns a [`SerialPortStream`].
    ///
    /// Applies line settings, [`dtr_on_open`](Self::dtr_on_open), and optional
    /// [`clear`](Self::clear) before async I/O begins.
    pub fn open(self) -> std::io::Result<SerialPortStream> {
        #[cfg(feature = "stream")]
        let read_inner = Arc::new(EventsInnerRead::new());
        Ok(SerialPortStream {
            platform: PlatformStream::new(
                self,
                #[cfg(feature = "stream")]
                read_inner.clone(),
            )?,
            #[cfg(feature = "stream")]
            read_inner,
            flush_task: None,
            write_in_flight: false,
        })
    }
}

/// Creates a [`SerialPortStreamBuilder`] with default line settings (8N1, no flow control).
///
/// # Examples
///
/// Unix device path:
///
/// ```no_run
/// # use serialport_stream::new;
/// # fn example() -> std::io::Result<()> {
/// let _stream = new("/dev/ttyUSB0", 115200).open()?;
/// # Ok(())
/// # }
/// ```
///
/// Windows COM port:
///
/// ```no_run
/// # use serialport_stream::new;
/// # fn example() -> std::io::Result<()> {
/// let _stream = new("COM3", 9600).open()?;
/// # Ok(())
/// # }
/// ```
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
        dtr_on_open: false,
        clear_buffer: None,
    }
}

/// An opened serial port for async reads and writes.
///
/// # Read behavior
///
/// - **Unix:** [`AsyncRead`] polls the port fd directly via async-io.
/// - **Windows:** [`AsyncRead`] uses overlapped `ReadFile` with a thread-pool completion reactor.
///
/// Enable the `stream` feature for [`Stream`] / [`SerialPortStream::try_poll_next`]. That starts a background receive
/// pump and FIFO on both platforms. With `stream` enabled, [`AsyncRead`] and [`Stream`] both read
/// from the same FIFO.
///
/// # Write behavior
///
/// Both platforms use direct async polling (async-io on Unix, overlapped `WriteFile` + thread-pool reactor on Windows).
///
/// # Example
///
/// ```no_run
/// use serialport_stream::new;
/// use futures::io::{AsyncReadExt, AsyncWriteExt};
///
/// # async fn example() -> std::io::Result<()> {
/// let mut stream = new("COM3", 115200).open()?;
/// stream.write_all(&[0x0a, 0xC0]).await?;
/// let mut buf = [0u8; 256];
/// let n = stream.read(&mut buf).await?;
/// # let _ = n;
/// # Ok(())
/// # }
/// ```
pub struct SerialPortStream {
    platform: PlatformStream,
    #[cfg(feature = "stream")]
    read_inner: Arc<EventsInnerRead>,
    flush_task: Option<blocking::Task<std::io::Result<()>>>,
    write_in_flight: bool,
}

impl std::fmt::Debug for SerialPortStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialPortStream")
            .field("platform", &self.platform)
            .field("flush_task", &self.flush_task.as_ref().map(|_| "..."))
            .field("write_in_flight", &self.write_in_flight)
            .finish()
    }
}

impl SerialPortStream {
    #[cfg(feature = "stream")]
    fn poll_receiver_ready(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.read_inner.waker.register(cx.waker());

        if let Some(err) = self.read_inner.stream_error.lock().unwrap().as_ref() {
            return Poll::Ready(Err(clone_io_error(err)));
        }

        if !self.platform.is_read_thread_started() {
            self.platform.start_read_thread();
            return Poll::Pending;
        }

        Poll::Ready(Ok(()))
    }

    #[cfg(feature = "stream")]
    fn poll_read_fifo(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.poll_receiver_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                let mut buffer = self.read_inner.in_buffer.lock().unwrap();
                if buffer.is_empty() {
                    return Poll::Pending;
                }
                let n = buffer.len().min(buf.len());
                buf[..n].copy_from_slice(&buffer[..n]);
                buffer.drain(..n);
                Poll::Ready(Ok(n))
            }
        }
    }

    #[cfg(feature = "stream")]
    /// Polls for the next received chunk, same as [`Stream::poll_next`].
    ///
    /// When ready, returns `Poll::Ready(Some(Ok(vec)))` with every byte currently buffered in the
    /// receive FIFO, or `Poll::Pending` if the pump thread has not yet delivered data.
    ///
    /// Requires the `stream` Cargo feature.
    pub fn try_poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Vec<u8>, std::io::Error>>> {
        match self.poll_receiver_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let mut buffer = self.read_inner.in_buffer.lock().unwrap();
                if !buffer.is_empty() {
                    let data = buffer.drain(..).collect();
                    return Poll::Ready(Some(Ok(data)));
                }
                Poll::Pending
            }
        }
    }

    /// Sets the baud rate (bits per second) on an already-open port.
    ///
    /// Other line settings (data bits, parity, stop bits, flow control) remain unchanged.
    pub fn set_baudrate(&mut self, baud_rate: u32) -> std::io::Result<()> {
        self.platform.set_baud_rate(baud_rate)
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
        assert!(!buf.is_empty());
        let this = self.as_mut().get_mut();
        #[cfg(feature = "stream")]
        {
            return this.poll_read_fifo(cx, buf);
        }
        #[cfg(not(feature = "stream"))]
        {
            this.platform.poll_read(cx, buf)
        }
    }
}

#[cfg(feature = "stream")]
impl Stream for SerialPortStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.try_poll_next(cx)
    }
}

impl AsyncWrite for SerialPortStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        assert!(!buf.is_empty());
        let this = self.as_mut().get_mut();
        let result = this.platform.poll_write(cx, buf);
        this.write_in_flight = result.is_pending();
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();

        if this.write_in_flight {
            return Poll::Pending;
        }

        if this.flush_task.is_none() {
            this.flush_task = Some(this.platform.flush_tx_unblocked());
        }

        let task = this.flush_task.as_mut().expect("flush task");
        match Pin::new(task).poll(cx) {
            Poll::Ready(result) => {
                this.flush_task = None;
                Poll::Ready(result)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}
