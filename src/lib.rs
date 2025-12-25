use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

mod platform;

use crate::platform::PlatformStream;

pub use futures::stream::{Stream, TryStreamExt};
pub use serialport;
use serialport::{DataBits, FlowControl, Parity, StopBits};

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
    #[allow(clippy::assigning_clones)]
    #[must_use]
    pub fn path<'a>(mut self, path: impl Into<std::borrow::Cow<'a, str>>) -> Self {
        self.path = path.into().as_ref().to_owned();
        self
    }

    #[must_use]
    pub fn baud_rate(mut self, baud_rate: u32) -> Self {
        self.baud_rate = baud_rate;
        self
    }

    #[must_use]
    pub fn data_bits(mut self, data_bits: DataBits) -> Self {
        self.data_bits = data_bits;
        self
    }

    #[must_use]
    pub fn flow_control(mut self, flow_control: FlowControl) -> Self {
        self.flow_control = flow_control;
        self
    }

    #[must_use]
    pub fn parity(mut self, parity: Parity) -> Self {
        self.parity = parity;
        self
    }

    #[must_use]
    pub fn stop_bits(mut self, stop_bits: StopBits) -> Self {
        self.stop_bits = stop_bits;
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    #[must_use]
    pub fn dtr_on_open(mut self, state: bool) -> Self {
        self.dtr_on_open = Some(state);
        self
    }

    #[must_use]
    pub fn preserve_dtr_on_open(mut self) -> Self {
        self.dtr_on_open = None;
        self
    }

    pub fn open(self) -> std::io::Result<SerialPortStream> {
        Ok(SerialPortStream {
            platform: PlatformStream::new(self)?,
        })
    }
}

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

pub struct SerialPortStream {
    platform: PlatformStream,
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
        todo!();
    }
}

impl Stream for SerialPortStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.platform.try_poll_next(cx)
    }
}
