use std::io::Read;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc;
use std::sync::Arc;

use nix::poll::{poll, PollFd, PollFlags};
use serialport::SerialPort;

use crate::{EventsInner, SerialPortStreamBuilder};

/// Unix-specific fields
#[derive(Debug)]
struct UnixInner {
    cancel_pipe: (OwnedFd, OwnedFd),
}

#[derive(Debug)]
pub struct PlatformStream {
    thread_handle: Option<std::thread::JoinHandle<()>>,
    inner: Arc<EventsInner>,
    unix_inner: UnixInner,
    port: Option<serialport::TTYPort>,
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            if !handle.is_finished() {
                let fd = self.unix_inner.cancel_pipe.1.as_fd();
                assert_eq!(nix::unistd::write(fd, &[1u8]).unwrap(), 1);
                handle.join().unwrap();
            }
        }
    }
}

impl PlatformStream {
    pub fn new(
        builder: SerialPortStreamBuilder,
        inner: Arc<EventsInner>,
    ) -> Result<Self, std::io::Error> {
        let serialport_builder = serialport::new(builder.path, builder.baud_rate)
            .timeout(builder.timeout)
            .data_bits(builder.data_bits)
            .flow_control(builder.flow_control)
            .parity(builder.parity)
            .stop_bits(builder.stop_bits)
            .dtr_on_open(builder.dtr_on_open.unwrap_or(false));

        let port = serialport_builder.open_native()?;

        let cancel_pipe = nix::unistd::pipe().unwrap();
        let unix_inner = UnixInner { cancel_pipe };

        Ok(Self {
            thread_handle: None,
            inner,
            unix_inner,
            port: Some(port),
        })
    }

    pub fn is_thread_started(&self) -> bool {
        self.thread_handle.is_some()
    }

    pub fn start_thread(&mut self) {
        assert!(self.thread_handle.is_none());

        let (tx, rx) = mpsc::channel();
        let inner_cloned = self.inner.clone();
        let cancel_fd = self.unix_inner.cancel_pipe.0.as_raw_fd();
        let port = self.port.take().unwrap();

        self.thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(err) = Self::receive_thread(&inner_cloned, port, cancel_fd) {
                *inner_cloned.stream_error.lock().unwrap() = Some(err);
                inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start thread");
    }

    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(ref mut port) = self.port {
            return port.read(buf);
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(ref mut port) = self.port {
            return port.write(buf);
        }
        Err(std::io::Error::other("Port not available"))
    }

    fn receive_thread(
        inner: &Arc<EventsInner>,
        mut port: serialport::TTYPort,
        cancel_fd: i32,
    ) -> std::io::Result<()> {
        let port_fd = port.as_raw_fd();

        let purge_pending_data = |port: &mut serialport::TTYPort| -> std::io::Result<()> {
            let bytes_count = port.bytes_to_read()?;
            if bytes_count > 0 {
                let mut buffer = vec![0u8; bytes_count as usize];
                let did_read = port.read(&mut buffer)?;
                buffer.truncate(did_read);
                inner.in_buffer.lock().unwrap().extend(buffer);
                inner.waker.wake();
            }
            Ok(())
        };

        purge_pending_data(&mut port)?;

        loop {
            let port_fd_ = unsafe { BorrowedFd::borrow_raw(port_fd) };
            let cancel_fd_ = unsafe { BorrowedFd::borrow_raw(cancel_fd) };
            let mut poll_fds = [
                PollFd::new(port_fd_, PollFlags::POLLIN),
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

            if let Some(port_poll) = poll_fds[0].revents() {
                if port_poll.contains(PollFlags::POLLIN) {
                    purge_pending_data(&mut port)?;
                } else {
                    return Err(std::io::Error::other("port fd events != POLLIN"));
                }
            }
        }
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        if let Some(ref mut port) = self.port {
            port.flush()?;
            return Ok(());
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn clear(&mut self, buffer_to_clear: serialport::ClearBuffer) -> std::io::Result<()> {
        if let Some(ref mut port) = self.port {
            port.clear(buffer_to_clear)?;
            return Ok(());
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn set_break(&mut self) -> std::io::Result<()> {
        if let Some(ref mut port) = self.port {
            port.set_break()?;
            return Ok(());
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn clear_break(&mut self) -> std::io::Result<()> {
        if let Some(ref mut port) = self.port {
            port.clear_break()?;
            return Ok(());
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn write_request_to_send(&mut self, level: bool) -> std::io::Result<()> {
        if let Some(ref mut port) = self.port {
            port.write_request_to_send(level)?;
            return Ok(());
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn write_data_terminal_ready(&mut self, level: bool) -> std::io::Result<()> {
        if let Some(ref mut port) = self.port {
            port.write_data_terminal_ready(level)?;
            return Ok(());
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn read_clear_to_send(&mut self) -> std::io::Result<bool> {
        if let Some(ref mut port) = self.port {
            return Ok(port.read_clear_to_send()?);
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn read_data_set_ready(&mut self) -> std::io::Result<bool> {
        if let Some(ref mut port) = self.port {
            return Ok(port.read_data_set_ready()?);
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn read_ring_indicator(&mut self) -> std::io::Result<bool> {
        if let Some(ref mut port) = self.port {
            return Ok(port.read_ring_indicator()?);
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn read_carrier_detect(&mut self) -> std::io::Result<bool> {
        if let Some(ref mut port) = self.port {
            return Ok(port.read_carrier_detect()?);
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn bytes_to_read(&self) -> std::io::Result<u32> {
        if let Some(ref port) = self.port {
            return Ok(port.bytes_to_read()?);
        }
        Err(std::io::Error::other("Port not available"))
    }
}
