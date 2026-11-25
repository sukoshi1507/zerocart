//! Byte transport under the protocol: a real serial port, or a mock in tests.
use crate::{Error, Result};
use std::time::Duration;

pub trait Transport: Send {
    fn write_all(&mut self, data: &[u8]) -> Result<()>;
    /// Reads up to `buf.len()` bytes, waiting at most the current timeout; returns the count read.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;
    fn set_timeout(&mut self, t: Duration) -> Result<()>;
    fn set_baud(&mut self, baud: u32) -> Result<()>;
    fn clear_input(&mut self) -> Result<()>;
    /// Pulses the DTR and RTS lines. The Chisflash firmware empties its command buffer on a rising edge of
    /// either line, which is the only way to resynchronise it after an abandoned frame. Readers that do not use
    /// the control lines (GBxCart RW) keep this default, which does nothing.
    fn reset_device(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Reads exactly `buf.len()` bytes or fails with `ShortRead`.
pub fn read_exact(t: &mut dyn Transport, buf: &mut [u8]) -> Result<()> {
    let mut got = 0;
    while got < buf.len() {
        let n = t.read(&mut buf[got..])?;
        if n == 0 {
            return Err(Error::ShortRead { got, want: buf.len() });
        }
        got += n;
    }
    Ok(())
}

pub struct SerialTransport(Box<dyn serialport::SerialPort>);

impl SerialTransport {
    pub fn open(path: &str, baud: u32) -> Result<Self> {
        let p = serialport::new(path, baud).timeout(Duration::from_millis(100)).open()?;
        Ok(Self(p))
    }
}

impl Transport for SerialTransport {
    fn write_all(&mut self, data: &[u8]) -> Result<()> {
        use std::io::Write;
        self.0.write_all(data)?;
        self.0.flush()?;
        Ok(())
    }
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        use std::io::{ErrorKind, Read};
        match self.0.read(buf) {
            Ok(n) => Ok(n),
            Err(e) if e.kind() == ErrorKind::TimedOut => Ok(0),
            Err(e) => Err(e.into()),
        }
    }
    fn set_timeout(&mut self, t: Duration) -> Result<()> {
        Ok(self.0.set_timeout(t)?)
    }
    fn set_baud(&mut self, baud: u32) -> Result<()> {
        Ok(self.0.set_baud_rate(baud)?)
    }
    fn clear_input(&mut self) -> Result<()> {
        Ok(self.0.clear(serialport::ClearBuffer::Input)?)
    }
    fn reset_device(&mut self) -> Result<()> {
        // low, high, low like the reference clients, with a pause so the firmware sees each edge
        let pause = || std::thread::sleep(Duration::from_millis(20));
        self.0.write_data_terminal_ready(false)?;
        self.0.write_request_to_send(false)?;
        pause();
        self.0.write_data_terminal_ready(true)?;
        self.0.write_request_to_send(true)?;
        pause();
        self.0.write_data_terminal_ready(false)?;
        self.0.write_request_to_send(false)?;
        std::thread::sleep(Duration::from_millis(150));
        Ok(self.0.clear(serialport::ClearBuffer::Input)?)
    }
}

/// USB ids of the supported readers (GBxCart RW = CH340).
pub const USB_IDS: &[(u16, u16)] = &[(0x1A86, 0x7523)];

/// Finds `/dev/ttyUSB*` and `/dev/ttyACM*` whose USB ids match, through sysfs (no libudev needed).
pub fn find_ports() -> Vec<String> {
    find_ports_in(std::path::Path::new("/sys/class/tty"), USB_IDS)
}

pub fn find_ports_in(sys_tty: &std::path::Path, ids: &[(u16, u16)]) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(sys_tty) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("ttyUSB") || name.starts_with("ttyACM")) {
            continue;
        }
        let mut dir = e.path().join("device");
        for _ in 0..4 {
            let hex = |f: &str| {
                std::fs::read_to_string(dir.join(f)).ok().and_then(|s| u16::from_str_radix(s.trim(), 16).ok())
            };
            if let (Some(v), Some(p)) = (hex("idVendor"), hex("idProduct")) {
                if ids.contains(&(v, p)) {
                    out.push(format!("/dev/{name}"));
                }
                break;
            }
            dir = dir.join("..");
        }
    }
    out.sort();
    out
}
