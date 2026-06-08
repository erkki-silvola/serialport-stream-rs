use std::io;
use std::mem::MaybeUninit;

use windows_sys::Win32::Devices::Communication::*;
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;

use crate::types::{ClearBuffer, DataBits, FlowControl, Parity, StopBits};
use crate::SerialPortStreamBuilder;

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum DtrControl {
    Disable = 0x00,
    Enable = 0x01,
    Handshake = 0x02,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
enum RtsControl {
    Disable = 0x00,
    Enable = 0x01,
    Handshake = 0x02,
    Toggle = 0x03,
}

trait DcbBitField {
    fn set_f_binary(&mut self, value: bool);
    fn set_f_parity(&mut self, value: bool);
    fn set_f_outx_cts_flow(&mut self, value: bool);
    fn set_f_outx_dsr_flow(&mut self, value: bool);
    fn set_f_dtr_control(&mut self, value: DtrControl);
    fn set_f_dsr_sensitivity(&mut self, value: bool);
    fn set_f_out_x(&mut self, value: bool);
    fn set_f_in_x(&mut self, value: bool);
    fn set_f_error_char(&mut self, value: bool);
    fn set_f_null(&mut self, value: bool);
    fn set_f_rts_control(&mut self, value: RtsControl);
    fn set_f_abort_on_error(&mut self, value: bool);
}

impl DcbBitField for DCB {
    fn set_f_binary(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 0, value);
    }

    fn set_f_parity(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 1, value);
    }

    fn set_f_outx_cts_flow(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 2, value);
    }

    fn set_f_outx_dsr_flow(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 3, value);
    }

    fn set_f_dtr_control(&mut self, value: DtrControl) {
        self._bitfield &= !(0b11 << 4);
        self._bitfield |= ((value as u32) & 0b11) << 4;
    }

    fn set_f_dsr_sensitivity(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 6, value);
    }

    fn set_f_out_x(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 8, value);
    }

    fn set_f_in_x(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 9, value);
    }

    fn set_f_error_char(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 10, value);
    }

    fn set_f_null(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 11, value);
    }

    fn set_f_rts_control(&mut self, value: RtsControl) {
        self._bitfield &= !(0b11 << 12);
        self._bitfield |= ((value as u32) & 0b11) << 12;
    }

    fn set_f_abort_on_error(&mut self, value: bool) {
        set_bitfield_bit(&mut self._bitfield, 14, value);
    }
}

fn set_bitfield_bit(field: &mut u32, bit: u32, value: bool) {
    if value {
        *field |= 1 << bit;
    } else {
        *field &= !(1 << bit);
    }
}

fn get_dcb(handle: HANDLE) -> io::Result<DCB> {
    let mut dcb: DCB = unsafe { MaybeUninit::zeroed().assume_init() };
    dcb.DCBlength = std::mem::size_of::<DCB>() as u32;
    if unsafe { GetCommState(handle, &mut dcb) } != 0 {
        Ok(dcb)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set_dcb(handle: HANDLE, dcb: DCB) -> io::Result<()> {
    if unsafe { SetCommState(handle, &dcb) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn init_dcb(dcb: &mut DCB) {
    dcb.XonChar = 17;
    dcb.XoffChar = 19;
    dcb.ErrorChar = 0;
    dcb.EofChar = 26;
    dcb.set_f_binary(true);
    dcb.set_f_outx_dsr_flow(false);
    dcb.set_f_dtr_control(DtrControl::Disable);
    dcb.set_f_dsr_sensitivity(false);
    dcb.set_f_error_char(false);
    dcb.set_f_null(false);
    dcb.set_f_abort_on_error(false);
}

fn apply_line_settings(dcb: &mut DCB, builder: &SerialPortStreamBuilder) {
    dcb.BaudRate = builder.baud_rate;
    dcb.ByteSize = match builder.data_bits {
        DataBits::Five => 5,
        DataBits::Six => 6,
        DataBits::Seven => 7,
        DataBits::Eight => 8,
    };
    dcb.Parity = match builder.parity {
        Parity::None => NOPARITY,
        Parity::Odd => ODDPARITY,
        Parity::Even => EVENPARITY,
    };
    dcb.set_f_parity(builder.parity != Parity::None);
    dcb.StopBits = match builder.stop_bits {
        StopBits::One => ONESTOPBIT,
        StopBits::Two => TWOSTOPBITS,
    };
    match builder.flow_control {
        FlowControl::None => {
            dcb.set_f_outx_cts_flow(false);
            dcb.set_f_rts_control(RtsControl::Disable);
            dcb.set_f_out_x(false);
            dcb.set_f_in_x(false);
        }
        FlowControl::Software => {
            dcb.set_f_outx_cts_flow(false);
            dcb.set_f_rts_control(RtsControl::Disable);
            dcb.set_f_out_x(true);
            dcb.set_f_in_x(true);
        }
        FlowControl::Hardware => {
            dcb.set_f_outx_cts_flow(true);
            dcb.set_f_rts_control(RtsControl::Enable);
            dcb.set_f_out_x(false);
            dcb.set_f_in_x(false);
        }
    }
}

pub fn configure_port(handle: HANDLE, builder: &SerialPortStreamBuilder) -> io::Result<()> {
    let mut dcb = get_dcb(handle)?;
    init_dcb(&mut dcb);
    apply_line_settings(&mut dcb, builder);
    set_dcb(handle, dcb)?;

    set_data_terminal_ready(handle, builder.dtr_on_open)?;
    Ok(())
}

fn set_data_terminal_ready(handle: HANDLE, state: bool) -> io::Result<()> {
    let func = if state { SETDTR } else { CLRDTR };
    if unsafe { EscapeCommFunction(handle, func) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub fn clear(handle: HANDLE, buffer: ClearBuffer) -> io::Result<()> {
    let flags = match buffer {
        ClearBuffer::Input => PURGE_RXABORT | PURGE_RXCLEAR,
        ClearBuffer::Output => PURGE_TXABORT | PURGE_TXCLEAR,
        ClearBuffer::All => PURGE_RXABORT | PURGE_RXCLEAR | PURGE_TXABORT | PURGE_TXCLEAR,
    };
    if unsafe { PurgeComm(handle, flags) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Waits until buffered transmit data has been sent (`FlushFileBuffers`).
pub fn flush_output(handle: super::HandleWrapper) -> io::Result<()> {
    if unsafe { FlushFileBuffers(handle.raw()) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
