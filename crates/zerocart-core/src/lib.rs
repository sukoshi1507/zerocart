//! Serial protocol, device detection and ROM header parsing for GBxCart RW (ported from FlashGBX).
pub mod chis;
pub mod db;
pub mod device;
pub mod fwupdate;
pub mod header;
pub mod krikzz;
pub mod mapper;
pub mod megadrive;
pub mod reader;
pub mod transport;

pub use device::{DumpSums, FlashProgrammer, CartMode, FirmwareInfo, GbxCart};
pub use reader::{CartKind, CartReader, ReaderInfo, ReaderKind, HEADER_LEN};
pub use transport::{Transport, find_ports};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("serial: {0}")]
    Serial(#[from] serialport::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no answer from the device")]
    NoAnswer,
    #[error("not a GBxCart RW: {0}")]
    NotSupported(&'static str),
    #[error("the device reported an error (ack {0:#04x})")]
    Nack(u8),
    /// A planned feature that has no implementation yet (the Mega Drive).
    #[error("{0} : bientôt")]
    Planned(&'static str),
    #[error("cancelled")]
    Cancelled,
    #[error("short read: got {got} of {want} bytes")]
    ShortRead { got: usize, want: usize },
}
pub type Result<T> = std::result::Result<T, Error>;
