use futures::task::AtomicWaker;
use std::io;
use std::os::windows::prelude::*;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Poll as TaskPoll};
use std::{mem::MaybeUninit, ptr};
use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::CreateEventW;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

use serialport::COMPort;
use serialport::SerialPort;
use serialport::SerialPortBuilder;

use crate::SerialPortStreamBuilder;

/// OVERLAPPED wrapper that manages the event handle
struct Overlapped(OVERLAPPED);

impl Overlapped {
    fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event == std::ptr::null_mut() {
            return Err(io::Error::last_os_error());
        }

        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = event;
        Ok(Self(overlapped))
    }

    fn as_mut_ptr(&mut self) -> *mut OVERLAPPED {
        &mut self.0
    }
}

impl Drop for Overlapped {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0.hEvent as HANDLE);
        }
    }
}

#[derive(Debug, Clone)]
struct HandleWrapper(HANDLE);

unsafe impl Send for HandleWrapper {}
unsafe impl Sync for HandleWrapper {}

#[derive(Debug)]
struct EventsInner {
    in_buffer: Mutex<Vec<u8>>,
    stream_error: Mutex<Option<io::Error>>,
    waker: AtomicWaker,
}

pub struct PlatformStream {
    thread_handle: Option<std::thread::JoinHandle<()>>,
    abort_event: HandleWrapper,
    inner: Arc<EventsInner>,
    file_handle: HandleWrapper,
    port: Option<serialport::COMPort>,
    timeout: std::time::Duration,
}

impl PlatformStream {
    pub fn new(builder: SerialPortStreamBuilder) -> io::Result<Self> {
        let path = builder.path;
        let mut name = Vec::<u16>::with_capacity(4 + path.len() + 1);

        if !path.starts_with('\\') {
            name.extend(r"\\.\".encode_utf16());
        }

        name.extend(path.encode_utf16());
        name.push(0);

        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                ptr::null_mut(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                0 as HANDLE,
            )
        };

        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }

        let mut com = unsafe { COMPort::from_raw_handle(handle as RawHandle) };

        com.set_baud_rate(builder.baud_rate)?;
        com.set_data_bits(builder.data_bits)?;
        com.set_parity(builder.parity)?;
        com.set_stop_bits(builder.stop_bits)?;
        com.set_flow_control(builder.flow_control)?;

        if let Some(dtr) = builder.dtr_on_open {
            let _ = com.write_data_terminal_ready(dtr);
        }

        let handle = HandleWrapper(handle as HANDLE);

        // Enable EV_RXCHAR event
        if unsafe { SetCommMask(handle.0, EV_RXCHAR) } == 0 {
            return Err(io::Error::last_os_error());
        }

        // Create abort event
        let abort_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if abort_event == std::ptr::null_mut() {
            return Err(io::Error::last_os_error());
        }
        let abort_event = HandleWrapper(abort_event);

        let inner = Arc::new(EventsInner {
            in_buffer: Mutex::new(Vec::new()),
            stream_error: Mutex::new(None),
            waker: AtomicWaker::new(),
        });

        Ok(Self {
            thread_handle: None,
            abort_event,
            inner,
            file_handle: handle,
            port: Some(com),
            timeout: std::time::Duration::from_secs(1),
        })
    }

    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut len = 0;

        let mut overlapped = Overlapped::new()?;

        let process_result = |len| -> io::Result<usize> {
            if len != 0 {
                return Ok(len as usize);
            }
            // if timeout occured len == 0
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Operation timed out",
            ))
        };

        match unsafe {
            ReadFile(
                self.file_handle.0,
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut len,
                &mut overlapped.0,
            )
        } {
            FALSE => match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {
                    let timeout = u128::min(self.timeout.as_millis(), INFINITE as u128 - 1) as u32;
                    match unsafe { WaitForSingleObject(overlapped.0.hEvent, timeout) } as u32 {
                        WAIT_OBJECT_0 => {
                            if unsafe {
                                GetOverlappedResult(
                                    self.file_handle.0,
                                    &mut overlapped.0,
                                    &mut len,
                                    TRUE,
                                )
                            } == TRUE
                            {
                                return process_result(len);
                            }
                            Err(io::Error::last_os_error())
                        }
                        WAIT_TIMEOUT => {
                            if unsafe { CancelIo(self.file_handle.0) } == TRUE {
                                let _ = unsafe {
                                    GetOverlappedResult(
                                        self.file_handle.0,
                                        &mut overlapped.0,
                                        &mut len,
                                        TRUE,
                                    )
                                };
                            }
                            Err(io::Error::new(
                                io::ErrorKind::TimedOut,
                                "Operation timed out",
                            ))
                        }
                        _ => Err(io::Error::last_os_error()),
                    }
                }
                _ => Err(io::Error::last_os_error()),
            },
            _ => process_result(len),
        }
    }

    pub fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut len = 0;

        let mut overlapped = Overlapped::new()?;

        match unsafe {
            WriteFile(
                self.file_handle.0,
                buf.as_ptr(),
                buf.len() as u32,
                &mut len,
                &mut overlapped.0,
            )
        } {
            FALSE => match unsafe { GetLastError() } {
                ERROR_IO_PENDING => {
                    match unsafe { WaitForSingleObject(overlapped.0.hEvent, INFINITE) } as u32 {
                        WAIT_OBJECT_0 => {
                            if unsafe {
                                GetOverlappedResult(
                                    self.file_handle.0,
                                    &mut overlapped.0,
                                    &mut len,
                                    TRUE,
                                )
                            } == TRUE
                            {
                                Ok(len as usize)
                            } else {
                                Err(io::Error::last_os_error())
                            }
                        }
                        _ => Err(io::Error::last_os_error()),
                    }
                }
                _ => Err(io::Error::last_os_error()),
            },
            _ => Ok(len as usize),
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        match unsafe { FlushFileBuffers(self.file_handle.0) } {
            0 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        }
    }

    pub fn try_poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Vec<u8>, io::Error>>> {
        self.inner.waker.register(cx.waker());

        if let Some(err) = self.inner.stream_error.lock().unwrap().as_ref() {
            return TaskPoll::Ready(Some(Err(std::io::Error::new(err.kind(), err.to_string()))));
        }

        if self.thread_handle.is_none() {
            let inner_cloned = self.inner.clone();
            let abort_event_cloned = self.abort_event.clone();
            let handle = self.file_handle.clone();
            let port = self.port.take().unwrap();
            self.thread_handle = Some(std::thread::spawn(move || {
                if let Err(e) =
                    receive_events(port, handle, abort_event_cloned, inner_cloned.clone())
                {
                    *inner_cloned.stream_error.lock().unwrap() = Some(e);
                    inner_cloned.waker.wake();
                }
            }));
        }

        // Check for data
        let mut buffer = self.inner.in_buffer.lock().unwrap();
        if !buffer.is_empty() {
            let data = buffer.drain(..).collect();
            return Poll::Ready(Some(Ok(data)));
        }

        Poll::Pending
    }
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            // Signal abort
            unsafe { SetEvent(self.abort_event.0) };
            handle.join().unwrap();
        }
        unsafe {
            CloseHandle(self.abort_event.0);
        }
    }
}

fn receive_events(
    port: serialport::COMPort,
    handle: HandleWrapper,
    abort_event: HandleWrapper,
    inner: Arc<EventsInner>,
) -> io::Result<()> {
    // Purge any pending data first
    purge_pending_data(handle.0, &inner)?;

    loop {
        // Check if abort was signaled
        match unsafe { WaitForSingleObject(abort_event.0, 0) } {
            WAIT_OBJECT_0 => {
                // Aborted
                return Ok(());
            }
            WAIT_TIMEOUT => {
                let mut overlapped = Overlapped::new()?;
                let mut mask: u32 = 0;

                assert_eq!(
                    unsafe { WaitCommEvent(handle.0, &mut mask, overlapped.as_mut_ptr()) },
                    0
                );

                if unsafe { GetLastError() } == ERROR_IO_PENDING {
                    // Wait for either comm event or abort signal
                    let objects = [overlapped.0.hEvent as HANDLE, abort_event.0];

                    match unsafe {
                        WaitForMultipleObjects(
                            objects.len() as u32,
                            objects.as_ptr(),
                            0, // Wait for any
                            INFINITE,
                        )
                    } {
                        WAIT_OBJECT_0 => {
                            // Comm event occurred
                            let mut len = 0;
                            if unsafe {
                                GetOverlappedResult(handle.0, overlapped.as_mut_ptr(), &mut len, 1)
                            } == 0
                            {
                                return Err(io::Error::last_os_error());
                            }
                            // Read the data
                            purge_pending_data(handle.0, &inner)?;
                            continue;
                        }
                        val if val == WAIT_OBJECT_0 + 1 => {
                            // Abort signaled
                            cancel_io(handle.0, &mut overlapped);
                            return Ok(());
                        }
                        _ => {
                            return Err(io::Error::last_os_error());
                        }
                    }
                } else {
                    return Err(io::Error::last_os_error());
                }
            }
            _ => {
                return Err(io::Error::last_os_error());
            }
        }
    }
}

fn purge_pending_data(handle: HANDLE, inner: &Arc<EventsInner>) -> io::Result<()> {
    loop {
        let mut errors: u32 = 0;
        let mut comstat = MaybeUninit::<COMSTAT>::uninit();

        if unsafe { ClearCommError(handle, &mut errors, comstat.as_mut_ptr()) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let len = unsafe { comstat.assume_init() }.cbInQue;
        if len > 0 {
            let mut buf = vec![0u8; len as usize];
            let mut overlapped = Overlapped::new()?;
            let mut bytes_read: u32 = 0;

            if unsafe {
                ReadFile(
                    handle,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as u32,
                    &mut bytes_read,
                    overlapped.as_mut_ptr(),
                )
            } == 0
            {
                // Check if operation is pending
                if unsafe { GetLastError() } == ERROR_IO_PENDING {
                    // Wait for the read to complete
                    match unsafe { WaitForSingleObject(overlapped.0.hEvent as HANDLE, INFINITE) } {
                        WAIT_OBJECT_0 => {
                            if unsafe {
                                GetOverlappedResult(
                                    handle,
                                    overlapped.as_mut_ptr(),
                                    &mut bytes_read,
                                    1,
                                )
                            } == 0
                            {
                                return Err(io::Error::last_os_error());
                            }
                        }
                        _ => {
                            return Err(io::Error::last_os_error());
                        }
                    }
                } else {
                    return Err(io::Error::last_os_error());
                }
            }

            buf.truncate(bytes_read as usize);
            inner.in_buffer.lock().unwrap().extend(buf);
        } else {
            inner.waker.wake();
            return Ok(());
        }
    }

    Ok(())
}

fn cancel_io(handle: HANDLE, overlapped: &mut Overlapped) {
    unsafe {
        CancelIoEx(handle, overlapped.as_mut_ptr());
        let mut len = 0;
        GetOverlappedResult(handle, overlapped.as_mut_ptr(), &mut len, 1);
    }
}
