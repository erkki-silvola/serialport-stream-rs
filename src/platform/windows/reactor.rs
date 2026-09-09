//! Thread-pool wait reactor for overlapped serial I/O on Windows.
//!
//! Registers manual-reset completion events with `RegisterWaitForSingleObject` and wakes
//! futures via [`AtomicWaker`], avoiding async-io's global reactor.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::*;

/// Manual-reset completion event integrated with the Windows thread pool.
pub struct OverlappedEvent {
    event: OwnedHandle,
    waker: AtomicWaker,
    wait_handle: AtomicPtr<std::ffi::c_void>,
}

impl OverlappedEvent {
    pub fn new() -> io::Result<Self> {
        let event = unsafe { CreateEventW(ptr::null(), TRUE, FALSE, ptr::null()) };
        if event.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            event: unsafe { OwnedHandle::from_raw_handle(event as RawHandle) },
            waker: AtomicWaker::new(),
            wait_handle: AtomicPtr::new(ptr::null_mut()),
        })
    }

    pub fn raw(&self) -> HANDLE {
        self.event.as_raw_handle() as HANDLE
    }

    pub fn reset(&self) -> io::Result<()> {
        if unsafe { ResetEvent(self.raw()) } == FALSE {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match unsafe { WaitForSingleObjectEx(self.raw(), 0, TRUE) } {
            WAIT_OBJECT_0 => Poll::Ready(Ok(())),
            WAIT_TIMEOUT => {
                self.waker.register(cx.waker());
                if let Err(err) = self.ensure_registered() {
                    return Poll::Ready(Err(err));
                }
                Poll::Pending
            }
            WAIT_FAILED => Poll::Ready(Err(io::Error::last_os_error())),
            _ => Poll::Ready(Err(io::Error::other(
                "unexpected WaitForSingleObjectEx result",
            ))),
        }
    }

    fn ensure_registered(&self) -> io::Result<()> {
        if self.wait_handle.load(Ordering::Acquire) != ptr::null_mut() {
            return Ok(());
        }

        let mut wait_handle = ptr::null_mut();
        let ok = unsafe {
            RegisterWaitForSingleObject(
                &mut wait_handle,
                self.raw(),
                Some(wait_callback),
                self as *const _ as *const std::ffi::c_void,
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if ok == FALSE {
            return Err(io::Error::last_os_error());
        }
        self.wait_handle
            .store(wait_handle as *mut std::ffi::c_void, Ordering::Release);
        Ok(())
    }

    fn clear_wait(&self) {
        let handle = self.wait_handle.swap(ptr::null_mut(), Ordering::AcqRel);
        if !handle.is_null() {
            unsafe {
                UnregisterWaitEx(handle as HANDLE, ptr::null_mut());
            }
        }
    }
}

unsafe extern "system" fn wait_callback(context: *mut std::ffi::c_void, _timed_out: bool) {
    let event = &*(context as *const OverlappedEvent);
    event.wait_handle.store(ptr::null_mut(), Ordering::Release);
    event.waker.wake();
}

impl Drop for OverlappedEvent {
    fn drop(&mut self) {
        self.clear_wait();
    }
}

pub fn new_overlapped(event: &OverlappedEvent) -> Box<OVERLAPPED> {
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event.raw();
    Box::new(overlapped)
}

pub fn poll_overlapped_result(
    port: HANDLE,
    overlapped: &mut OVERLAPPED,
    event: &OverlappedEvent,
) -> Poll<io::Result<u32>> {
    let mut bytes = 0u32;
    let res = unsafe { GetOverlappedResult(port, overlapped, &mut bytes, FALSE) };
    if res == FALSE {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
            let _ = event.reset();
            return Poll::Pending;
        }
        return Poll::Ready(Err(err));
    }
    Poll::Ready(Ok(bytes))
}
