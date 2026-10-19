#[cfg(not(feature = "stream"))]
use std::io::Read;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
#[cfg(feature = "stream")]
use std::sync::Arc;
use std::task::{Context, Poll};

use async_io::Async;
#[cfg(not(feature = "stream"))]
use futures::io::AsyncRead as _;
use futures::io::AsyncWrite as _;
use nix::errno::Errno;

#[cfg(feature = "stream")]
use nix::libc::{c_int, ioctl, FIONREAD};
#[cfg(feature = "stream")]
use nix::poll::{poll, PollFd, PollFlags};
#[cfg(feature = "stream")]
use std::sync::mpsc;

#[cfg(feature = "stream")]
use crate::EventsInnerRead;
use crate::SerialPortStreamBuilder;

mod serial;

#[cfg(not(feature = "stream"))]
#[derive(Debug)]
struct SerialRead(OwnedFd);

#[cfg(not(feature = "stream"))]
impl Read for SerialRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match nix::unistd::read(self.0.as_fd(), buf) {
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

#[cfg(feature = "stream")]
#[derive(Debug)]
struct UnixInner {
    cancel_pipe: (OwnedFd, OwnedFd),
}

#[derive(Debug)]
pub struct PlatformRead {
    #[cfg(not(feature = "stream"))]
    read_async: Async<SerialRead>,
    #[cfg(feature = "stream")]
    read_thread_handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(feature = "stream")]
    read_inner: Arc<EventsInnerRead>,
    #[cfg(feature = "stream")]
    unix_inner: UnixInner,
    #[cfg(feature = "stream")]
    read_fd: Option<OwnedFd>,
}

#[derive(Debug)]
pub struct PlatformWrite {
    write_async: Async<SerialWrite>,
    flush_fd: OwnedFd,
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
            let read_running = self
                .read_thread_handle
                .as_ref()
                .is_some_and(|handle| !handle.is_finished());
            if read_running {
                let fd = self.unix_inner.cancel_pipe.1.as_fd();
                assert_eq!(nix::unistd::write(fd, &[1u8]).unwrap(), 1);
            }

            if let Some(handle) = self.read_thread_handle.take() {
                if !handle.is_finished() {
                    handle.join().unwrap();
                }
            }
        }
    }
}

impl Drop for PlatformWrite {
    fn drop(&mut self) {
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
        let read_fd = nix::unistd::dup(port_fd)?;
        let write_fd = nix::unistd::dup(port_fd)?;
        let flush_fd = nix::unistd::dup(port_fd)?;
        let settings_fd = nix::unistd::dup(port_fd)?;
        #[cfg(not(feature = "stream"))]
        let read_async = Async::new_nonblocking(SerialRead(read_fd))?;
        #[cfg(feature = "stream")]
        let stream_read_fd = Some(nix::unistd::dup(port_fd)?);
        let write_async = Async::new_nonblocking(SerialWrite(write_fd))?;
        drop(port);

        #[cfg(feature = "stream")]
        let cancel_pipe = nix::unistd::pipe().unwrap();

        Ok(Self {
            read: PlatformRead {
                #[cfg(not(feature = "stream"))]
                read_async,
                #[cfg(feature = "stream")]
                read_thread_handle: None,
                #[cfg(feature = "stream")]
                read_inner,
                #[cfg(feature = "stream")]
                unix_inner: UnixInner { cancel_pipe },
                #[cfg(feature = "stream")]
                read_fd: stream_read_fd,
            },
            write: PlatformWrite {
                write_async,
                flush_fd,
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
        assert!(self.read_thread_handle.is_none());

        let (tx, rx) = mpsc::channel();
        let read_inner_cloned = self.read_inner.clone();
        let cancel_fd = self.unix_inner.cancel_pipe.0.as_raw_fd();
        let read_fd = self.read_fd.take().unwrap();

        self.read_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(err) = Self::receive_thread(&read_inner_cloned, read_fd, cancel_fd) {
                *read_inner_cloned.stream_error.lock().unwrap() = Some(err);
                read_inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start thread");
    }

    #[cfg(feature = "stream")]
    fn bytes_to_read_fd(fd: BorrowedFd<'_>) -> std::io::Result<u32> {
        let mut count: c_int = 0;
        let ret = unsafe { ioctl(fd.as_raw_fd(), FIONREAD, &mut count) };
        if ret == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(count.max(0) as u32)
    }

    #[cfg(feature = "stream")]
    fn receive_thread(
        read_inner: &Arc<EventsInnerRead>,
        read_fd: OwnedFd,
        cancel_fd: i32,
    ) -> std::io::Result<()> {
        let read_fd_raw = read_fd.as_raw_fd();
        let mut buffer = Vec::with_capacity(1024);

        let purge_pending_data = |buffer: &mut Vec<u8>| -> std::io::Result<()> {
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(read_fd_raw) };
            let bytes_count = Self::bytes_to_read_fd(borrowed_fd)?;
            if bytes_count > 0 {
                buffer.resize(bytes_count as usize, 0);
                let did_read = match nix::unistd::read(borrowed_fd, buffer) {
                    Ok(n) => n,
                    Err(Errno::EAGAIN) => {
                        #[cfg(feature = "tracing")]
                        tracing::info!("EAGAIN for read");
                        0
                    }
                    Err(e) => return Err(std::io::Error::from(e)),
                };
                if did_read > 0 {
                    buffer.truncate(did_read);
                    read_inner
                        .in_buffer
                        .lock()
                        .unwrap()
                        .extend_from_slice(buffer);
                    buffer.clear();
                    read_inner.waker.wake();
                }
            }
            Ok(())
        };

        purge_pending_data(&mut buffer)?;

        loop {
            let read_fd_ = unsafe { BorrowedFd::borrow_raw(read_fd_raw) };
            let cancel_fd_ = unsafe { BorrowedFd::borrow_raw(cancel_fd) };
            let mut poll_fds = [
                PollFd::new(read_fd_, PollFlags::POLLIN),
                PollFd::new(cancel_fd_, PollFlags::POLLIN),
            ];

            let poll_result = poll(&mut poll_fds, nix::poll::PollTimeout::NONE)?;
            assert!(poll_result != 0);

            if poll_fds[1]
                .revents()
                .is_some_and(|events| events.contains(PollFlags::POLLIN))
            {
                return Ok(());
            }

            if let Some(read_poll) = poll_fds[0].revents() {
                if read_poll.contains(PollFlags::POLLIN) {
                    purge_pending_data(&mut buffer)?;
                } else {
                    return Err(std::io::Error::other("read fd events != POLLIN"));
                }
            }
        }
    }
}

impl PlatformWrite {
    pub fn flush_tx_unblocked(&self) -> blocking::Task<std::io::Result<()>> {
        let fd = self.flush_fd.as_raw_fd();
        blocking::unblock(move || serial::flush_output(fd))
    }

    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.write_async).poll_write(cx, buf)
    }
}

unsafe impl Send for PlatformRead {}
unsafe impl Sync for PlatformRead {}
unsafe impl Send for PlatformWrite {}
unsafe impl Sync for PlatformWrite {}
