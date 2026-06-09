use std::io;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::{mem::MaybeUninit, ptr};
use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

use crate::{EventsInnerRead, EventsInnerWrite, SerialPortStreamBuilder};

mod comm;

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

    fn reset(&mut self) -> io::Result<()> {
        if unsafe { ResetEvent(self.0.hEvent as HANDLE) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Overlapped {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0.hEvent as HANDLE);
        }
    }
}

enum WriteState {
    Idle,
    InFlight(Box<Overlapped>),
    Completed(usize),
}

struct WriteShared {
    state: Mutex<WriteState>,
}

unsafe impl Send for WriteShared {}
unsafe impl Sync for WriteShared {}

impl std::fmt::Debug for WriteShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight = matches!(
            self.state
                .lock()
                .map(|s| matches!(*s, WriteState::InFlight(_))),
            Ok(true)
        );
        f.debug_struct("WriteShared")
            .field("in_flight", &in_flight)
            .finish()
    }
}

/// Sole owner of a raw `HANDLE`; closes it via `CloseHandle` on drop.
#[derive(Debug)]
struct OwnedHandle(HANDLE);

unsafe impl Send for OwnedHandle {}
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[derive(Debug, Clone)]
struct HandleWrapper(Arc<OwnedHandle>);

impl HandleWrapper {
    fn new(handle: HANDLE) -> Self {
        Self(Arc::new(OwnedHandle(handle)))
    }

    fn raw(&self) -> HANDLE {
        self.0 .0
    }
}

#[derive(Debug)]
struct WindowsInner {
    write_signal_event: HandleWrapper,
    write_abort_event: HandleWrapper,
}

#[derive(Debug)]
pub struct PlatformStream {
    read_thread_handle: Option<std::thread::JoinHandle<()>>,
    write_thread_handle: Option<std::thread::JoinHandle<()>>,
    abort_event: HandleWrapper,
    read_inner: Arc<EventsInnerRead>,
    write_inner: Arc<EventsInnerWrite>,
    windows_inner: WindowsInner,
    write_shared: Arc<WriteShared>,
    port: Option<HandleWrapper>,
}

impl PlatformStream {
    fn port_handle(&self) -> HandleWrapper {
        self.port.as_ref().expect("port not available").clone()
    }

    pub fn new(
        builder: SerialPortStreamBuilder,
        read_inner: Arc<EventsInnerRead>,
        write_inner: Arc<EventsInnerWrite>,
    ) -> io::Result<Self> {
        let path = &builder.path;
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
            let err = io::Error::last_os_error();
            return Err(std::io::Error::new(
                err.kind(),
                format!("failed to open port {path}: {err}"),
            ));
        }
        // Wrap the port handle so any early return below closes it on drop.
        let port = HandleWrapper::new(handle);

        comm::configure_port(port.raw(), &builder)?;

        if let Some(buffer) = builder.clear_buffer {
            comm::clear(port.raw(), buffer)?;
        }

        // NOTE with jlinkcdc driver on windows 11 ReadTotalTimeoutMultiplier and ReadTotalTimeoutConstant needs to be max - 1
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: 0,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: u32::MAX,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 0,
        };

        if unsafe { SetCommTimeouts(port.raw(), &timeouts) } == FALSE {
            let err = io::Error::last_os_error();
            return Err(std::io::Error::new(
                err.kind(),
                format!("SetCommTimeouts failed: {err}"),
            ));
        }

        let abort_event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if abort_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let abort_event = HandleWrapper::new(abort_event);

        let write_signal_event = unsafe { CreateEventW(ptr::null(), FALSE, FALSE, ptr::null()) };
        if write_signal_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let write_signal_event = HandleWrapper::new(write_signal_event);

        let write_abort_event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if write_abort_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let write_abort_event = HandleWrapper::new(write_abort_event);

        Ok(Self {
            read_thread_handle: None,
            write_thread_handle: None,
            abort_event,
            read_inner,
            write_inner,
            windows_inner: WindowsInner {
                write_signal_event,
                write_abort_event,
            },
            write_shared: Arc::new(WriteShared {
                state: Mutex::new(WriteState::Idle),
            }),
            port: Some(port),
        })
    }

    pub fn is_read_thread_started(&self) -> bool {
        self.read_thread_handle.is_some()
    }

    pub fn is_write_thread_started(&self) -> bool {
        self.write_thread_handle.is_some()
    }

    pub fn start_read_thread(&mut self) {
        assert!(self.read_thread_handle.is_none());

        let read_inner_cloned = self.read_inner.clone();
        let abort_event_cloned = self.abort_event.clone();
        let read_handle = self.port_handle();
        let (tx, rx) = std::sync::mpsc::channel();

        self.read_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(e) =
                Self::receive_events(read_handle, abort_event_cloned, read_inner_cloned.clone())
            {
                *read_inner_cloned.stream_error.lock().unwrap() = Some(e);
                read_inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start thread");
    }

    pub fn start_write_thread(&mut self) {
        assert!(self.write_thread_handle.is_none());

        let (tx, rx) = std::sync::mpsc::channel();
        let write_inner_cloned = self.write_inner.clone();
        let write_shared = self.write_shared.clone();
        let write_abort_event = self.windows_inner.write_abort_event.clone();
        let write_signal_event = self.windows_inner.write_signal_event.clone();
        let write_handle = self.port_handle();

        self.write_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(err) = Self::write_thread(
                &write_inner_cloned,
                &write_shared,
                write_handle,
                write_signal_event,
                write_abort_event,
            ) {
                *write_inner_cloned.write_error.lock().unwrap() = Some(err);
                write_inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start write thread");
    }

    pub fn signal_write(&self) {
        assert_eq!(
            unsafe { SetEvent(self.windows_inner.write_signal_event.raw()) },
            TRUE
        );
    }

    pub fn poll_write(&mut self, buf: &[u8]) -> Poll<io::Result<usize>> {
        let mut state = self.write_shared.state.lock().unwrap();
        match *state {
            WriteState::Idle => {
                let handle = self.port_handle().raw();
                let mut overlapped = match Overlapped::new() {
                    Ok(o) => Box::new(o),
                    Err(e) => return Poll::Ready(Err(e)),
                };
                let mut bytes_written: u32 = 0;
                let ok = unsafe {
                    WriteFile(
                        handle,
                        buf.as_ptr() as *const _,
                        buf.len() as u32,
                        &mut bytes_written,
                        overlapped.as_mut_ptr(),
                    )
                };
                if ok != FALSE {
                    // Completed synchronously; overlapped is dropped here.
                    return Poll::Ready(Ok(bytes_written as usize));
                }
                if unsafe { GetLastError() } == ERROR_IO_PENDING {
                    *state = WriteState::InFlight(overlapped);
                    drop(state);
                    self.signal_write();
                    Poll::Pending
                } else {
                    Poll::Ready(Err(io::Error::last_os_error()))
                }
            }
            WriteState::InFlight(_) => Poll::Pending,
            WriteState::Completed(n) => {
                *state = WriteState::Idle;
                Poll::Ready(Ok(n))
            }
        }
    }

    pub fn flush_tx_unblocked(&self) -> blocking::Task<io::Result<()>> {
        let port = self.port_handle();
        blocking::unblock(move || comm::flush_output(port))
    }

    fn receive_events(
        read_handle: HandleWrapper,
        abort_event: HandleWrapper,
        read_inner: Arc<EventsInnerRead>,
    ) -> io::Result<()> {
        let handle = read_handle.raw();

        let mut event_overlapped = Overlapped::new()?;
        let mut read_overlapped = Overlapped::new()?;

        // Purge any pending data first
        Self::purge_pending_data(handle, &read_inner, &mut read_overlapped)?;

        // Enable EV_RXCHAR event
        if unsafe { SetCommMask(handle, EV_RXCHAR) } == FALSE {
            return Err(io::Error::last_os_error());
        }

        loop {
            event_overlapped.reset()?;
            let mut mask: u32 = 0;

            assert_eq!(
                unsafe { WaitCommEvent(handle, &mut mask, event_overlapped.as_mut_ptr()) },
                0
            );

            if unsafe { GetLastError() } == ERROR_IO_PENDING {
                // Wait for either comm event or abort signal
                let objects = [event_overlapped.0.hEvent as HANDLE, abort_event.raw()];

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
                            GetOverlappedResult(handle, event_overlapped.as_mut_ptr(), &mut len, 1)
                        } == FALSE
                        {
                            return Err(io::Error::last_os_error());
                        }
                        Self::purge_pending_data(handle, &read_inner, &mut read_overlapped)?;
                        continue;
                    }
                    val if val == WAIT_OBJECT_0 + 1 => {
                        // Abort signaled
                        let mut len = 0;
                        Self::cancel_io(handle, &mut event_overlapped, &mut len);
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
    }

    fn purge_pending_data(
        handle: HANDLE,
        read_inner: &Arc<EventsInnerRead>,
        overlapped: &mut Overlapped,
    ) -> io::Result<()> {
        let mut errors: u32 = 0;
        let mut comstat = MaybeUninit::<COMSTAT>::uninit();

        if unsafe { ClearCommError(handle, &mut errors, comstat.as_mut_ptr()) } == FALSE {
            return Err(io::Error::last_os_error());
        }

        let len = unsafe { comstat.assume_init() }.cbInQue;
        if len > 0 {
            let mut buf = vec![0u8; len as usize];
            overlapped.reset()?;
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
                                GetOverlappedResult(
                                    handle,
                                    overlapped.as_mut_ptr(),
                                    &mut bytes_read,
                                    1,
                                )
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
            read_inner.in_buffer.lock().unwrap().extend(buf);
            read_inner.waker.wake();
        }
        Ok(())
    }

    fn cancel_io(handle: HANDLE, overlapped: &mut Overlapped, len: &mut u32) {
        let _ = unsafe { CancelIo(handle) };
        let _ = unsafe { GetOverlappedResult(handle, &overlapped.0, len, TRUE) };
    }

    fn write_thread(
        write_inner: &Arc<EventsInnerWrite>,
        write_shared: &Arc<WriteShared>,
        write_handle: HandleWrapper,
        write_signal_event: HandleWrapper,
        write_abort_event: HandleWrapper,
    ) -> io::Result<()> {
        let handle = write_handle.raw();

        loop {
            let objects = [write_signal_event.raw(), write_abort_event.raw()];
            match unsafe {
                WaitForMultipleObjects(objects.len() as u32, objects.as_ptr(), 0, INFINITE)
            } {
                WAIT_OBJECT_0 => {}
                val if val == WAIT_OBJECT_0 + 1 => return Ok(()),
                _ => return Err(io::Error::last_os_error()),
            }

            let (event, overlapped_ptr) = {
                let mut state = write_shared.state.lock().unwrap();
                match &mut *state {
                    WriteState::InFlight(overlapped) => {
                        (overlapped.0.hEvent as HANDLE, overlapped.as_mut_ptr())
                    }
                    // Spurious wakeup (e.g. a write that completed synchronously); nothing to await.
                    _ => panic!("was waiting InFlight"),
                }
            };

            let wait_objects = [event, write_abort_event.raw()];
            match unsafe {
                WaitForMultipleObjects(
                    wait_objects.len() as u32,
                    wait_objects.as_ptr(),
                    0,
                    INFINITE,
                )
            } {
                WAIT_OBJECT_0 => {
                    let mut bytes_written: u32 = 0;
                    let res = unsafe {
                        GetOverlappedResult(handle, overlapped_ptr, &mut bytes_written, 1)
                    };
                    if res == FALSE {
                        *write_shared.state.lock().unwrap() = WriteState::Idle;
                        return Err(io::Error::last_os_error());
                    }
                    *write_shared.state.lock().unwrap() =
                        WriteState::Completed(bytes_written as usize);
                    write_inner.waker.wake();
                }
                val if val == WAIT_OBJECT_0 + 1 => {
                    unsafe { PurgeComm(handle, PURGE_TXABORT | PURGE_TXCLEAR) };
                    let mut len = 0;
                    let _ = unsafe { CancelIoEx(handle, overlapped_ptr) };
                    let _ = unsafe { GetOverlappedResult(handle, overlapped_ptr, &mut len, TRUE) };
                    *write_shared.state.lock().unwrap() = WriteState::Idle;
                    return Ok(());
                }
                _ => return Err(io::Error::last_os_error()),
            }
        }
    }
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        if let Some(handle) = self.read_thread_handle.take() {
            if !handle.is_finished() {
                assert_eq!(unsafe { SetEvent(self.abort_event.raw()) }, TRUE);
                handle.join().unwrap();
            }
        }

        if let Some(handle) = self.write_thread_handle.take() {
            if !handle.is_finished() {
                assert_eq!(
                    unsafe { SetEvent(self.windows_inner.write_abort_event.raw()) },
                    TRUE
                );
                handle.join().unwrap();
            }
        }
    }
}
