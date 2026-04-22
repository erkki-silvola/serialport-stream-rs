use serialport::{DataBits, FlowControl, Parity, StopBits};
use std::io;

fn invalid_input(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into())
}

/// Maps `5`…`8` to [`DataBits`], same rule set as `serialport::TryFrom<u8>`.
///
/// Returns [`io::ErrorKind::InvalidInput`] when `value` is not in range.
pub fn data_bits_from_u8(value: u8) -> Result<DataBits, io::Error> {
    match value {
        5 => Ok(DataBits::Five),
        6 => Ok(DataBits::Six),
        7 => Ok(DataBits::Seven),
        8 => Ok(DataBits::Eight),
        _ => Err(invalid_input(format!(
            "invalid number of data bits: {value} (expected 5–8)"
        ))),
    }
}

/// Maps `1` or `2` to [`StopBits`], same rule set as `serialport::TryFrom<u8>`.
///
/// Returns [`io::ErrorKind::InvalidInput`] when `value` is not `1` or `2`.
pub fn stop_bits_from_u8(value: u8) -> Result<StopBits, io::Error> {
    match value {
        1 => Ok(StopBits::One),
        2 => Ok(StopBits::Two),
        _ => Err(invalid_input(format!(
            "invalid number of stop bits: {value} (expected 1 or 2)"
        ))),
    }
}

/// Parses lowercase tokens: `none`, `even`, `odd`.
///
/// Returns [`io::ErrorKind::InvalidInput`] for unrecognized `value`.
pub fn parity_from_str(value: &str) -> Result<Parity, io::Error> {
    match value {
        "none" => Ok(Parity::None),
        "even" => Ok(Parity::Even),
        "odd" => Ok(Parity::Odd),
        _ => Err(invalid_input(format!(
            "invalid parity value: {value} (expected none, even, odd)"
        ))),
    }
}

/// Parses lowercase tokens: `none`, `hardware`, `software`.
///
/// Returns [`io::ErrorKind::InvalidInput`] for unrecognized `value`.
pub fn flow_control_from_str(value: &str) -> Result<FlowControl, io::Error> {
    match value {
        "none" => Ok(FlowControl::None),
        "hardware" => Ok(FlowControl::Hardware),
        "software" => Ok(FlowControl::Software),
        _ => Err(invalid_input(format!(
            "invalid flow control value: {value} (expected none, hardware, software)"
        ))),
    }
}

/// Single-letter parity in conventional notation (`N` / `E` / `O`).
pub fn parity_to_conventional_letter(parity: Parity) -> &'static str {
    match parity {
        Parity::None => "N",
        Parity::Even => "E",
        Parity::Odd => "O",
    }
}

/// Formats `<baud>/<data bits>/<parity letter>/<stop bits count>`, e.g. `115200/8/N/1`.
pub fn format_conventional_settings(
    baud_rate: u32,
    data_bits: DataBits,
    parity: Parity,
    stop_bits: StopBits,
) -> String {
    let data = u8::from(data_bits);
    let parity_ch = parity_to_conventional_letter(parity);
    let stop = u8::from(stop_bits);
    format!("{}/{}/{}/{}", baud_rate, data, parity_ch, stop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    #[test]
    fn data_bits_round_trip() {
        for n in 5u8..=8u8 {
            let db = data_bits_from_u8(n).unwrap();
            assert_eq!(u8::from(db), n);
        }
        let err = data_bits_from_u8(4).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(err.to_string().contains("invalid number of data bits"));
    }

    #[test]
    fn stop_bits_round_trip() {
        for n in [1u8, 2u8] {
            let sb = stop_bits_from_u8(n).unwrap();
            assert_eq!(u8::from(sb), n);
        }
        let err = stop_bits_from_u8(3).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(err.to_string().contains("invalid number of stop bits"));
    }

    #[test]
    fn parity_and_flow_tokens() {
        assert_eq!(parity_from_str("none").unwrap(), Parity::None);
        assert_eq!(parity_from_str("even").unwrap(), Parity::Even);
        assert_eq!(parity_from_str("odd").unwrap(), Parity::Odd);
        assert_eq!(
            parity_from_str("None").unwrap_err().kind(),
            ErrorKind::InvalidInput
        );

        assert_eq!(flow_control_from_str("none").unwrap(), FlowControl::None);
        assert_eq!(
            flow_control_from_str("hardware").unwrap(),
            FlowControl::Hardware
        );
        assert_eq!(
            flow_control_from_str("software").unwrap(),
            FlowControl::Software
        );
    }

    #[test]
    fn conventional_format_matches_prior_behavior() {
        let s =
            format_conventional_settings(1_000_000, DataBits::Eight, Parity::None, StopBits::One);
        assert_eq!(s, "1000000/8/N/1");
    }
}
