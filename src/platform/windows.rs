use std::io;
#[cfg(feature = "stream")]
use std::mem::MaybeUninit;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;
use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::System::IO::*;

#[cfg(feature = "stream")]
use crate::EventsInnerRead;
use crate::{EventsInnerWrite, SerialPortStreamBuilder};

mod comm;

#[cfg(feature = "stream")]
/// OVERLAPPED wrapper for the `stream` feature background thread.
struct Overlapped(OVERLAPPED);

#[cfg(feature = "stream")]
impl Overlapped {
    fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
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

#[cfg(feature = "stream")]
impl Drop for Overlapped {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0.hEvent as HANDLE);
        }
    }
}

struct InFlightOp {
    _event: OwnedHandle,
    overlapped: Box<OVERLAPPED>,
}

impl InFlightOp {
    fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        let owned = unsafe { OwnedHandle::from_raw_handle(event as RawHandle) };
        let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
        overlapped.hEvent = owned.as_raw_handle() as HANDLE;
        Ok(Self {
            _event: owned,
            overlapped: Box::new(overlapped),
        })
    }

    fn event(&self) -> HANDLE {
        self.overlapped.hEvent
    }
}

struct OverlappedWait {
    waker: AtomicWaker,
    wait_handle: AtomicPtr<std::ffi::c_void>,
}

impl OverlappedWait {
    const fn new() -> Self {
        Self {
            waker: AtomicWaker::new(),
            wait_handle: AtomicPtr::new(ptr::null_mut()),
        }
    }

    fn clear(&self) {
        let handle = self.wait_handle.swap(ptr::null_mut(), Ordering::AcqRel);
        if !handle.is_null() {
            unsafe {
                UnregisterWaitEx(handle as HANDLE, ptr::null_mut());
            }
        }
    }
}

enum IoState {
    Idle,
    InFlight(InFlightOp),
}

struct WriteShared {
    state: Mutex<IoState>,
    wait: OverlappedWait,
}

unsafe impl Send for WriteShared {}
unsafe impl Sync for WriteShared {}

impl std::fmt::Debug for WriteShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight = matches!(
            self.state
                .lock()
                .map(|s| matches!(*s, IoState::InFlight(_))),
            Ok(true)
        );
        f.debug_struct("WriteShared")
            .field("in_flight", &in_flight)
            .finish()
    }
}

#[cfg(not(feature = "stream"))]
enum ReadIoState {
    Idle,
    InFlight { op: InFlightOp, buffer: Vec<u8> },
}

#[cfg(not(feature = "stream"))]
struct ReadShared {
    state: Mutex<ReadIoState>,
    wait: OverlappedWait,
}

#[cfg(not(feature = "stream"))]
unsafe impl Send for ReadShared {}
#[cfg(not(feature = "stream"))]
unsafe impl Sync for ReadShared {}

#[cfg(not(feature = "stream"))]
impl std::fmt::Debug for ReadShared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let in_flight = matches!(
            self.state
                .lock()
                .map(|s| matches!(*s, ReadIoState::InFlight { .. })),
            Ok(true)
        );
        f.debug_struct("ReadShared")
            .field("in_flight", &in_flight)
            .finish()
    }
}

/// Sole owner of a raw port `HANDLE`; closes it via `CloseHandle` on drop.
#[derive(Debug)]
struct PortHandle(HANDLE);

unsafe impl Send for PortHandle {}
unsafe impl Sync for PortHandle {}

impl Drop for PortHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

#[derive(Debug, Clone)]
struct HandleWrapper(Arc<PortHandle>);

impl HandleWrapper {
    fn new(handle: HANDLE) -> Self {
        Self(Arc::new(PortHandle(handle)))
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
    #[cfg(not(feature = "stream"))]
    read_shared: Arc<ReadShared>,
    #[cfg(not(feature = "stream"))]
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

        let timeouts = COMMTIMEOUTS {
            ReadIntervalTimeout: 1,        //u32::MAX,
            ReadTotalTimeoutMultiplier: 0, //u32::MAX,
            ReadTotalTimeoutConstant: 0,   //u32::MAX - 1,
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

        #[cfg(not(feature = "stream"))]
        let read_port = HandleWrapper::new(port.raw());
        let write_port = HandleWrapper::new(port.raw());

        Ok(Self {
            #[cfg(feature = "stream")]
            read_thread_handle: None,
            #[cfg(feature = "stream")]
            abort_event,
            #[cfg(feature = "stream")]
            read_inner,
            #[cfg(not(feature = "stream"))]
            read_shared: Arc::new(ReadShared {
                state: Mutex::new(ReadIoState::Idle),
                wait: OverlappedWait::new(),
            }),
            #[cfg(not(feature = "stream"))]
            read_port,
            write_shared: Arc::new(WriteShared {
                state: Mutex::new(IoState::Idle),
                wait: OverlappedWait::new(),
            }),
            write_port,
            port: Some(port),
        })
    }

    #[cfg(not(feature = "stream"))]
    pub fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let handle = self.read_port.raw();
        let shared = &self.read_shared;
        shared.wait.waker.register(cx.waker());
        let mut state = shared.state.lock().unwrap();
        match &mut *state {
            ReadIoState::Idle => {
                let mut op = match InFlightOp::new() {
                    Ok(v) => v,
                    Err(e) => return Poll::Ready(Err(e)),
                };
                let mut read_buf = vec![0u8; buf.len()];
                let ok = unsafe {
                    ReadFile(
                        handle,
                        read_buf.as_mut_ptr() as *mut _,
                        read_buf.len() as u32,
                        ptr::null_mut(),
                        op.overlapped.as_mut(),
                    )
                };
                if ok != FALSE {
                    let mut bytes_read: u32 = 0;
                    if unsafe {
                        GetOverlappedResult(handle, op.overlapped.as_mut(), &mut bytes_read, TRUE)
                    } == FALSE
                    {
                        return Poll::Ready(Err(io::Error::last_os_error()));
                    }
                    let n = bytes_read as usize;
                    buf[..n].copy_from_slice(&read_buf[..n]);
                    return Poll::Ready(Ok(n));
                }
                let io_err = unsafe { GetLastError() };
                if io_err != ERROR_IO_PENDING {
                    return Poll::Ready(Err(io::Error::from_raw_os_error(io_err as _)));
                }
                let event = op.event();
                if let Err(err) = register_overlapped_wait(&shared.wait, event) {
                    return Poll::Ready(Err(err));
                }
                *state = ReadIoState::InFlight {
                    op,
                    buffer: read_buf,
                };
                Poll::Pending
            }
            ReadIoState::InFlight { .. } => {
                let (mut op, read_buf) = match std::mem::replace(&mut *state, ReadIoState::Idle) {
                    ReadIoState::InFlight { op, buffer } => (op, buffer),
                    _ => unreachable!(),
                };
                match poll_in_flight(handle, &mut op) {
                    Poll::Pending => {
                        *state = ReadIoState::InFlight {
                            op,
                            buffer: read_buf,
                        };
                        Poll::Pending
                    }
                    Poll::Ready(Ok(bytes)) => {
                        shared.wait.clear();
                        let n = bytes as usize;
                        buf[..n].copy_from_slice(&read_buf[..n]);
                        Poll::Ready(Ok(n))
                    }
                    Poll::Ready(Err(err)) => {
                        shared.wait.clear();
                        cancel_overlapped(handle, op.overlapped.as_mut());
                        Poll::Ready(Err(err))
                    }
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
        let shared = &self.write_shared;
        shared.wait.waker.register(cx.waker());
        let mut state = shared.state.lock().unwrap();
        match &mut *state {
            IoState::Idle => {
                let mut op = match InFlightOp::new() {
                    Ok(v) => v,
                    Err(e) => return Poll::Ready(Err(e)),
                };
                let ok = unsafe {
                    WriteFile(
                        handle,
                        buf.as_ptr() as *const _,
                        buf.len() as u32,
                        ptr::null_mut(),
                        op.overlapped.as_mut(),
                    )
                };
                if ok != FALSE {
                    let mut bytes_written: u32 = 0;
                    if unsafe {
                        GetOverlappedResult(
                            handle,
                            op.overlapped.as_mut(),
                            &mut bytes_written,
                            TRUE,
                        )
                    } == FALSE
                    {
                        return Poll::Ready(Err(io::Error::last_os_error()));
                    }
                    return Poll::Ready(Ok(bytes_written as usize));
                }
                let io_err = unsafe { GetLastError() };
                if io_err != ERROR_IO_PENDING {
                    return Poll::Ready(Err(io::Error::from_raw_os_error(io_err as _)));
                }
                let event = op.event();
                if let Err(err) = register_overlapped_wait(&shared.wait, event) {
                    return Poll::Ready(Err(err));
                }
                *state = IoState::InFlight(op);
                Poll::Pending
            }
            IoState::InFlight(_) => {
                let mut op = match std::mem::replace(&mut *state, IoState::Idle) {
                    IoState::InFlight(op) => op,
                    _ => unreachable!(),
                };
                match poll_in_flight(handle, &mut op) {
                    Poll::Pending => {
                        *state = IoState::InFlight(op);
                        Poll::Pending
                    }
                    Poll::Ready(Ok(bytes)) => {
                        shared.wait.clear();
                        Poll::Ready(Ok(bytes as usize))
                    }
                    Poll::Ready(Err(err)) => {
                        shared.wait.clear();
                        cancel_overlapped(handle, op.overlapped.as_mut());
                        Poll::Ready(Err(err))
                    }
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
                            GetOverlappedResult(
                                handle,
                                event_overlapped.as_mut_ptr(),
                                &mut len,
                                TRUE,
                            )
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
                        cancel_overlapped(handle, event_overlapped.as_mut_ptr());
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
                                    TRUE,
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
}

unsafe extern "system" fn overlapped_wait_callback(context: *mut std::ffi::c_void, _timed_out: bool) {
    let wait = &*(context as *const OverlappedWait);
    wait.wait_handle.store(ptr::null_mut(), Ordering::Release);
    wait.waker.wake();
}

fn register_overlapped_wait(wait: &OverlappedWait, event: HANDLE) -> io::Result<()> {
    if wait.wait_handle.load(Ordering::Acquire) == ptr::null_mut() {
        let mut wait_handle = ptr::null_mut();
        let ok = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                event,
                Some(overlapped_wait_callback),
                wait as *const _ as *const std::ffi::c_void,
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if ok == FALSE {
            return Err(io::Error::last_os_error());
        }
        wait.wait_handle
            .store(wait_handle as *mut std::ffi::c_void, Ordering::Release);
    }
    Ok(())
}

fn poll_in_flight(port: HANDLE, op: &mut InFlightOp) -> Poll<io::Result<u32>> {
    let event = op.event();
    match unsafe { WaitForSingleObjectEx(event, 0, TRUE) } {
        WAIT_OBJECT_0 => finish_overlapped(port, op.overlapped.as_mut(), event),
        WAIT_TIMEOUT => Poll::Pending,
        WAIT_FAILED => Poll::Ready(Err(io::Error::last_os_error())),
        _ => Poll::Ready(Err(io::Error::other(
            "unexpected WaitForSingleObjectEx result",
        ))),
    }
}

fn finish_overlapped(
    port: HANDLE,
    overlapped: &mut OVERLAPPED,
    _event: HANDLE,
) -> Poll<io::Result<u32>> {
    let mut bytes = 0u32;
    let res = unsafe { GetOverlappedResult(port, overlapped, &mut bytes, TRUE) };
    if res == FALSE {
        let err = io::Error::last_os_error();
        return Poll::Ready(Err(err));
    }
    Poll::Ready(Ok(bytes))
}

fn cancel_overlapped(handle: HANDLE, overlapped: *mut OVERLAPPED) {
    let _c = unsafe { CancelIoEx(handle, overlapped) };
    let mut transferred = 0;
    let _g = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, TRUE) };
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

        #[cfg(not(feature = "stream"))]
        {
            self.read_shared.wait.clear();
            if let ReadIoState::InFlight { op, .. } = &mut *self.read_shared.state.lock().unwrap() {
                cancel_overlapped(self.read_port.raw(), op.overlapped.as_mut());
            }
        }

        self.write_shared.wait.clear();
        if let IoState::InFlight(op) = &mut *self.write_shared.state.lock().unwrap() {
            cancel_overlapped(self.write_port.raw(), op.overlapped.as_mut());
        }
    }
}
