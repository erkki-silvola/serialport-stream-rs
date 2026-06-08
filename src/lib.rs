//! Async serial port I/O as [`Stream`], [`AsyncRead`], and [`AsyncWrite`].
//!
//! Configure and open ports with [`new`] → [`SerialPortStreamBuilder`] → [`.open()`](SerialPortStreamBuilder::open).
//! Line settings, DTR, and buffer clearing are applied at open time.
//!
//! POSIX `termios` on Unix; Win32 COMM APIs on Windows. Configuration types ([`DataBits`],
//! [`Parity`], [`StopBits`], [`FlowControl`], [`ClearBuffer`]) are defined in this crate.
//!
//! The first read poll starts a background thread that appends incoming bytes to an in-memory FIFO
//! shared by [`Stream`] and [`AsyncRead`]. There is no backpressure. The first write poll starts
//! a separate background thread.
//!
//! [`Stream`] / [`TryStreamExt::try_next`] drains the full FIFO per item; [`AsyncRead`] reads
//! partially and leaves the remainder cached. Use one read style per open port.
//!
//! [`AsyncWriteExt`], [`AsyncReadExt`], and [`TryStreamExt`] are re-exported from `futures`.
//!
//! ```no_run
//! use serialport_stream::{new, AsyncWriteExt, TryStreamExt};
//!
//! # async fn example() -> std::io::Result<()> {
//! let mut stream = new("/dev/ttyUSB0", 115200).open()?;
//! stream.write_all(b"PING\r\n").await?;
//! while let Some(chunk) = stream.try_next().await? {
//!     println!("{chunk:?}");
//! }
//! # Ok(())
//! # }
//! ```
//!

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

mod platform;
mod types;

pub mod line_settings;
pub use types::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};

use crate::platform::PlatformStream;
use futures::task::AtomicWaker;

fn clone_io_error(err: &std::io::Error) -> std::io::Error {
    match err.raw_os_error() {
        Some(code) => std::io::Error::from_raw_os_error(code),
        None => std::io::Error::new(err.kind(), err.to_string()),
    }
}

pub use futures::io::{AsyncRead, AsyncReadExt};
pub use futures::io::{AsyncWrite, AsyncWriteExt};
pub use futures::stream::{Stream, TryStreamExt};

#[derive(Debug)]
pub(crate) struct EventsInnerRead {
    pub(crate) in_buffer: Mutex<Vec<u8>>,
    pub(crate) stream_error: Mutex<Option<std::io::Error>>,
    pub(crate) waker: AtomicWaker,
}

impl EventsInnerRead {
    pub(crate) fn new() -> Self {
        Self {
            in_buffer: Mutex::new(Vec::new()),
            stream_error: Mutex::new(None),
            waker: AtomicWaker::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PendingWrite {
    Idle,
    Buffer(Vec<u8>),
    Completed(usize),
}

#[derive(Debug)]
pub(crate) struct EventsInnerWrite {
    pub(crate) pending: Mutex<PendingWrite>,
    pub(crate) write_error: Mutex<Option<std::io::Error>>,
    pub(crate) waker: AtomicWaker,
}

impl EventsInnerWrite {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(PendingWrite::Idle),
            write_error: Mutex::new(None),
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
    /// `true` asserts DTR, `false` clears it. The line is always driven on open.
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
    /// [`clear`](Self::clear) before any background read/write threads are started.
    pub fn open(self) -> std::io::Result<SerialPortStream> {
        let read_inner = Arc::new(EventsInnerRead::new());
        let write_inner = Arc::new(EventsInnerWrite::new());
        Ok(SerialPortStream {
            platform: PlatformStream::new(self, read_inner.clone(), write_inner.clone())?,
            read_inner,
            write_inner,
            flush_task: None,
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
/// - [`Stream`] / [`AsyncRead`]: shared in-memory receive FIFO (background read thread).
/// - [`AsyncWrite`]: dedicated background write thread.
///
/// Configure the port only via [`SerialPortStreamBuilder`] before [`SerialPortStreamBuilder::open`].
///
/// # Example
///
/// ```no_run
/// use serialport_stream::new;
/// use futures::io::AsyncWriteExt;
/// use futures::stream::TryStreamExt;
///
/// # async fn example() -> std::io::Result<()> {
/// let mut stream = new("COM3", 115200).open()?;
/// stream.write_all(&[0x0a, 0xC0]).await?;
/// if let Some(bytes) = stream.try_next().await? {
///     println!("{bytes:?}");
/// }
/// # Ok(())
/// # }
/// ```
pub struct SerialPortStream {
    platform: PlatformStream,
    read_inner: Arc<EventsInnerRead>,
    write_inner: Arc<EventsInnerWrite>,
    flush_task: Option<blocking::Task<std::io::Result<()>>>,
}

impl std::fmt::Debug for SerialPortStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SerialPortStream")
            .field("platform", &self.platform)
            .field("read_inner", &self.read_inner)
            .field("write_inner", &self.write_inner)
            .field("flush_task", &self.flush_task.as_ref().map(|_| "..."))
            .finish()
    }
}

impl SerialPortStream {
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

    fn poll_writer_ready(&mut self, cx: &mut Context<'_>) -> std::io::Result<()> {
        self.write_inner.waker.register(cx.waker());

        if let Some(err) = self.write_inner.write_error.lock().unwrap().as_ref() {
            return Err(clone_io_error(err));
        }

        if !self.platform.is_write_thread_started() {
            self.platform.start_write_thread();
        }

        Ok(())
    }

    /// Polls for the next received chunk, same as [`Stream::poll_next`].
    ///
    /// When ready, returns `Poll::Ready(Some(Ok(vec)))` with every byte currently buffered,
    /// or `Poll::Pending` if the read thread has not yet delivered data.
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
        assert!(!buf.is_empty());
        let this = self.as_mut().get_mut();
        match this.poll_receiver_ready(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                let mut buffer = this.read_inner.in_buffer.lock().unwrap();
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

impl AsyncWrite for SerialPortStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        assert!(!buf.is_empty());
        let this = self.as_mut().get_mut();
        match this.poll_writer_ready(cx) {
            Err(e) => Poll::Ready(Err(e)),
            Ok(()) => {
                let mut pending = this.write_inner.pending.lock().unwrap();
                match *pending {
                    crate::PendingWrite::Idle => {
                        // new transaction
                        *pending = crate::PendingWrite::Buffer(buf.to_vec());
                        drop(pending);
                        self.platform.signal_write();
                        Poll::Pending
                    }
                    crate::PendingWrite::Buffer(_) => {
                        // write still pending
                        Poll::Pending
                    }
                    crate::PendingWrite::Completed(n) => {
                        *pending = crate::PendingWrite::Idle;
                        Poll::Ready(Ok(n))
                    }
                }
            }
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();

        this.write_inner.waker.register(cx.waker());

        if let Some(err) = this.write_inner.write_error.lock().unwrap().as_ref() {
            return Poll::Ready(Err(clone_io_error(err)));
        }

        if matches!(
            *this.write_inner.pending.lock().unwrap(),
            crate::PendingWrite::Buffer(_)
        ) {
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
