//! Serial port configuration types (formerly re-exported from serialport-rs).

/// Number of data bits per character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataBits {
    Five,
    Six,
    Seven,
    Eight,
}

impl From<DataBits> for u8 {
    fn from(bits: DataBits) -> u8 {
        match bits {
            DataBits::Five => 5,
            DataBits::Six => 6,
            DataBits::Seven => 7,
            DataBits::Eight => 8,
        }
    }
}

impl TryFrom<u8> for DataBits {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            5 => Ok(DataBits::Five),
            6 => Ok(DataBits::Six),
            7 => Ok(DataBits::Seven),
            8 => Ok(DataBits::Eight),
            _ => Err(()),
        }
    }
}

/// Parity checking mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Parity {
    None,
    Odd,
    Even,
}

/// Number of stop bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopBits {
    One,
    Two,
}

impl From<StopBits> for u8 {
    fn from(bits: StopBits) -> u8 {
        match bits {
            StopBits::One => 1,
            StopBits::Two => 2,
        }
    }
}

impl TryFrom<u8> for StopBits {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(StopBits::One),
            2 => Ok(StopBits::Two),
            _ => Err(()),
        }
    }
}

/// Flow control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlowControl {
    None,
    Software,
    Hardware,
}

/// Buffer(s) to clear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClearBuffer {
    All,
    Input,
    Output,
}
