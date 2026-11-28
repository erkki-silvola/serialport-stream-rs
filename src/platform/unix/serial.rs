use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::path::Path;

use nix::errno::Errno;
use nix::fcntl::{fcntl, open, FcntlArg, OFlag};
use nix::libc;
use nix::sys::stat::Mode;
use nix::sys::termios;
use nix::sys::termios::FlushArg;

use crate::types::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};
use crate::SerialPortStreamBuilder;

#[cfg(not(any(
    target_os = "macos",
    all(
        target_os = "linux",
        not(any(target_arch = "powerpc", target_arch = "powerpc64"))
    )
)))]
compile_error!("serialport-stream supports Linux (non-PowerPC), macOS and Windows");

#[cfg(target_os = "linux")]
type Termios = libc::termios2;
#[cfg(target_os = "macos")]
type Termios = libc::termios;

#[cfg(target_os = "macos")]
const IOSSIOSPEED: libc::c_ulong = 0x8004_5402;

pub fn dup_cloexec(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    let raw = fcntl(fd, FcntlArg::F_DUPFD_CLOEXEC(0))?;
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

pub fn open_port(builder: &SerialPortStreamBuilder) -> io::Result<OwnedFd> {
    let path = Path::new(&builder.path);
    let fd = open(
        path,
        OFlag::O_RDWR | OFlag::O_NOCTTY | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)?;

    let raw = fd.as_raw_fd();
    let mut termios = MaybeUninit::uninit();
    Errno::result(unsafe { libc::tcgetattr(raw, termios.as_mut_ptr()) })?;
    let mut termios = unsafe { termios.assume_init() };

    termios.c_cflag |= libc::CREAD | libc::CLOCAL;
    unsafe { libc::cfmakeraw(&mut termios) };
    Errno::result(unsafe { libc::tcsetattr(raw, libc::TCSANOW, &termios) })?;

    apply_line_settings(raw, builder)?;

    let _ = set_data_terminal_ready(raw, builder.dtr_on_open);

    Ok(fd)
}

fn apply_line_settings(fd: RawFd, builder: &SerialPortStreamBuilder) -> io::Result<()> {
    let mut termios = get_termios(fd)?;
    set_parity(&mut termios, builder.parity);
    set_flow_control(&mut termios, builder.flow_control);
    set_data_bits(&mut termios, builder.data_bits);
    set_stop_bits(&mut termios, builder.stop_bits);
    #[cfg(target_os = "linux")]
    set_baud_rate_in_termios(&mut termios, builder.baud_rate);
    set_termios(fd, &termios)?;
    #[cfg(target_os = "macos")]
    set_baud_rate(fd, builder.baud_rate)?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn set_baud_rate(fd: RawFd, baud_rate: u32) -> io::Result<()> {
    let mut termios = get_termios(fd)?;
    set_baud_rate_in_termios(&mut termios, baud_rate);
    set_termios(fd, &termios)
}

#[cfg(target_os = "macos")]
pub fn set_baud_rate(fd: RawFd, baud_rate: u32) -> io::Result<()> {
    // on macos in-flight TCSANOW fails, so only set speed
    if baud_rate > 0 {
        let speed = baud_rate as libc::speed_t;
        Errno::result(unsafe { libc::ioctl(fd, IOSSIOSPEED, &speed) })?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn get_termios(fd: RawFd) -> io::Result<Termios> {
    let mut termios = MaybeUninit::uninit();
    Errno::result(unsafe { libc::ioctl(fd, libc::TCGETS2, termios.as_mut_ptr()) })?;
    Ok(unsafe { termios.assume_init() })
}

#[cfg(target_os = "macos")]
fn get_termios(fd: RawFd) -> io::Result<Termios> {
    let mut termios = MaybeUninit::uninit();
    Errno::result(unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) })?;
    Ok(unsafe { termios.assume_init() })
}

#[cfg(target_os = "linux")]
fn set_termios(fd: RawFd, termios: &Termios) -> io::Result<()> {
    Errno::result(unsafe { libc::ioctl(fd, libc::TCSETS2, termios) })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn set_termios(fd: RawFd, termios: &Termios) -> io::Result<()> {
    Errno::result(unsafe { libc::tcsetattr(fd, libc::TCSANOW, termios) })?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn set_baud_rate_in_termios(termios: &mut Termios, baud_rate: u32) {
    termios.c_cflag &= !(libc::CBAUD | libc::CIBAUD);
    termios.c_cflag |= libc::BOTHER;
    termios.c_ispeed = baud_rate;
    termios.c_ospeed = baud_rate;
}

fn set_parity(termios: &mut Termios, parity: Parity) {
    match parity {
        Parity::None => {
            termios.c_cflag &= !(libc::PARENB | libc::PARODD);
            termios.c_iflag &= !libc::INPCK;
            termios.c_iflag |= libc::IGNPAR;
        }
        Parity::Odd => {
            termios.c_cflag |= libc::PARENB | libc::PARODD;
            termios.c_iflag |= libc::INPCK;
            termios.c_iflag &= !libc::IGNPAR;
        }
        Parity::Even => {
            termios.c_cflag &= !libc::PARODD;
            termios.c_cflag |= libc::PARENB;
            termios.c_iflag |= libc::INPCK;
            termios.c_iflag &= !libc::IGNPAR;
        }
    }
}

fn set_flow_control(termios: &mut Termios, flow_control: FlowControl) {
    match flow_control {
        FlowControl::None => {
            termios.c_iflag &= !(libc::IXON | libc::IXOFF);
            termios.c_cflag &= !libc::CRTSCTS;
        }
        FlowControl::Software => {
            termios.c_iflag |= libc::IXON | libc::IXOFF;
            termios.c_cflag &= !libc::CRTSCTS;
        }
        FlowControl::Hardware => {
            termios.c_iflag &= !(libc::IXON | libc::IXOFF);
            termios.c_cflag |= libc::CRTSCTS;
        }
    }
}

fn set_data_bits(termios: &mut Termios, data_bits: DataBits) {
    let size = match data_bits {
        DataBits::Five => libc::CS5,
        DataBits::Six => libc::CS6,
        DataBits::Seven => libc::CS7,
        DataBits::Eight => libc::CS8,
    };
    termios.c_cflag &= !libc::CSIZE;
    termios.c_cflag |= size;
}

fn set_stop_bits(termios: &mut Termios, stop_bits: StopBits) {
    match stop_bits {
        StopBits::One => termios.c_cflag &= !libc::CSTOPB,
        StopBits::Two => termios.c_cflag |= libc::CSTOPB,
    }
}

fn set_data_terminal_ready(fd: RawFd, state: bool) -> io::Result<()> {
    let request = if state {
        libc::TIOCMBIS
    } else {
        libc::TIOCMBIC
    };
    let bits: libc::c_int = libc::TIOCM_DTR;
    Errno::result(unsafe { libc::ioctl(fd, request, &bits) })?;
    Ok(())
}

pub fn clear(fd: RawFd, buffer: ClearBuffer) -> io::Result<()> {
    let arg = match buffer {
        ClearBuffer::Input => FlushArg::TCIFLUSH,
        ClearBuffer::Output => FlushArg::TCOFLUSH,
        ClearBuffer::All => FlushArg::TCIOFLUSH,
    };
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    termios::tcflush(borrowed, arg).map_err(io::Error::from)
}

/// Waits until all written output has been transmitted (`tcdrain`).
pub fn flush_output(fd: RawFd) -> io::Result<()> {
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    const MAX_ATTEMPTS: u32 = 3;
    for attempt in 1..=MAX_ATTEMPTS {
        match termios::tcdrain(borrowed) {
            Ok(()) => return Ok(()),
            Err(Errno::EINTR) if attempt < MAX_ATTEMPTS => {
                #[cfg(feature = "tracing")]
                tracing::info!(attempt, "EINTR for flush");
                continue;
            }
            Err(e) => return Err(io::Error::from(e)),
        }
    }
    unreachable!()
}
