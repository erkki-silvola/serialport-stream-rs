use std::io;
#[cfg(feature = "stream")]
use std::mem::MaybeUninit;
use std::os::windows::io::{AsHandle, BorrowedHandle};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_io::os::windows::Waitable;
use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

#[cfg(feature = "stream")]
use crate::EventsInnerRead;
use crate::{EventsInnerWrite, SerialPortStreamBuilder};

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
    InFlight {
        overlapped: Box<Overlapped>,
        waitable: Waitable<OverlappedEvent>,
    },
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
                .map(|s| matches!(*s, WriteState::InFlight { .. })),
            Ok(true)
        );
        f.debug_struct("WriteShared")
            .field("in_flight", &in_flight)
            .finish()
    }
}

enum ReadState {
    Idle,
    InFlight {
        overlapped: Box<Overlapped>,
        waitable: Waitable<OverlappedEvent>,
    },
}

struct ReadShared {
    state: Mutex<ReadState>,
}

unsafe impl Send for ReadShared {}
unsafe impl Sync for ReadShared {}

impl std::fmt::Debug for ReadShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight = matches!(
            self.state
                .lock()
                .map(|s| matches!(*s, ReadState::InFlight { .. })),
            Ok(true)
        );
        f.debug_struct("ReadShared")
            .field("in_flight", &in_flight)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
struct OverlappedEvent(HANDLE);

impl AsHandle for OverlappedEvent {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        unsafe { BorrowedHandle::borrow_raw(self.0) }
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
pub struct PlatformStream {
    #[cfg(feature = "stream")]
    read_thread_handle: Option<std::thread::JoinHandle<()>>,
    #[cfg(feature = "stream")]
    abort_event: HandleWrapper,
    #[cfg(feature = "stream")]
    read_inner: Arc<EventsInnerRead>,
    read_shared: Arc<ReadShared>,
    read_port: HandleWrapper,
    write_shared: Arc<WriteShared>,
    write_port: HandleWrapper,
    port: Option<HandleWrapper>,
}

impl PlatformStream {
    fn port_handle(&self) -> HandleWrapper {
        self.port.as_ref().expect("port not available").clone()
    }

    pub fn new(
        builder: SerialPortStreamBuilder,
        #[cfg(feature = "stream")] read_inner: Arc<EventsInnerRead>,
        _write_inner: Arc<EventsInnerWrite>,
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

        #[cfg(feature = "stream")]
        let abort_event = {
            let abort_event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
            if abort_event.is_null() {
                return Err(io::Error::last_os_error());
            }
            HandleWrapper::new(abort_event)
        };

        let read_port = HandleWrapper::new(Self::duplicate_handle(port.raw())?);
        let write_port = HandleWrapper::new(Self::duplicate_handle(port.raw())?);

        Ok(Self {
            #[cfg(feature = "stream")]
            read_thread_handle: None,
            #[cfg(feature = "stream")]
            abort_event,
            #[cfg(feature = "stream")]
            read_inner,
            read_shared: Arc::new(ReadShared {
                state: Mutex::new(ReadState::Idle),
            }),
            read_port,
            write_shared: Arc::new(WriteShared {
                state: Mutex::new(WriteState::Idle),
            }),
            write_port,
            port: Some(port),
        })
    }

    pub fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let handle = self.read_port.raw();
        let mut state = self.read_shared.state.lock().unwrap();
        loop {
            match &mut *state {
                ReadState::Idle => {
                    let mut overlapped = match Overlapped::new() {
                        Ok(o) => o,
                        Err(e) => return Poll::Ready(Err(e)),
                    };
                    if let Err(e) = overlapped.reset() {
                        return Poll::Ready(Err(e));
                    }
                    let mut bytes_read: u32 = 0;
                    let ok = unsafe {
                        ReadFile(
                            handle,
                            buf.as_mut_ptr() as *mut _,
                            buf.len() as u32,
                            &mut bytes_read,
                            overlapped.as_mut_ptr(),
                        )
                    };
                    if ok != FALSE {
                        return Poll::Ready(Ok(bytes_read as usize));
                    }
                    if unsafe { GetLastError() } != ERROR_IO_PENDING {
                        return Poll::Ready(Err(io::Error::last_os_error()));
                    }
                    let event = overlapped.0.hEvent;
                    let waitable = match Waitable::new(OverlappedEvent(event)) {
                        Ok(w) => w,
                        Err(e) => return Poll::Ready(Err(e)),
                    };
                    *state = ReadState::InFlight {
                        overlapped: Box::new(overlapped),
                        waitable,
                    };
                }
                ReadState::InFlight {
                    waitable,
                    overlapped,
                } => {
                    return match waitable.poll_ready(cx) {
                        Poll::Pending => Poll::Pending,
                        Poll::Ready(Err(e)) => {
                            *state = ReadState::Idle;
                            Poll::Ready(Err(e))
                        }
                        Poll::Ready(Ok(())) => {
                            let mut bytes_read: u32 = 0;
                            let res = unsafe {
                                GetOverlappedResult(
                                    handle,
                                    overlapped.as_mut_ptr(),
                                    &mut bytes_read,
                                    FALSE,
                                )
                            };
                            *state = ReadState::Idle;
                            if res == FALSE {
                                Poll::Ready(Err(io::Error::last_os_error()))
                            } else {
                                Poll::Ready(Ok(bytes_read as usize))
                            }
                        }
                    };
                }
            }
        }
    }

    #[cfg(feature = "stream")]
    pub fn is_read_thread_started(&self) -> bool {
        self.read_thread_handle.is_some()
    }

    #[cfg(feature = "stream")]
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

    pub fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        let handle = self.write_port.raw();
        let mut state = self.write_shared.state.lock().unwrap();
        loop {
            match &mut *state {
                WriteState::Idle => {
                    let mut overlapped = match Overlapped::new() {
                        Ok(o) => o,
                        Err(e) => return Poll::Ready(Err(e)),
                    };
                    if let Err(e) = overlapped.reset() {
                        return Poll::Ready(Err(e));
                    }
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
                        return Poll::Ready(Ok(bytes_written as usize));
                    }
                    if unsafe { GetLastError() } != ERROR_IO_PENDING {
                        return Poll::Ready(Err(io::Error::last_os_error()));
                    }
                    let event = overlapped.0.hEvent;
                    let waitable = match Waitable::new(OverlappedEvent(event)) {
                        Ok(w) => w,
                        Err(e) => return Poll::Ready(Err(e)),
                    };
                    *state = WriteState::InFlight {
                        overlapped: Box::new(overlapped),
                        waitable,
                    };
                }
                WriteState::InFlight {
                    waitable,
                    overlapped,
                } => {
                    return match waitable.poll_ready(cx) {
                        Poll::Pending => Poll::Pending,
                        Poll::Ready(Err(e)) => {
                            *state = WriteState::Idle;
                            Poll::Ready(Err(e))
                        }
                        Poll::Ready(Ok(())) => {
                            let mut bytes_written: u32 = 0;
                            let res = unsafe {
                                GetOverlappedResult(
                                    handle,
                                    overlapped.as_mut_ptr(),
                                    &mut bytes_written,
                                    FALSE,
                                )
                            };
                            *state = WriteState::Idle;
                            if res == FALSE {
                                Poll::Ready(Err(io::Error::last_os_error()))
                            } else {
                                Poll::Ready(Ok(bytes_written as usize))
                            }
                        }
                    };
                }
            }
        }
    }

    pub fn set_baud_rate(&self, baud_rate: u32) -> io::Result<()> {
        comm::set_baud_rate(self.port_handle().raw(), baud_rate)
    }

    pub fn flush_tx_unblocked(&self) -> blocking::Task<io::Result<()>> {
        let port = self.port_handle();
        blocking::unblock(move || comm::flush_output(port))
    }

    #[cfg(feature = "stream")]
    fn receive_events(
        read_handle: HandleWrapper,
        abort_event: HandleWrapper,
        read_inner: Arc<EventsInnerRead>,
    ) -> io::Result<()> {
        let handle = read_handle.raw();

        let mut event_overlapped = Overlapped::new()?;
        let mut read_overlapped = Overlapped::new()?;
        let mut buffer = Vec::with_capacity(1024);

        // Purge any pending data first
        Self::purge_pending_data(handle, &read_inner, &mut read_overlapped, &mut buffer)?;

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
                        Self::purge_pending_data(
                            handle,
                            &read_inner,
                            &mut read_overlapped,
                            &mut buffer,
                        )?;
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

    #[cfg(feature = "stream")]
    fn purge_pending_data(
        handle: HANDLE,
        read_inner: &Arc<EventsInnerRead>,
        overlapped: &mut Overlapped,
        buffer: &mut Vec<u8>,
    ) -> io::Result<()> {
        let mut errors: u32 = 0;
        let mut comstat = MaybeUninit::<COMSTAT>::uninit();

        if unsafe { ClearCommError(handle, &mut errors, comstat.as_mut_ptr()) } == FALSE {
            return Err(io::Error::last_os_error());
        }

        let len = unsafe { comstat.assume_init() }.cbInQue;
        if len > 0 {
            buffer.resize(len as usize, 0);
            overlapped.reset()?;
            let mut bytes_read: u32 = 0;

            if unsafe {
                ReadFile(
                    handle,
                    buffer.as_mut_ptr() as *mut _,
                    buffer.len() as u32,
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

            buffer.truncate(bytes_read as usize);
            read_inner
                .in_buffer
                .lock()
                .unwrap()
                .extend_from_slice(buffer);
            buffer.clear();
            read_inner.waker.wake();
        }
        Ok(())
    }

    fn cancel_io(handle: HANDLE, overlapped: &mut Overlapped, len: &mut u32) {
        let _ = unsafe { CancelIo(handle) };
        let _ = unsafe { GetOverlappedResult(handle, &overlapped.0, len, TRUE) };
    }

    fn duplicate_handle(handle: HANDLE) -> io::Result<HANDLE> {
        let mut dup = ptr::null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                GetCurrentProcess(),
                &mut dup,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        } == FALSE
        {
            return Err(io::Error::last_os_error());
        }
        Ok(dup)
    }
}

impl Drop for PlatformStream {
    fn drop(&mut self) {
        #[cfg(feature = "stream")]
        if let Some(handle) = self.read_thread_handle.take() {
            if !handle.is_finished() {
                assert_eq!(unsafe { SetEvent(self.abort_event.raw()) }, TRUE);
                handle.join().unwrap();
            }
        }

        if let ReadState::InFlight { overlapped, .. } = &mut *self.read_shared.state.lock().unwrap()
        {
            let mut len = 0;
            Self::cancel_io(self.read_port.raw(), overlapped, &mut len);
        }

        if let WriteState::InFlight { overlapped, .. } =
            &mut *self.write_shared.state.lock().unwrap()
        {
            let mut len = 0;
            Self::cancel_io(self.write_port.raw(), overlapped, &mut len);
        }
    }
}
