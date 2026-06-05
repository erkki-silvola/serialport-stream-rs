use std::io;
use std::sync::Arc;
use std::{mem::MaybeUninit, ptr};
use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

use crate::{EventsInner, EventsInnerWrite, PendingWrite, SerialPortStreamBuilder};

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

    /// Prepares the structure for reuse in a new overlapped operation: clears the
    /// status/offset fields (keeping the event handle) and resets the manual-reset
    /// event to the non-signaled state.
    fn reset(&mut self) -> io::Result<()> {
        let event = self.0.hEvent;
        self.0 = unsafe { std::mem::zeroed() };
        self.0.hEvent = event;
        if unsafe { ResetEvent(event as HANDLE) } == FALSE {
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

#[derive(Debug, Clone)]
struct HandleWrapper(HANDLE);

unsafe impl Send for HandleWrapper {}
unsafe impl Sync for HandleWrapper {}

/// RAII guard that closes a raw `HANDLE` on drop unless ownership is released
/// via [`HandleGuard::into_raw`]. Used during construction so that any early
/// error return closes the handles created so far instead of leaking them.
struct HandleGuard(HANDLE);

impl HandleGuard {
    fn into_raw(self) -> HANDLE {
        let handle = self.0;
        std::mem::forget(self);
        handle
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
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
    inner: Arc<EventsInner>,
    write_inner: Arc<EventsInnerWrite>,
    windows_inner: WindowsInner,
    port: Option<HandleWrapper>,
}

impl PlatformStream {
    fn port_handle(&self) -> HandleWrapper {
        self.port.as_ref().expect("port not available").clone()
    }

    pub fn new(
        builder: SerialPortStreamBuilder,
        inner: Arc<EventsInner>,
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
                format!("Failed to open port {path} reason {err}"),
            ));
        }
        // Guard the port handle so any early return below closes it.
        let handle = HandleGuard(handle);

        comm::configure_port(handle.0, &builder)?;

        if let Some(buffer) = builder.clear_buffer {
            comm::clear(handle.0, buffer)?;
        }

        // NOTE with jlinkcdc driver on windows 11 ReadTotalTimeoutMultiplier and ReadTotalTimeoutConstant needs to be max - 1
        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: 0,
            ReadTotalTimeoutMultiplier: u32::MAX,
            ReadTotalTimeoutConstant: u32::MAX,
            WriteTotalTimeoutMultiplier: 0,
            WriteTotalTimeoutConstant: 0,
        };

        if unsafe { SetCommTimeouts(handle.0, &timeouts) } == FALSE {
            let err = io::Error::last_os_error();
            return Err(std::io::Error::new(
                err.kind(),
                format!("SetCommTimeouts failed reason {err}"),
            ));
        }

        let abort_event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if abort_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let abort_event = HandleGuard(abort_event);

        let write_signal_event = unsafe { CreateEventW(ptr::null(), FALSE, FALSE, ptr::null()) };
        if write_signal_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let write_signal_event = HandleGuard(write_signal_event);

        let write_abort_event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if write_abort_event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let write_abort_event = HandleGuard(write_abort_event);

        // All fallible setup succeeded; release the guards into the struct,
        // whose own `Drop` is now responsible for closing the handles.
        Ok(Self {
            read_thread_handle: None,
            write_thread_handle: None,
            abort_event: HandleWrapper(abort_event.into_raw()),
            inner,
            write_inner,
            windows_inner: WindowsInner {
                write_signal_event: HandleWrapper(write_signal_event.into_raw()),
                write_abort_event: HandleWrapper(write_abort_event.into_raw()),
            },
            port: Some(HandleWrapper(handle.into_raw())),
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

        let inner_cloned = self.inner.clone();
        let abort_event_cloned = self.abort_event.clone();
        let read_handle = self.port_handle();
        let (tx, rx) = std::sync::mpsc::channel();

        self.read_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(e) =
                Self::receive_events(read_handle, abort_event_cloned, inner_cloned.clone())
            {
                *inner_cloned.stream_error.lock().unwrap() = Some(e);
                inner_cloned.waker.wake();
            }
        }));
        rx.recv().expect("Failed to start thread");
    }

    pub fn start_write_thread(&mut self) {
        assert!(self.write_thread_handle.is_none());

        let (tx, rx) = std::sync::mpsc::channel();
        let write_inner_cloned = self.write_inner.clone();
        let write_abort_event = self.windows_inner.write_abort_event.clone();
        let write_signal_event = self.windows_inner.write_signal_event.clone();
        let write_handle = self.port_handle();

        self.write_thread_handle = Some(std::thread::spawn(move || {
            tx.send(0).unwrap();
            if let Err(err) = Self::write_thread(
                &write_inner_cloned,
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
            unsafe { SetEvent(self.windows_inner.write_signal_event.0) },
            TRUE
        );
    }

    pub fn flush_tx_unblocked(&self) -> smol::Task<io::Result<()>> {
        let port = self.port_handle();
        smol::unblock(move || comm::flush_output(port))
    }

    fn receive_events(
        read_handle: HandleWrapper,
        abort_event: HandleWrapper,
        inner: Arc<EventsInner>,
    ) -> io::Result<()> {
        let handle = read_handle.0;

        // Reusable overlapped structures for the lifetime of the thread: one for the
        // `WaitCommEvent` wait, one for the `ReadFile` in `purge_pending_data`. Reusing
        // them (with `reset`) avoids a CreateEventW/CloseHandle pair on every event.
        let mut event_overlapped = Overlapped::new()?;
        let mut read_overlapped = Overlapped::new()?;

        // Purge any pending data first
        Self::purge_pending_data(handle, &inner, &mut read_overlapped)?;

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
                let objects = [event_overlapped.0.hEvent as HANDLE, abort_event.0];

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
                        Self::purge_pending_data(handle, &inner, &mut read_overlapped)?;
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
        inner: &Arc<EventsInner>,
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
            inner.in_buffer.lock().unwrap().extend(buf);
            inner.waker.wake();
        }
        Ok(())
    }

    fn cancel_io(handle: HANDLE, overlapped: &mut Overlapped, len: &mut u32) {
        let _ = unsafe { CancelIo(handle) };
        let _ = unsafe { GetOverlappedResult(handle, &overlapped.0, len, TRUE) };
    }

    fn write_thread(
        write_inner: &Arc<EventsInnerWrite>,
        write_handle: HandleWrapper,
        write_signal_event: HandleWrapper,
        write_abort_event: HandleWrapper,
    ) -> io::Result<()> {
        let handle = write_handle.0;

        loop {
            let objects = [write_signal_event.0, write_abort_event.0];
            match unsafe {
                WaitForMultipleObjects(objects.len() as u32, objects.as_ptr(), 0, INFINITE)
            } {
                WAIT_OBJECT_0 => {}
                val if val == WAIT_OBJECT_0 + 1 => return Ok(()),
                _ => return Err(io::Error::last_os_error()),
            }

            let pending = write_inner.pending.lock().unwrap().clone();
            let PendingWrite::Buffer(buf) = pending else {
                panic!("was waiting for PendingWrite::Buffer but got {pending:?}");
            };

            let mut overlapped = Overlapped::new()?;
            let mut bytes_written: u32 = 0;

            if unsafe {
                WriteFile(
                    handle,
                    buf.as_ptr() as *const _,
                    buf.len() as u32,
                    &mut bytes_written,
                    overlapped.as_mut_ptr(),
                )
            } == FALSE
            {
                if unsafe { GetLastError() } == ERROR_IO_PENDING {
                    let wait_objects = [overlapped.0.hEvent as HANDLE, write_abort_event.0];
                    match unsafe {
                        WaitForMultipleObjects(
                            wait_objects.len() as u32,
                            wait_objects.as_ptr(),
                            0,
                            INFINITE,
                        )
                    } {
                        WAIT_OBJECT_0 => {
                            if unsafe {
                                GetOverlappedResult(
                                    handle,
                                    overlapped.as_mut_ptr(),
                                    &mut bytes_written,
                                    1,
                                )
                            } == FALSE
                            {
                                return Err(io::Error::last_os_error());
                            }
                        }
                        val if val == WAIT_OBJECT_0 + 1 => {
                            let mut len = 0;
                            Self::cancel_io(handle, &mut overlapped, &mut len);
                            return Ok(());
                        }
                        _ => return Err(io::Error::last_os_error()),
                    }
                } else {
                    return Err(io::Error::last_os_error());
                }
            }

            let mut pending = write_inner.pending.lock().unwrap();
            *pending = PendingWrite::Completed(bytes_written as usize);
            write_inner.waker.wake();
        }
    }
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        if let Some(handle) = self.read_thread_handle.take() {
            if !handle.is_finished() {
                assert_eq!(unsafe { SetEvent(self.abort_event.0) }, TRUE);
                handle.join().unwrap();
            }
        }

        if let Some(handle) = self.write_thread_handle.take() {
            if !handle.is_finished() {
                assert_eq!(
                    unsafe { SetEvent(self.windows_inner.write_abort_event.0) },
                    TRUE
                );
                handle.join().unwrap();
            }
        }

        unsafe {
            CloseHandle(self.abort_event.0);
            CloseHandle(self.windows_inner.write_signal_event.0);
            CloseHandle(self.windows_inner.write_abort_event.0);
            if let Some(port) = self.port.take() {
                CloseHandle(port.0);
            }
        }
    }
}
