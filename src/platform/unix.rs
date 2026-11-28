#[cfg(not(feature = "stream"))]
use std::io::Read;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_io::Async;
#[cfg(not(feature = "stream"))]
use futures::io::AsyncRead as _;
use futures::io::AsyncWrite as _;
use nix::errno::Errno;

#[cfg(feature = "stream")]
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};

#[cfg(feature = "stream")]
use crate::EventsInnerRead;
use crate::SerialPortStreamBuilder;

mod serial;

fn disconnected() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "device disconnected")
}

#[cfg(not(feature = "stream"))]
#[derive(Debug)]
struct SerialRead(OwnedFd);

#[cfg(not(feature = "stream"))]
impl Read for SerialRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match nix::unistd::read(self.0.as_fd(), buf) {
            Ok(0) => Err(disconnected()),
            Ok(n) => Ok(n),
            Err(Errno::EAGAIN) => Err(io::ErrorKind::WouldBlock.into()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(not(feature = "stream"))]
impl AsFd for SerialRead {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[cfg(not(feature = "stream"))]
unsafe impl async_io::IoSafe for SerialRead {}

#[derive(Debug)]
struct SerialWrite(OwnedFd);

impl Write for SerialWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match nix::unistd::write(self.0.as_fd(), buf) {
            Ok(n) => Ok(n),
            Err(Errno::EAGAIN) => Err(io::ErrorKind::WouldBlock.into()),
            Err(e) => Err(e.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl AsFd for SerialWrite {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

unsafe impl async_io::IoSafe for SerialWrite {}

#[derive(Debug)]
pub struct PlatformRead {
    #[cfg(not(feature = "stream"))]
    read_async: Async<SerialRead>,
    #[cfg(feature = "stream")]
    read_thread_handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(feature = "stream")]
    read_inner: Arc<EventsInnerRead>,
    #[cfg(feature = "stream")]
    read_fd: Option<OwnedFd>,
    #[cfg(feature = "stream")]
    cancel_rx: Option<OwnedFd>,
    #[cfg(feature = "stream")]
    cancel_tx: Option<OwnedFd>,
}

#[derive(Debug)]
pub struct PlatformWrite {
    write_async: Async<SerialWrite>,
    flush_fd: Arc<OwnedFd>,
}

#[derive(Debug)]
pub struct PlatformSettings(OwnedFd);

impl PlatformSettings {
    pub fn set_baud_rate(&self, baud_rate: u32) -> std::io::Result<()> {
        serial::set_baud_rate(self.0.as_raw_fd(), baud_rate)
    }
}

#[derive(Debug)]
pub struct PlatformStream {
    read: PlatformRead,
    write: PlatformWrite,
    settings: PlatformSettings,
}

impl Drop for PlatformRead {
    fn drop(&mut self) {
        #[cfg(feature = "stream")]
        {
            if let Some(cancel_tx) = self.cancel_tx.take() {
                let _ = nix::unistd::write(&cancel_tx, &[1u8]);
            }
            if let Some(handle) = self.read_thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for PlatformWrite {
    fn drop(&mut self) {
        // note specially on linux if there was a large write -> then drop, the fd will hang until bytes are written
        // therefore we need to drain/clear the output for better system responsiveness
        //
        let _ = serial::clear(self.flush_fd.as_raw_fd(), crate::ClearBuffer::Output);
    }
}

impl PlatformStream {
    pub fn new(
        builder: SerialPortStreamBuilder,
        #[cfg(feature = "stream")] read_inner: Arc<EventsInnerRead>,
    ) -> Result<Self, std::io::Error> {
        let port = serial::open_port(&builder)?;
        if let Some(buffer) = builder.clear_buffer {
            serial::clear(port.as_raw_fd(), buffer)?;
        }
        let port_fd = port.as_fd();
        #[cfg(not(feature = "stream"))]
        let read_fd = serial::dup_cloexec(port_fd)?;
        let write_fd = serial::dup_cloexec(port_fd)?;
        let flush_fd = serial::dup_cloexec(port_fd)?;
        let settings_fd = serial::dup_cloexec(port_fd)?;
        #[cfg(not(feature = "stream"))]
        let read_async = Async::new_nonblocking(SerialRead(read_fd))?;
        #[cfg(feature = "stream")]
        let stream_read_fd = serial::dup_cloexec(port_fd)?;
        let write_async = Async::new_nonblocking(SerialWrite(write_fd))?;
        drop(port);

        #[cfg(feature = "stream")]
        let (cancel_rx, cancel_tx) = nix::unistd::pipe()?;

        Ok(Self {
            read: PlatformRead {
                #[cfg(not(feature = "stream"))]
                read_async,
                #[cfg(feature = "stream")]
                read_thread_handle: None,
                #[cfg(feature = "stream")]
                read_inner,
                #[cfg(feature = "stream")]
                read_fd: Some(stream_read_fd),
                #[cfg(feature = "stream")]
                cancel_rx: Some(cancel_rx),
                #[cfg(feature = "stream")]
                cancel_tx: Some(cancel_tx),
            },
            write: PlatformWrite {
                write_async,
                flush_fd: Arc::new(flush_fd),
            },
            settings: PlatformSettings(settings_fd),
        })
    }

    pub fn set_baud_rate(&self, baud_rate: u32) -> std::io::Result<()> {
        self.settings.set_baud_rate(baud_rate)
    }

    pub fn flush_tx_unblocked(&self) -> blocking::Task<std::io::Result<()>> {
        self.write.flush_tx_unblocked()
    }

    #[cfg(not(feature = "stream"))]
    pub fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        self.read.poll_read(cx, buf)
    }

    #[cfg(feature = "stream")]
    pub fn is_read_thread_started(&self) -> bool {
        self.read.is_read_thread_started()
    }

    #[cfg(feature = "stream")]
    pub fn start_read_thread(&mut self) {
        self.read.start_read_thread();
    }

    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        self.write.poll_write(cx, buf)
    }

    pub fn split(self) -> (PlatformRead, PlatformWrite, PlatformSettings) {
        (self.read, self.write, self.settings)
    }
}

impl PlatformRead {
    #[cfg(not(feature = "stream"))]
    pub fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.read_async).poll_read(cx, buf)
    }

    #[cfg(feature = "stream")]
    pub fn is_read_thread_started(&self) -> bool {
        self.read_thread_handle.is_some()
    }

    #[cfg(feature = "stream")]
    pub fn start_read_thread(&mut self) {
        let (Some(read_fd), Some(cancel_fd)) = (self.read_fd.take(), self.cancel_rx.take()) else {
            return;
        };
        let read_inner = self.read_inner.clone();
        self.read_thread_handle = Some(std::thread::spawn(move || {
            if let Err(err) = Self::receive_thread(&read_inner, read_fd, cancel_fd) {
                *read_inner.stream_error.lock().unwrap() = Some(err);
                read_inner.waker.wake();
            }
        }));
    }

    #[cfg(feature = "stream")]
    fn receive_thread(
        read_inner: &EventsInnerRead,
        read_fd: OwnedFd,
        cancel_fd: OwnedFd,
    ) -> std::io::Result<()> {
        let mut buffer = [0u8; 4096];
        loop {
            let mut poll_fds = [
                PollFd::new(read_fd.as_fd(), PollFlags::POLLIN),
                PollFd::new(cancel_fd.as_fd(), PollFlags::POLLIN),
            ];
            match poll(&mut poll_fds, PollTimeout::NONE) {
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }

            if poll_fds[1]
                .revents()
                .is_some_and(|events| !events.is_empty())
            {
                return Ok(());
            }

            let Some(events) = poll_fds[0].revents() else {
                continue;
            };
            if events.contains(PollFlags::POLLIN) {
                match nix::unistd::read(read_fd.as_fd(), &mut buffer) {
                    Ok(0) => return Err(disconnected()),
                    Ok(n) => {
                        read_inner
                            .in_buffer
                            .lock()
                            .unwrap()
                            .extend_from_slice(&buffer[..n]);
                        read_inner.waker.wake();
                    }
                    Err(Errno::EAGAIN) => {}
                    Err(e) => return Err(e.into()),
                }
            } else if events
                .intersects(PollFlags::POLLHUP | PollFlags::POLLERR | PollFlags::POLLNVAL)
            {
                return Err(disconnected());
            }
        }
    }
}

impl PlatformWrite {
    pub fn flush_tx_unblocked(&self) -> blocking::Task<std::io::Result<()>> {
        let fd = self.flush_fd.clone();
        blocking::unblock(move || serial::flush_output(fd.as_raw_fd()))
    }

    /// Writes from `buf` without copying. On `Pending`, `async_io` retries from the same slice
    /// after `EAGAIN`; callers must repoll with the same unsent bytes until `Ready`.
    /// `futures` / Tokio `write` and `write_all` keep the same `&[u8]` across `Pending` polls per
    /// chunk.
    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.write_async).poll_write(cx, buf)
    }
}
