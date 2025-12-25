use crate::platform::PlatformStream;
use futures::stream::Stream;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct SerialPortStream {
    inner: PlatformStream,
}

impl SerialPortStream {
    #[cfg(unix)]
    pub fn new(port: serialport::TTYPort) -> Result<Self, std::io::Error> {
        Ok(Self {
            inner: PlatformStream::new(port)?,
        })
    }

    #[cfg(windows)]
    pub fn new(port: serialport::COMPort) -> Result<Self, std::io::Error> {
        Ok(Self {
            inner: PlatformStream::new(port)?,
        })
    }
}

impl Stream for SerialPortStream {
    type Item = Result<Vec<u8>, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.inner.try_poll_next(cx)
    }
}
