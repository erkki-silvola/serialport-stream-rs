use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::path::Path;

use nix::errno::Errno;
use nix::fcntl::{open, OFlag};
use nix::libc;
use nix::sys::stat::Mode;
use nix::sys::termios;
use nix::sys::termios::FlushArg;

use crate::types::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};
use crate::SerialPortStreamBuilder;

const TIOCMGET: libc::c_int = 0x5415;
const TIOCMSET: libc::c_int = 0x5418;
const TIOCM_DTR: libc::c_int = 0x002;

/// The integer type expected for the `request` argument of `ioctl`.
///
/// glibc/macOS/BSD use `c_ulong`, while musl and Android use `c_int`
/// (exposed as `libc::Ioctl` on linux-like targets). Casting through this
/// alias keeps the request constants well-typed on every libc.
#[cfg(any(target_os = "linux", target_os = "android"))]
type IoctlRequest = libc::Ioctl;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
type IoctlRequest = libc::c_ulong;

#[cfg(any(
    target_os = "android",
    all(
        target_os = "linux",
        not(any(target_arch = "powerpc", target_arch = "powerpc64"))
    )
))]
const TCGETS2: libc::c_ulong = 0x802c542a;
#[cfg(any(
    target_os = "android",
    all(
        target_os = "linux",
        not(any(target_arch = "powerpc", target_arch = "powerpc64"))
    )
))]
const TCSETS2: libc::c_ulong = 0x402c542b;

#[cfg(any(target_os = "ios", target_os = "macos"))]
const IOSSIOSPEED: libc::c_ulong = 0x8004_5402;

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
    set_baud_rate(&mut termios, builder.baud_rate)?;
    set_termios(fd, &termios, builder.baud_rate)
}

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "ios",
    target_os = "macos",
    target_os = "netbsd",
    target_os = "openbsd",
    all(
        target_os = "linux",
        any(target_arch = "powerpc", target_arch = "powerpc64")
    )
))]
type Termios = libc::termios;

#[cfg(any(
    target_os = "android",
    all(
        target_os = "linux",
        not(any(target_arch = "powerpc", target_arch = "powerpc64"))
    )
))]
type Termios = libc::termios2;

fn get_termios(fd: RawFd) -> io::Result<Termios> {
    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "ios",
        target_os = "macos",
        target_os = "netbsd",
        target_os = "openbsd",
        all(
            target_os = "linux",
            any(target_arch = "powerpc", target_arch = "powerpc64")
        )
    ))]
    {
        let mut termios = MaybeUninit::uninit();
        Errno::result(unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) })?;
        Ok(unsafe { termios.assume_init() })
    }

    #[cfg(any(
        target_os = "android",
        all(
            target_os = "linux",
            not(any(target_arch = "powerpc", target_arch = "powerpc64"))
        )
    ))]
    {
        let mut termios = MaybeUninit::uninit();
        let res = unsafe { libc::ioctl(fd, TCGETS2 as IoctlRequest, termios.as_mut_ptr()) };
        Errno::result(res)?;
        Ok(unsafe { termios.assume_init() })
    }
}

#[cfg(any(target_os = "ios", target_os = "macos"))]
fn set_termios(fd: RawFd, termios: &libc::termios, baud_rate: u32) -> io::Result<()> {
    Errno::result(unsafe { libc::tcsetattr(fd, libc::TCSANOW, termios) })?;
    if baud_rate > 0 {
        let speed = baud_rate as libc::speed_t;
        let res = unsafe { libc::ioctl(fd, IOSSIOSPEED as IoctlRequest, &speed) };
        Errno::result(res)?;
    }
    Ok(())
}

#[cfg(any(
    target_os = "dragonfly",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    all(
        target_os = "linux",
        any(target_arch = "powerpc", target_arch = "powerpc64")
    )
))]
fn set_termios(fd: RawFd, termios: &libc::termios, baud_rate: u32) -> io::Result<()> {
    let _ = baud_rate;
    Errno::result(unsafe { libc::tcsetattr(fd, libc::TCSANOW, termios) })?;
    Ok(())
}

#[cfg(any(
    target_os = "android",
    all(
        target_os = "linux",
        not(any(target_arch = "powerpc", target_arch = "powerpc64"))
    )
))]
fn set_termios(fd: RawFd, termios: &libc::termios2, baud_rate: u32) -> io::Result<()> {
    let _ = baud_rate;
    let res = unsafe {
        libc::ioctl(
            fd,
            TCSETS2 as IoctlRequest,
            termios as *const libc::termios2 as *mut libc::c_void,
        )
    };
    Errno::result(res)?;
    Ok(())
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

fn set_baud_rate(termios: &mut Termios, baud_rate: u32) -> io::Result<()> {
    #[cfg(any(
        target_os = "android",
        all(
            target_os = "linux",
            not(any(target_arch = "powerpc", target_arch = "powerpc64"))
        )
    ))]
    {
        termios.c_cflag &= !(libc::CBAUD | libc::CIBAUD);
        termios.c_cflag |= libc::BOTHER;
        termios.c_ispeed = baud_rate;
        termios.c_ospeed = baud_rate;
        return Ok(());
    }

    #[cfg(any(
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    ))]
    {
        Errno::result(unsafe { libc::cfsetspeed(termios, baud_rate) })?;
        return Ok(());
    }

    #[cfg(any(target_os = "ios", target_os = "macos"))]
    {
        let _ = termios;
        let _ = baud_rate;
        Ok(())
    }

    #[cfg(all(
        target_os = "linux",
        any(target_arch = "powerpc", target_arch = "powerpc64")
    ))]
    {
        let speed = linux_ppc_baud_constant(baud_rate)?;
        termios.c_cflag &= !libc::CBAUD;
        termios.c_cflag |= speed;
        termios.c_ispeed = speed;
        termios.c_ospeed = speed;
        Ok(())
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "powerpc", target_arch = "powerpc64")
))]
fn linux_ppc_baud_constant(baud_rate: u32) -> io::Result<libc::tcflag_t> {
    use libc::{
        B1000000, B110, B115200, B1152000, B1200, B134, B150, B1500000, B1800, B19200, B200,
        B2000000, B230400, B2400, B2500000, B300, B3000000, B3500000, B38400, B4000000, B460800,
        B4800, B50, B500000, B57600, B576000, B600, B75, B921600, B9600,
    };
    let speed = match baud_rate {
        50 => B50,
        75 => B75,
        110 => B110,
        134 => B134,
        150 => B150,
        200 => B200,
        300 => B300,
        600 => B600,
        1200 => B1200,
        1800 => B1800,
        2400 => B2400,
        4800 => B4800,
        9600 => B9600,
        19_200 => B19200,
        38_400 => B38400,
        57_600 => B57600,
        115_200 => B115200,
        230_400 => B230400,
        460_800 => B460800,
        500_000 => B500000,
        576_000 => B576000,
        921_600 => B921600,
        1_000_000 => B1000000,
        1_152_000 => B1152000,
        1_500_000 => B1500000,
        2_000_000 => B2000000,
        2_500_000 => B2500000,
        3_000_000 => B3000000,
        3_500_000 => B3500000,
        4_000_000 => B4000000,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported baud rate: {baud_rate}"),
            ));
        }
    };
    Ok(speed)
}

fn set_data_terminal_ready(fd: RawFd, state: bool) -> io::Result<()> {
    let mut status: libc::c_int = 0;
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    let res = unsafe { libc::ioctl(borrowed.as_raw_fd(), TIOCMGET as IoctlRequest, &mut status) };
    Errno::result(res).map_err(io::Error::from)?;
    if state {
        status |= TIOCM_DTR;
    } else {
        status &= !TIOCM_DTR;
    }
    let res = unsafe { libc::ioctl(borrowed.as_raw_fd(), TIOCMSET as IoctlRequest, &status) };
    Errno::result(res).map(|_| ()).map_err(io::Error::from)
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
                trace_info!(attempt, "EINTR for flush");
                continue;
            }
            Err(e) => return Err(io::Error::from(e)),
        }
    }
    unreachable!()
}
