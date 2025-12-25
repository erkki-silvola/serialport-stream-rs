mod platform;
mod stream;

pub use stream::SerialPortStream;

pub use futures::stream::{Stream, TryStreamExt};
pub use serialport;
