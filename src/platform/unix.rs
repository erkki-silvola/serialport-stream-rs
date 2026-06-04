use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc;
use std::sync::Arc;

use nix::libc::{c_int, ioctl, FIONREAD};
use nix::poll::{poll, PollFd, PollFlags};

use crate::{EventsInner, EventsInnerWrite, SerialPortStreamBuilder};

mod serial;

/// Unix-specific fields
#[derive(Debug)]
struct UnixInner {
    cancel_pipe: (OwnedFd, OwnedFd),
    write_signal_pipe: (OwnedFd, OwnedFd),
}

#[derive(Debug)]
pub struct PlatformStream {
    read_thread_handle: Option<std::thread::JoinHandle<()>>,
    write_thread_handle: Option<std::thread::JoinHandle<()>>,
    inner: Arc<EventsInner>,
    write_inner: Arc<EventsInnerWrite>,
    unix_inner: UnixInner,
    read_fd: Option<OwnedFd>,
    write_fd: Option<OwnedFd>,
    flush_fd: OwnedFd,
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        let read_running = self
            .read_thread_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished());
        let write_running = self
            .write_thread_handle
            .as_ref()
            .is_some_and(|handle| !handle.is_finished());
        if read_running || write_running {
            let fd = self.unix_inner.cancel_pipe.1.as_fd();
            assert_eq!(nix::unistd::write(fd, &[1u8]).unwrap(), 1);
        }

        println!("read_running {read_running} write_running {write_running}");

        if let Some(handle) = self.read_thread_handle.take() {
            if !handle.is_finished() {
                handle.join().unwrap();
            }
        }

        if let Some(handle) = self.write_thread_handle.take() {
            if !handle.is_finished() {
                handle.join().unwrap();
            }
        }
        println!("all done");
    }
}

impl PlatformStream {
    pub fn new(
        builder: SerialPortStreamBuilder,
        inner: Arc<EventsInner>,
        write_inner: Arc<EventsInnerWrite>,
    ) -> Result<Self, std::io::Error> {
        let port = serial::open_port(&builder)?;
        if let Some(buffer) = builder.clear_buffer {
            serial::clear(port.as_raw_fd(), buffer)?;
        }
        let port_fd = port.as_fd();
        let read_fd = nix::unistd::dup(port_fd)?;
        let write_fd = nix::unistd::dup(port_fd)?;
        let flush_fd = nix::unistd::dup(port_fd)?;
        drop(port);

        let cancel_pipe = nix::unistd::pipe().unwrap();
        let write_signal_pipe = nix::unistd::pipe().unwrap();
        let unix_inner = UnixInner {
            cancel_pipe,
            write_signal_pipe,
        };

        Ok(Self {
            read_thread_handle: None,
            write_thread_handle: None,
            inner,
            write_inner,
            unix_inner,
            read_fd: Some(read_fd),
            write_fd: Some(write_fd),
            flush_fd,
        })
    }

    pub fn flush_tx_unblocked(&self) -> smol::Task<std::io::Result<()>> {
        let fd = self.flush_fd.as_raw_fd();
        smol::unblock(move || serial::flush_output(fd))
    }

    pub fn is_read_thread_started(&self) -> bool {
        self.read_thread_handle.is_some()
    }

    pub fn is_write_thread_started(&self) -> bool {
        self.write_thread_handle.is_some()
    }

    pub fn start_read_thread(&mut self) {
        assert!(self.read_thread_handle.is_none());

        let (tx, rx) = mpsc::channel();
        let inner_cloned = self.inner.clone();
        let cancel_fd = self.unix_inner.cancel_pipe.0.as_raw_fd();
        let read_fd = self.read_fd.take().unwrap();

        self.read_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(err) = Self::receive_thread(&inner_cloned, read_fd, cancel_fd) {
                *inner_cloned.stream_error.lock().unwrap() = Some(err);
                inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start thread");
    }

    pub fn start_write_thread(&mut self) {
        assert!(self.write_thread_handle.is_none());

        let (tx, rx) = mpsc::channel();
        let write_inner_cloned = self.write_inner.clone();
        let cancel_fd = self.unix_inner.cancel_pipe.0.as_raw_fd();
        let write_signal_fd = self.unix_inner.write_signal_pipe.0.as_raw_fd();
        let write_fd = self.write_fd.take().unwrap();

        self.write_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(err) =
                Self::write_thread(&write_inner_cloned, write_fd, write_signal_fd, cancel_fd)
            {
                *write_inner_cloned.write_error.lock().unwrap() = Some(err);
                write_inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start write thread");
    }

    pub fn signal_write(&self) {
        let fd = self.unix_inner.write_signal_pipe.1.as_fd();
        assert_eq!(nix::unistd::write(fd, &[1u8]).unwrap(), 1);
    }

    fn bytes_to_read_fd(fd: BorrowedFd<'_>) -> std::io::Result<u32> {
        let mut count: c_int = 0;
        let ret = unsafe { ioctl(fd.as_raw_fd(), FIONREAD, &mut count) };
        if ret == -1 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(count.max(0) as u32)
    }

    fn receive_thread(
        inner: &Arc<EventsInner>,
        read_fd: OwnedFd,
        cancel_fd: i32,
    ) -> std::io::Result<()> {
        let read_fd_raw = read_fd.as_raw_fd();

        let purge_pending_data = || -> std::io::Result<()> {
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(read_fd_raw) };
            let bytes_count = Self::bytes_to_read_fd(borrowed_fd)?;
            if bytes_count > 0 {
                let mut buffer = vec![0u8; bytes_count as usize];
                let did_read = nix::unistd::read(borrowed_fd, &mut buffer)?;
                buffer.truncate(did_read);
                inner.in_buffer.lock().unwrap().extend(buffer);
                inner.waker.wake();
            }
            Ok(())
        };

        purge_pending_data()?;

        loop {
            let read_fd_ = unsafe { BorrowedFd::borrow_raw(read_fd_raw) };
            let cancel_fd_ = unsafe { BorrowedFd::borrow_raw(cancel_fd) };
            let mut poll_fds = [
                PollFd::new(read_fd_, PollFlags::POLLIN),
                PollFd::new(cancel_fd_, PollFlags::POLLIN),
            ];

            let poll_result = poll(&mut poll_fds, nix::poll::PollTimeout::NONE)?;
            if poll_result == -1 {
                return Err(std::io::Error::last_os_error());
            }
            assert!(poll_result != 0);
            if let Some(cancel_poll) = poll_fds[1].revents() {
                if cancel_poll.contains(PollFlags::POLLIN) {
                    // Cancel signal received, exit thread
                    return Ok(());
                }
            }

            if let Some(read_poll) = poll_fds[0].revents() {
                if read_poll.contains(PollFlags::POLLIN) {
                    purge_pending_data()?;
                } else {
                    return Err(std::io::Error::other("read fd events != POLLIN"));
                }
            }
        }
    }

    fn write_thread(
        write_inner: &Arc<EventsInnerWrite>,
        write_fd: OwnedFd,
        write_signal_fd: i32,
        cancel_fd: i32,
    ) -> std::io::Result<()> {
        let write_fd_raw = write_fd.as_raw_fd();

        loop {
            let write_signal_fd_ = unsafe { BorrowedFd::borrow_raw(write_signal_fd) };
            let cancel_fd_ = unsafe { BorrowedFd::borrow_raw(cancel_fd) };
            let mut wait_poll_fds = [
                PollFd::new(write_signal_fd_, PollFlags::POLLIN),
                PollFd::new(cancel_fd_, PollFlags::POLLIN),
            ];

            let poll_result = poll(&mut wait_poll_fds, nix::poll::PollTimeout::NONE)?;
            if poll_result == -1 {
                return Err(std::io::Error::last_os_error());
            }
            assert!(poll_result != 0);

            if wait_poll_fds[1]
                .revents()
                .is_some_and(|events| events.contains(PollFlags::POLLIN))
            {
                // cancel
                return Ok(());
            }

            if wait_poll_fds[0]
                .revents()
                .is_some_and(|events| events.contains(PollFlags::POLLIN))
            {
                let mut buffer = [0u8; 1];
                assert_eq!(nix::unistd::read(write_signal_fd_, &mut buffer).unwrap(), 1);
            }

            let pending = write_inner.pending.lock().unwrap().clone();
            match pending {
                crate::PendingWrite::Buffer(buf) => {
                    let write_fd_ = unsafe { BorrowedFd::borrow_raw(write_fd_raw) };
                    let cancel_fd_ = unsafe { BorrowedFd::borrow_raw(cancel_fd) };
                    let mut write_poll_fds = [
                        PollFd::new(write_fd_, PollFlags::POLLOUT),
                        PollFd::new(cancel_fd_, PollFlags::POLLIN),
                    ];

                    let poll_result = poll(&mut write_poll_fds, nix::poll::PollTimeout::NONE)?;
                    if poll_result == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    assert!(poll_result != 0);

                    if write_poll_fds[1]
                        .revents()
                        .is_some_and(|events| events.contains(PollFlags::POLLIN))
                    {
                        return Ok(());
                    }
                    if write_poll_fds[0]
                        .revents()
                        .is_some_and(|events| events.contains(PollFlags::POLLOUT))
                    {
                        let written = nix::unistd::write(write_fd_, &buf)?;
                        let mut pending = write_inner.pending.lock().unwrap();
                        *pending = crate::PendingWrite::Completed(written);
                        write_inner.waker.wake();
                    } else {
                        return Err(std::io::Error::other(format!(
                            "POLLOUT fd error {:?}",
                            write_poll_fds[0].revents()
                        )));
                    }
                }
                _ => {
                    panic!("was waiting for PendingWriteBuffer but got {pending:?}");
                }
            }
        }
    }
}
