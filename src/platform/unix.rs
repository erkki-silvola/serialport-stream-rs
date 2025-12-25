use std::io::Read;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::fd::BorrowedFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll as TaskPoll};

use futures::task::AtomicWaker;
use nix::poll::{poll, PollFd, PollFlags};
use serialport::SerialPort;

use crate::SerialPortStreamBuilder;

#[derive(Debug)]
struct EventsInner {
    in_buffer: Mutex<Vec<u8>>,
    stream_error: Mutex<Option<std::io::Error>>,
    waker: AtomicWaker,
    cancel_pipe: (OwnedFd, OwnedFd),
}

pub struct PlatformStream {
    thread_handle: Option<std::thread::JoinHandle<()>>,
    inner: Arc<EventsInner>,
    port: Option<serialport::TTYPort>,
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            if !handle.is_finished() {
                let fd = self.inner.cancel_pipe.1.as_fd();
                assert_eq!(nix::unistd::write(fd, &[1u8]).unwrap(), 1);
                handle.join().unwrap();
            }
        }
    }
}

impl PlatformStream {
    pub fn new(builder: SerialPortStreamBuilder) -> Result<Self, std::io::Error> {
        let serialport_builder = serialport::new(builder.path, builder.baud_rate)
            .timeout(builder.timeout)
            .data_bits(builder.data_bits)
            .flow_control(builder.flow_control)
            .parity(builder.parity)
            .stop_bits(builder.stop_bits)
            .dtr_on_open(builder.dtr_on_open.unwrap_or(false));

        let port = serialport_builder.open_native()?;

        let cancel_pipe = nix::unistd::pipe().unwrap();
        let inner = Arc::new(EventsInner {
            in_buffer: Mutex::new(Vec::new()),
            stream_error: Mutex::new(None),
            waker: AtomicWaker::new(),
            cancel_pipe,
        });

        Ok(Self {
            thread_handle: None,
            inner,
            port: Some(port),
        })
    }

    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(ref mut port) = self.port {
            return port.read(buf);
        }
        todo!();
    }

    pub fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(ref mut port) = self.port {
            return port.write(buf);
        }
        todo!();
    }

    fn receive_thread(
        inner: &Arc<EventsInner>,
        mut port: serialport::TTYPort,
    ) -> std::io::Result<()> {
        let port_fd = port.as_raw_fd();
        let cancel_fd = inner.cancel_pipe.0.as_raw_fd();

        loop {
            let port_fd_ = unsafe { BorrowedFd::borrow_raw(port_fd) };
            let cancel_fd_ = unsafe { BorrowedFd::borrow_raw(cancel_fd) };
            let mut poll_fds = [
                PollFd::new(port_fd_, PollFlags::POLLIN),
                PollFd::new(cancel_fd_, PollFlags::POLLIN),
            ];

            let poll_result = poll(&mut poll_fds, nix::poll::PollTimeout::NONE)?;
            match poll_result {
                1 => {
                    if let Some(cancel_poll) = poll_fds[1].revents() {
                        if cancel_poll.contains(PollFlags::POLLIN) {
                            // Cancel signal received, exit thread
                            return Ok(());
                        }
                    }

                    if let Some(port_poll) = poll_fds[0].revents() {
                        if port_poll.contains(PollFlags::POLLIN) {
                            let bytes_count = port.bytes_to_read()?;
                            let mut buffer = vec![0u8; bytes_count as usize];
                            let did_read = port.read(&mut buffer)?;
                            buffer.truncate(did_read);
                            inner.in_buffer.lock().unwrap().extend(buffer);
                            inner.waker.wake();
                        } else {
                            return Err(std::io::Error::last_os_error());
                        }
                    } else {
                        panic!("no events");
                    }
                }
                _ => {
                    return Err(std::io::Error::last_os_error());
                }
            }
        }
    }

    pub fn try_poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> TaskPoll<Option<Result<Vec<u8>, std::io::Error>>> {
        self.inner.waker.register(cx.waker());

        if let Some(err) = self.inner.stream_error.lock().unwrap().as_ref() {
            return TaskPoll::Ready(Some(Err(std::io::Error::new(err.kind(), err.to_string()))));
        }

        if self.thread_handle.is_none() {
            let (tx, rx) = mpsc::channel();
            let inner_cloned = self.inner.clone();
            let port = self.port.take().unwrap();
            self.thread_handle = Some(std::thread::spawn(move || {
                tx.send(0).unwrap();
                if let Err(err) = Self::receive_thread(&inner_cloned, port) {
                    *inner_cloned.stream_error.lock().unwrap() = Some(err);
                    inner_cloned.waker.wake();
                }
            }));
            rx.recv().expect("failed to start thread");
            return TaskPoll::Pending;
        }

        let mut buffer = self.inner.in_buffer.lock().unwrap();

        if !buffer.is_empty() {
            // Drain all available data
            let buf = buffer.drain(..).collect();
            return TaskPoll::Ready(Some(Ok(buf)));
        }

        TaskPoll::Pending
    }
}
