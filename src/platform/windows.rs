use std::io;
use std::os::windows::prelude::*;
use std::sync::Arc;
use std::{mem::MaybeUninit, ptr};
use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

use serialport::COMPort;
use serialport::SerialPort;

use crate::{EventsInner, SerialPortStreamBuilder};

/// OVERLAPPED wrapper that manages the event handle
struct Overlapped(OVERLAPPED);

impl Overlapped {
    fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if event.is_null() {
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

pub struct PlatformStream {
    thread_handle: Option<std::thread::JoinHandle<()>>,
    abort_event: HandleWrapper,
    inner: Arc<EventsInner>,
    port: Option<serialport::COMPort>,
    timeout: std::time::Duration,
}

impl PlatformStream {
    pub fn new(builder: SerialPortStreamBuilder, inner: Arc<EventsInner>) -> io::Result<Self> {
        if builder.timeout.as_millis() >= u32::MAX as u128 {
            return Err(std::io::Error::other(
                "Invalid timeout value greater than MAX u32",
            ));
        }
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

        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: u32::MAX,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: u32::MAX,
            WriteTotalTimeoutMultiplier: u32::MAX,
            WriteTotalTimeoutConstant: u32::MAX,
        };

        if unsafe { SetCommTimeouts(handle, &timeouts) } == FALSE {
            return Err(io::Error::last_os_error());
        }

        let abort_event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
        if abort_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let abort_event = HandleWrapper(abort_event);

        Ok(Self {
            thread_handle: None,
            abort_event,
            inner,
            port: Some(com),
            timeout: builder.timeout,
        })
    }

    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(ref mut port) = self.port {
            let handle = port.as_raw_handle();

            let mut len = 0;

            let mut overlapped = Overlapped::new()?;

            match unsafe {
                ReadFile(
                    handle,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut len,
                    &mut overlapped.0,
                )
            } {
                FALSE => match unsafe { GetLastError() } {
                    ERROR_IO_PENDING => {
                        let timeout = self.timeout.as_millis() as u32;
                        return overlapped_timed(timeout, handle, &mut overlapped);
                    }
                    _ => return Err(io::Error::last_os_error()),
                },
                _ => {
                    return Ok(len as usize);
                }
            }
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(ref mut port) = self.port {
            let handle = port.as_raw_handle();

            let mut len = 0;

            let mut overlapped = Overlapped::new()?;

            match unsafe {
                WriteFile(
                    handle,
                    buf.as_ptr(),
                    buf.len() as u32,
                    &mut len,
                    &mut overlapped.0,
                )
            } {
                FALSE => match unsafe { GetLastError() } {
                    ERROR_IO_PENDING => {
                        let timeout = self.timeout.as_millis() as u32;
                        return overlapped_timed(timeout, handle, &mut overlapped);
                    }
                    _ => return Err(io::Error::last_os_error()),
                },
                _ => return Ok(len as usize),
            }
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(ref mut port) = self.port {
            let handle = port.as_raw_handle();

            match unsafe { FlushFileBuffers(handle) } {
                0 => return Err(io::Error::last_os_error()),
                _ => return Ok(()),
            }
        }
        Err(std::io::Error::other("Port not available"))
    }

    pub fn is_thread_started(&self) -> bool {
        self.thread_handle.is_some()
    }

    pub fn start_thread(&mut self) {
        assert!(self.thread_handle.is_none());

        let inner_cloned = self.inner.clone();
        let abort_event_cloned = self.abort_event.clone();
        let port = self.port.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();

        self.thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(e) = receive_events(port, abort_event_cloned, inner_cloned.clone()) {
                *inner_cloned.stream_error.lock().unwrap() = Some(e);
                inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start thread");
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
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        if let Some(handle) = self.thread_handle.take() {
            // Signal abort
            assert_eq!(unsafe { SetEvent(self.abort_event.0) }, TRUE);
            handle.join().unwrap();
        }
        unsafe {
            CloseHandle(self.abort_event.0);
        }
    }
}

fn receive_events(
    port: serialport::COMPort,
    abort_event: HandleWrapper,
    inner: Arc<EventsInner>,
) -> io::Result<()> {
    let handle = port.as_raw_handle();

    // Purge any pending data first
    purge_pending_data(handle, &inner)?;

    // Enable EV_RXCHAR event
    if unsafe { SetCommMask(handle, EV_RXCHAR) } == FALSE {
        return Err(io::Error::last_os_error());
    }

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
                    unsafe { WaitCommEvent(handle, &mut mask, overlapped.as_mut_ptr()) },
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
                            // note could check if mask == 0, but still need to wait the object signal
                            let mut len = 0;
                            if unsafe {
                                GetOverlappedResult(handle, overlapped.as_mut_ptr(), &mut len, 1)
                            } == FALSE
                            {
                                return Err(io::Error::last_os_error());
                            }
                            purge_pending_data(handle, &inner)?;
                            continue;
                        }
                        val if val == WAIT_OBJECT_0 + 1 => {
                            // Abort signaled
                            let mut len = 0;
                            cancel_io(handle, &mut overlapped, &mut len);
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
    let mut errors: u32 = 0;
    let mut comstat = MaybeUninit::<COMSTAT>::uninit();

    if unsafe { ClearCommError(handle, &mut errors, comstat.as_mut_ptr()) } == FALSE {
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
        } == FALSE
        {
            if unsafe { GetLastError() } == ERROR_IO_PENDING {
                match unsafe { WaitForSingleObject(overlapped.0.hEvent as HANDLE, INFINITE) } {
                    WAIT_OBJECT_0 => {
                        if unsafe {
                            GetOverlappedResult(handle, overlapped.as_mut_ptr(), &mut bytes_read, 1)
                        } == FALSE
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
        inner.waker.wake();
    }
    Ok(())
}

fn cancel_io(handle: HANDLE, overlapped: &mut Overlapped, len: &mut u32) {
    assert_eq!(unsafe { CancelIo(handle) }, TRUE);
    assert_eq!(
        unsafe { GetOverlappedResult(handle, &overlapped.0, len, TRUE,) },
        FALSE
    );
    assert_eq!(*len, 0);
}

fn overlapped_timed(
    timeout_ms: u32,
    handle: HANDLE,
    overlapped: &mut Overlapped,
) -> std::io::Result<usize> {
    let mut len = 0;
    match unsafe { WaitForSingleObject(overlapped.0.hEvent, timeout_ms) } as u32 {
        WAIT_OBJECT_0 => {
            if unsafe { GetOverlappedResult(handle, &overlapped.0, &mut len, TRUE) } == TRUE {
                Ok(len as usize)
            } else {
                Err(io::Error::last_os_error())
            }
        }
        WAIT_TIMEOUT => {
            cancel_io(handle, overlapped, &mut len);
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Operation timed out",
            ))
        }
        _ => Err(io::Error::last_os_error()),
    }
}
