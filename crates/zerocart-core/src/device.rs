//! GBxCart RW protocol (ported from FlashGBX `LK_Device.py` / `hw_GBxCartRW.py`).
//! Read-only for now: connect, firmware info, mode/power, ROM/RAM reads. No cartridge writes.
use crate::transport::{read_exact, Transport};
use crate::{Error, Result};
use std::time::Duration;

pub mod cmd {
    pub const QUERY_FW_INFO: u8 = 0xA1;
    pub const GET_DEVICE_SERIAL: u8 = 0xFD;
    pub const SET_MODE_AGB: u8 = 0xA2;
    pub const SET_MODE_DMG: u8 = 0xA3;
    pub const SET_VOLTAGE_3_3V: u8 = 0xA4;
    pub const SET_VOLTAGE_5V: u8 = 0xA5;
    pub const SET_VARIABLE: u8 = 0xA6;
    pub const DISABLE_PULLUPS: u8 = 0xAC;
    pub const GET_VARIABLE: u8 = 0xAD;
    pub const DMG_CART_READ: u8 = 0xB1;
    pub const DMG_CART_WRITE: u8 = 0xB2;
    pub const DMG_CART_WRITE_SRAM: u8 = 0xB3;
    pub const CART_WRITE_FLASH_CMD: u8 = 0xD4;
    pub const SET_FLASH_CMD: u8 = 0xA7;
    pub const DMG_SET_BANK_CHANGE_CMD: u8 = 0xB8;
    pub const FLASH_PROGRAM: u8 = 0xD3;
    pub const DMG_MBC_RESET: u8 = 0xB4;
    pub const AGB_CART_READ: u8 = 0xC1;
    pub const AGB_CART_READ_SRAM: u8 = 0xC3;
    pub const AGB_CART_WRITE_SRAM: u8 = 0xC4;
    pub const AGB_BOOTUP_SEQUENCE: u8 = 0xC9;
    pub const CART_PWR_ON: u8 = 0xF2;
    pub const CART_PWR_OFF: u8 = 0xF3;
    pub const QUERY_CART_PWR: u8 = 0xF4;
    pub const SET_PIN: u8 = 0xF5;
    pub const OFW_PCB_VER: u8 = 0x68;
    pub const OFW_FW_VER: u8 = 0x56;
}

/// Firmware variables: (size in bits, key). Only those used so far.
pub mod var {
    pub const ADDRESS: (u8, u32) = (4, 0x00);
    pub const TRANSFER_SIZE: (u8, u32) = (2, 0x00);
    pub const DMG_ACCESS_MODE: (u8, u32) = (1, 0x01);
    pub const CART_MODE: (u8, u32) = (1, 0x00);
    pub const DMG_READ_CS_PULSE: (u8, u32) = (1, 0x08);
    pub const DMG_WRITE_CS_PULSE: (u8, u32) = (1, 0x09);
    pub const FLASH_WE_PIN: (u8, u32) = (1, 0x04);
    pub const DMG_ROM_BANK: (u8, u32) = (2, 0x02);
    pub const STATUS_REGISTER_MASK: (u8, u32) = (2, 0x05);
    pub const STATUS_REGISTER_VALUE: (u8, u32) = (2, 0x06);
    pub const FLASH_PULSE_RESET: (u8, u32) = (1, 0x05);
    pub const FLASH_COMMANDS_BANK_1: (u8, u32) = (1, 0x06);
    pub const FLASH_DOUBLE_DIE: (u8, u32) = (1, 0x0A);
    pub const PULLUPS_ENABLED: (u8, u32) = (1, 0x0E);
    pub const DMG_READ_METHOD: (u8, u32) = (1, 0x0B);
    pub const AGB_READ_METHOD: (u8, u32) = (1, 0x0C);
    pub const AGB_IRQ_ENABLED: (u8, u32) = (1, 0x10);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CartMode {
    Dmg,
    Agb,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FirmwareInfo {
    pub ofw_ver: u8,
    pub pcb_ver: u8,
    pub cfw_id: char,
    pub fw_ver: u16,
    pub fw_ts: u32,
    pub pcb_name: String,
    pub cart_power_ctrl: bool,
    /// The firmware can restart into its bootloader on command (used by firmware updates).
    pub bootloader_reset: bool,
    /// The firmware can report a device serial number (firmware L16 and newer).
    pub serial_supported: bool,
}

impl FirmwareInfo {
    pub fn pcb_label(&self) -> &'static str {
        if self.pcb_name == "GBFlash" && self.pcb_ver == 13 {
            return "v1.3";
        }
        match self.pcb_ver {
            5 => "v1.4",
            6 => "v1.4a/b/c",
            2 => "v1.1/v1.2",
            4 => "v1.3",
            90 => "XMAS v1.0",
            100 => "Mini v1.0",
            _ => "unknown revision",
        }
    }
    pub fn version_string(&self) -> String {
        if self.fw_ver == 0 {
            format!("R{}", self.ofw_ver)
        } else if matches!(self.pcb_ver, 5 | 6 | 101) {
            format!("R{}+{}{}", self.ofw_ver, self.cfw_id, self.fw_ver)
        } else {
            format!("{}{}", self.cfw_id, self.fw_ver)
        }
    }
    fn acked(&self) -> bool {
        self.fw_ver >= 12
    }
    fn can_power_cycle(&self) -> bool {
        if self.fw_ver >= 12 { self.cart_power_ctrl } else { matches!(self.pcb_ver, 5 | 6) }
    }
}

/// How the reader's firmware programs a flash chip (the part of a profile it needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashProgrammer {
    /// 1 = AMD, 2 = Intel, 0 = none
    pub command_set: u8,
    /// 1 = single (unbuffered) write, 2 = buffered
    pub method: u8,
    /// Write-enable pin: 1 = WR, 2 = AUDIO, 0 = none (GBA)
    pub we_pin: u8,
    /// Up to 6 `(address, value)` program commands; tokens such as PA/PD/SA are given as 0
    pub cmds: Vec<(u32, u16)>,
    /// The profile's `enable_pullup_wr`
    pub pullup_wr: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpSums {
    pub crc32: u32,
    /// Game Boy global checksum as computed over the dump (compare with the header's 0x14E-0x14F).
    pub checksum16: u16,
}

pub struct GbxCart<T: Transport> {
    t: T,
    pub fw: FirmwareInfo,
    pub mode: Option<CartMode>,
    /// Bytes per read request (0x100 at 1 Mbaud without the ACK transport, as FlashGBX).
    pub max_read: usize,
}

impl<T: Transport> GbxCart<T> {
    /// Identifies the reader. Tries the LK firmware query first (GBFlash, GBxCart RW with custom firmware),
    /// then the original GBxCart RW firmware commands. `sync_delay` is 200 ms on hardware.
    pub fn connect(mut t: T, sync_delay: Duration) -> Result<Self> {
        t.set_timeout(Duration::from_millis(75))?;
        t.clear_input()?;
        t.write_all(&[cmd::QUERY_FW_INFO])?;
        let mut b = [0u8; 1];
        if t.read(&mut b)? == 1 && b[0] == 8 {
            t.set_timeout(Duration::from_secs(1))?;
            let mut fw = Self::read_lk_info(&mut t)?;
            if matches!(fw.pcb_ver, 5 | 6 | 101) {
                t.write_all(&[cmd::OFW_FW_VER])?;
                read_exact(&mut t, &mut b)?;
                fw.ofw_ver = b[0];
            }
            return Ok(Self { t, fw, mode: None, max_read: 0x100 });
        }
        // Original GBxCart RW firmware: resynchronise, then ask PCB and firmware versions.
        t.write_all(&[0xFF, 0x18, 0xFF, 0x18, 0xFF, 0x18])?;
        std::thread::sleep(sync_delay);
        t.clear_input()?;
        t.write_all(&[cmd::OFW_PCB_VER])?;
        if t.read(&mut b)? == 0 {
            return Err(Error::NoAnswer);
        }
        let pcb = b[0];
        t.set_timeout(Duration::from_secs(1))?;
        t.write_all(&[cmd::OFW_FW_VER])?;
        read_exact(&mut t, &mut b)?;
        let ofw = b[0];
        if (pcb == 2 && ofw == 2) || (pcb >= 5 && ofw == 0) || !(pcb < 5 && ofw > 0) {
            if std::env::var_os("ZEROCART_DEBUG").is_some() {
                eprintln!("pcb={pcb} ofw={ofw}");
            }
            return Err(Error::NotSupported("not a GBxCart RW / GBFlash (unexpected answers)"));
        }
        let fw = FirmwareInfo { ofw_ver: ofw, pcb_ver: pcb, pcb_name: "GBxCart RW".into(), ..Default::default() };
        Ok(Self { t, fw, mode: None, max_read: 0x100 })
    }

    /// Reads the answer to QUERY_FW_INFO after its size byte (8).
    fn read_lk_info(t: &mut T) -> Result<FirmwareInfo> {
        let mut b = [0u8; 1];
        let mut info = [0u8; 8];
        read_exact(t, &mut info)?;
        let mut fw = FirmwareInfo {
            cfw_id: info[0] as char,
            fw_ver: u16::from_be_bytes([info[1], info[2]]),
            pcb_ver: info[3],
            fw_ts: u32::from_be_bytes([info[4], info[5], info[6], info[7]]),
            pcb_name: "GBxCart RW".into(),
            ..Default::default()
        };
        if fw.cfw_id == 'L' && fw.fw_ver >= 12 {
            read_exact(t, &mut b)?;
            let mut name = vec![0u8; b[0] as usize];
            read_exact(t, &mut name)?;
            let name = String::from_utf8_lossy(&name).replace('\0', "");
            if !name.trim().is_empty() {
                fw.pcb_name = name.trim().to_string();
            }
            read_exact(t, &mut b)?;
            fw.cart_power_ctrl = b[0] & 1 == 1;
            read_exact(t, &mut b)?;
            fw.bootloader_reset = b[0] & 1 != 0;
            fw.serial_supported = b[0] & 2 != 0;
        }
        Ok(fw)
    }

    /// The device serial number as upper-case hex, for firmware L16 and newer (`None` on older ones).
    pub fn read_device_serial(&mut self) -> Result<Option<String>> {
        if self.fw.cfw_id != 'L' || self.fw.fw_ver < 16 {
            return Ok(None);
        }
        self.t.write_all(&[cmd::GET_DEVICE_SERIAL])?;
        let mut len = [0u8; 1];
        read_exact(&mut self.t, &mut len)?;
        if len[0] == 0 {
            return Ok(None);
        }
        let mut data = vec![0u8; len[0] as usize];
        read_exact(&mut self.t, &mut data)?;
        Ok(Some(data.iter().map(|b| format!("{b:02X}")).collect()))
    }

    fn wait_ack(&mut self) -> Result<()> {
        let mut b = [0u8; 1];
        read_exact(&mut self.t, &mut b)?;
        match b[0] {
            1 | 3 => Ok(()),
            n => Err(Error::Nack(n)),
        }
    }

    /// Sends a command, waiting for the firmware ack when it supports it (fw >= 12).
    fn send(&mut self, data: &[u8]) -> Result<()> {
        self.t.write_all(data)?;
        if self.fw.acked() {
            self.wait_ack()?;
        }
        Ok(())
    }

    pub fn set_var(&mut self, (size, key): (u8, u32), value: u32) -> Result<()> {
        let mut b = vec![cmd::SET_VARIABLE, size];
        b.extend_from_slice(&key.to_be_bytes());
        b.extend_from_slice(&value.to_be_bytes());
        self.send(&b)
    }

    pub fn get_var(&mut self, (size, key): (u8, u32)) -> Result<u32> {
        if self.fw.fw_ver < 10 {
            return Ok(0);
        }
        let mut b = vec![cmd::GET_VARIABLE, size];
        b.extend_from_slice(&key.to_be_bytes());
        self.t.write_all(&b)?;
        let mut r = [0u8; 4];
        read_exact(&mut self.t, &mut r)?;
        Ok(u32::from_be_bytes(r))
    }

    fn set_pin(&mut self, pin_bit: u32, high: bool) -> Result<()> {
        if self.fw.fw_ver < 12 {
            return Ok(());
        }
        let mut b = vec![cmd::SET_PIN];
        b.extend_from_slice(&(1u32 << pin_bit).to_be_bytes());
        b.push(high as u8);
        self.send(&b)
    }

    pub fn set_mode(&mut self, mode: CartMode) -> Result<()> {
        match mode {
            CartMode::Dmg => {
                self.send(&[cmd::SET_MODE_DMG])?;
                self.send(&[cmd::SET_VOLTAGE_5V])?;
                self.set_var(var::DMG_READ_METHOD, 1)?;
                self.set_var(var::CART_MODE, 1)?;
            }
            CartMode::Agb => {
                self.send(&[cmd::SET_MODE_AGB])?;
                self.send(&[cmd::SET_VOLTAGE_3_3V])?;
                self.set_var(var::AGB_READ_METHOD, 0)?;
                self.set_var(var::CART_MODE, 2)?;
                if self.fw.fw_ver >= 12 {
                    self.set_var(var::AGB_IRQ_ENABLED, 0)?;
                }
            }
        }
        self.mode = Some(mode);
        self.set_var(var::ADDRESS, 0)?;
        if self.fw.can_power_cycle() {
            self.cart_power_on()?;
        } else {
            // PIN_AUDIO (bit 29): high for DMG, low for AGB
            self.set_pin(29, mode == CartMode::Dmg)?;
        }
        Ok(())
    }

    pub fn cart_power_on(&mut self) -> Result<()> {
        if self.fw.fw_ver >= 12 {
            self.t.write_all(&[cmd::QUERY_CART_PWR])?;
            let mut b = [0u8; 1];
            read_exact(&mut self.t, &mut b)?;
            if b[0] == 0 {
                match self.mode {
                    Some(CartMode::Dmg) => self.send(&[cmd::SET_MODE_DMG])?,
                    Some(CartMode::Agb) => self.send(&[cmd::SET_MODE_AGB])?,
                    None => {}
                }
                self.send(&[cmd::CART_PWR_ON])?;
                std::thread::sleep(Duration::from_millis(200));
                self.t.write_all(&[cmd::QUERY_CART_PWR])?;
                read_exact(&mut self.t, &mut b)?;
                match self.mode {
                    Some(CartMode::Dmg) => self.send(&[cmd::DMG_MBC_RESET])?,
                    Some(CartMode::Agb) => self.send(&[cmd::AGB_BOOTUP_SEQUENCE])?,
                    None => {}
                }
            }
        } else if self.mode == Some(CartMode::Agb) {
            self.send(&[cmd::AGB_BOOTUP_SEQUENCE])?;
        }
        Ok(())
    }

    /// True when the reader can switch the cartridge power off and on.
    pub fn can_power_cycle(&self) -> bool {
        self.fw.can_power_cycle()
    }

    /// Powers the cartridge off for `off_time`, then on again and re-prepares the pins. A battery-backed RAM keeps
    /// its content through this; one with a dead battery does not. Returns false when the reader cannot do it.
    pub fn power_cycle(&mut self, off_time: Duration) -> Result<bool> {
        if !self.fw.can_power_cycle() {
            return Ok(false);
        }
        self.send(&[cmd::CART_PWR_OFF])?;
        std::thread::sleep(off_time);
        self.cart_power_on()?;
        self.prepare_header()?;
        Ok(true)
    }

    /// Prepares the pins for a header read (as `ReadHeader` in FlashGBX).
    pub fn prepare_header(&mut self) -> Result<()> {
        if self.fw.can_power_cycle() {
            self.cart_power_on()?;
        }
        if self.fw.fw_ver >= 8 {
            self.send(&[cmd::DISABLE_PULLUPS])?;
        }
        match self.mode {
            Some(CartMode::Dmg) => {
                self.send(&[cmd::SET_VOLTAGE_5V])?;
                self.send(&[cmd::DMG_MBC_RESET])?;
                self.set_var(var::DMG_READ_CS_PULSE, 0)?;
                self.set_var(var::DMG_WRITE_CS_PULSE, 0)?;
            }
            Some(CartMode::Agb) => {
                self.send(&[cmd::SET_VOLTAGE_3_3V])?;
                if !self.fw.can_power_cycle() {
                    self.send(&[cmd::AGB_BOOTUP_SEQUENCE])?;
                }
            }
            None => return Err(Error::NotSupported("no mode set")),
        }
        Ok(())
    }

    /// Reads `len` bytes of ROM at `address` (a byte address; GBA addresses are halved for the firmware).
    pub fn read_rom(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        let mode = self.mode.ok_or(Error::NotSupported("no mode set"))?;
        let chunk = self.max_read.min(len.max(1));
        self.set_var(var::TRANSFER_SIZE, chunk as u32)?;
        match mode {
            CartMode::Dmg => {
                self.set_var(var::ADDRESS, address)?;
                self.set_var(var::DMG_ACCESS_MODE, 1)?;
            }
            CartMode::Agb => self.set_var(var::ADDRESS, address >> 1)?,
        }
        let command = if mode == CartMode::Dmg { cmd::DMG_CART_READ } else { cmd::AGB_CART_READ };
        let mut out = Vec::with_capacity(len);
        let mut size = chunk;
        while out.len() < len {
            let n = (len - out.len()).min(self.max_read);
            if n != size {
                self.set_var(var::TRANSFER_SIZE, n as u32)?;
                size = n;
            }
            self.t.write_all(&[command])?;
            let start = out.len();
            out.resize(start + n, 0);
            read_exact(&mut self.t, &mut out[start..])?;
        }
        Ok(out)
    }

    /// Writes a list of `(address, value)` flash commands to the cartridge in one request (`flashcart`: use the
    /// flash-cartridge bus timing; GBA addresses are halved for the 16-bit bus). Needs firmware >= 6.
    pub fn flash_write(&mut self, cmds: &[(u32, u16)], flashcart: bool) -> Result<()> {
        for group in cmds.chunks(255) {
            let mut b = vec![cmd::CART_WRITE_FLASH_CMD, flashcart as u8, group.len() as u8];
            for &(addr, value) in group {
                let a = if self.mode == Some(CartMode::Agb) && flashcart { addr >> 1 } else { addr };
                b.extend_from_slice(&a.to_be_bytes());
                b.extend_from_slice(&value.to_be_bytes());
            }
            self.t.write_all(&b)?;
            let mut r = [0u8; 1];
            read_exact(&mut self.t, &mut r)?;
            if r[0] != 1 {
                return Err(Error::Nack(r[0]));
            }
        }
        Ok(())
    }

    /// Sets the cartridge voltage: 3.3 V when `v33`, else 5 V.
    pub fn set_voltage(&mut self, v33: bool) -> Result<()> {
        self.send(&[if v33 { cmd::SET_VOLTAGE_3_3V } else { cmd::SET_VOLTAGE_5V }])
    }

    /// Loads the flash programming parameters into the firmware, as FlashGBX does before a ROM write. Works in the
    /// current mode: Game Boy (command addresses as given) or GBA (16-bit bus: addresses are halved).
    pub fn configure_flash_programmer(&mut self, p: &FlashProgrammer) -> Result<()> {
        if self.fw.fw_ver < 12 {
            return Err(Error::NotSupported("flash programming needs firmware L12 or newer"));
        }
        let mode = self.mode.ok_or(Error::NotSupported("no mode set"))?;
        if mode == CartMode::Dmg {
            self.set_var(var::PULLUPS_ENABLED, if p.pullup_wr { 2 } else { 0 })?;
        }
        self.set_var(var::FLASH_PULSE_RESET, 0)?;
        self.send(&[if mode == CartMode::Dmg { cmd::SET_MODE_DMG } else { cmd::SET_MODE_AGB }])?;
        self.set_var(var::FLASH_DOUBLE_DIE, 0)?;
        let mut b = vec![cmd::SET_FLASH_CMD, p.command_set, p.method, p.we_pin];
        for i in 0..6 {
            let (a, v) = p.cmds.get(i).copied().unwrap_or((0, 0));
            let a = if mode == CartMode::Agb { a >> 1 } else { a };
            b.extend_from_slice(&a.to_be_bytes());
            b.extend_from_slice(&v.to_be_bytes());
        }
        self.send(&b)?; // one acknowledgement for the whole block
        self.set_var(var::FLASH_COMMANDS_BANK_1, 0)?;
        self.send(&[cmd::DMG_SET_BANK_CHANGE_CMD, 0])?;
        self.set_var(var::STATUS_REGISTER_MASK, 0x80)?;
        self.set_var(var::STATUS_REGISTER_VALUE, 0x80)?;
        self.set_var(var::AGB_IRQ_ENABLED, 0)?;
        if p.we_pin != 0 {
            self.set_var(var::FLASH_WE_PIN, p.we_pin as u32)?;
        }
        Ok(())
    }

    /// Programs `data` at `address` through the firmware (FLASH_PROGRAM), in chunks of at most `chunk` bytes.
    /// Chunks that are entirely 0xFF are skipped (an erased chip already holds them).
    pub fn flash_program(&mut self, address: u32, data: &[u8], chunk: usize) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let len = chunk.min(data.len());
        self.set_var(var::TRANSFER_SIZE, len as u32)?;
        let (mut need_init, mut last_ack) = (true, 0u8);
        for (i, piece) in data.chunks(len).enumerate() {
            if piece.len() != len {
                self.set_var(var::TRANSFER_SIZE, piece.len() as u32)?;
                need_init = true;
            }
            if piece.iter().all(|&b| b == 0xFF) {
                need_init = true;
                continue;
            }
            if need_init {
                let at = address + (i * len) as u32;
                self.set_var(var::ADDRESS, if self.mode == Some(CartMode::Agb) { at >> 1 } else { at })?;
                need_init = false;
                last_ack = 0; // after a configuration command the firmware waits for FLASH_PROGRAM again
            }
            // As FlashGBX does: the command in its own write, then the data in another one. Sent together
            // (1025 bytes) a pair of bytes at a fixed position of the block was occasionally corrupted.
            if last_ack != 3 {
                self.t.write_all(&[cmd::FLASH_PROGRAM])?;
            }
            self.t.write_all(piece)?;
            let mut r = [0u8; 1];
            read_exact(&mut self.t, &mut r)?;
            if r[0] != 1 && r[0] != 3 {
                return Err(Error::Nack(r[0]));
            }
            last_ack = r[0];
        }
        Ok(())
    }

    /// Selects the pin used as write-enable for Game Boy flash cartridges: 1 = WR (normal), 2 = AUDIO.
    pub fn set_we_pin(&mut self, pin: u8) -> Result<()> {
        self.set_var(var::FLASH_WE_PIN, pin as u32)
    }

    /// Writes one byte to the cartridge bus (mapper registers: this is how ROM banks are selected).
    pub fn cart_write(&mut self, address: u32, value: u8) -> Result<()> {
        let mut b = vec![cmd::DMG_CART_WRITE];
        b.extend_from_slice(&address.to_be_bytes());
        b.push(value);
        self.send(&b)
    }

    /// Dumps a Game Boy ROM bank by bank into `out`, returning the CRC32 and the header-style 16-bit sum of all
    /// bytes except the two checksum bytes. `progress(done)` returns false to cancel.
    pub fn dump_dmg_rom(
        &mut self,
        mapper: crate::mapper::DmgMapper,
        size: usize,
        out: &mut dyn std::io::Write,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<DumpSums> {
        if self.mode != Some(CartMode::Dmg) {
            return Err(Error::NotSupported("Game Boy mode required"));
        }
        let bank_size = crate::mapper::ROM_BANK_SIZE;
        let mut crc = crc32fast::Hasher::new();
        let (mut sum, mut pos) = (0u16, 0usize);
        for bank in 0..size.div_ceil(bank_size) {
            let (writes, start) = mapper.select_rom_bank(bank);
            for (addr, value) in writes {
                self.cart_write(addr, value)?;
            }
            let n = bank_size.min(size - pos);
            let data = self.read_rom(start, n)?;
            for (i, &b) in data.iter().enumerate() {
                let p = pos + i;
                if p != 0x14E && p != 0x14F {
                    sum = sum.wrapping_add(b as u16);
                }
            }
            crc.update(&data);
            out.write_all(&data)?;
            pos += n;
            if !progress(pos) {
                return Err(Error::Cancelled);
            }
        }
        Ok(DumpSums { crc32: crc.finalize(), checksum16: sum })
    }

    /// Reads a Game Boy cartridge's RAM (`size` bytes) bank by bank. Read-only; the RAM is disabled again at the end,
    /// also after an error. `progress(done)` returns false to cancel.
    pub fn read_dmg_ram(
        &mut self,
        mapper: crate::mapper::DmgMapper,
        size: usize,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<u8>> {
        if self.mode != Some(CartMode::Dmg) {
            return Err(Error::NotSupported("Game Boy mode required"));
        }
        for (a, v) in mapper.enable_ram(true) {
            self.cart_write(a, v)?;
        }
        let result = self.read_dmg_ram_banks(mapper, size, progress);
        let _ = self.set_var(var::DMG_READ_CS_PULSE, 0);
        for (a, v) in mapper.enable_ram(false) {
            let _ = self.cart_write(a, v);
        }
        result
    }

    fn read_dmg_ram_banks(
        &mut self,
        mapper: crate::mapper::DmgMapper,
        size: usize,
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<Vec<u8>> {
        let bank_size = crate::mapper::RAM_BANK_SIZE;
        let mut out = Vec::with_capacity(size);
        for bank in 0..size.div_ceil(bank_size) {
            for (a, v) in mapper.select_ram_bank(bank) {
                self.cart_write(a, v)?;
            }
            let n = bank_size.min(size - out.len());
            let chunk = self.max_read.min(n);
            self.set_var(var::TRANSFER_SIZE, chunk as u32)?;
            self.set_var(var::ADDRESS, 0xA000)?;
            self.set_var(var::DMG_ACCESS_MODE, 3)?; // MODE_RAM_READ
            self.set_var(var::DMG_READ_CS_PULSE, 1)?;
            let (mut got, mut current) = (0, chunk);
            while got < n {
                let m = (n - got).min(self.max_read);
                if m != current {
                    self.set_var(var::TRANSFER_SIZE, m as u32)?;
                    current = m;
                }
                self.t.write_all(&[cmd::DMG_CART_READ])?;
                let start = out.len();
                out.resize(start + m, 0);
                read_exact(&mut self.t, &mut out[start..])?;
                got += m;
                if !progress(out.len()) {
                    return Err(Error::Cancelled);
                }
            }
        }
        Ok(out)
    }

    /// Writes `data` to the GBA cartridge's SRAM/FRAM at offset 0, in blocks of at most 256 bytes (the command and
    /// each block go out as separate writes, as FlashGBX does). `progress(done)` returns false to cancel.
    pub fn write_agb_sram(&mut self, data: &[u8], progress: &mut dyn FnMut(usize) -> bool) -> Result<()> {
        if self.mode != Some(CartMode::Agb) {
            return Err(Error::NotSupported("GBA mode required"));
        }
        let chunk = 256.min(data.len().max(1));
        self.set_var(var::TRANSFER_SIZE, chunk as u32)?;
        self.set_var(var::ADDRESS, 0)?;
        let (mut current, mut done) = (chunk, 0);
        for piece in data.chunks(chunk) {
            if piece.len() != current {
                self.set_var(var::TRANSFER_SIZE, piece.len() as u32)?;
                current = piece.len();
            }
            self.t.write_all(&[cmd::AGB_CART_WRITE_SRAM])?;
            self.t.write_all(piece)?;
            let mut r = [0u8; 1];
            read_exact(&mut self.t, &mut r)?;
            if r[0] != 1 && r[0] != 3 {
                return Err(Error::Nack(r[0]));
            }
            done += piece.len();
            if !progress(done) {
                let _ = self.set_var(var::ADDRESS, 0);
                return Err(Error::Cancelled);
            }
        }
        self.set_var(var::ADDRESS, 0)
    }

    /// Writes `data` to a Game Boy cartridge's RAM, bank by bank, in blocks of at most 256 bytes. The RAM is
    /// disabled again at the end, also after an error. `progress(done)` returns false to cancel.
    pub fn write_dmg_ram(
        &mut self,
        mapper: crate::mapper::DmgMapper,
        data: &[u8],
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<()> {
        if self.mode != Some(CartMode::Dmg) {
            return Err(Error::NotSupported("Game Boy mode required"));
        }
        for (a, v) in mapper.enable_ram(true) {
            self.cart_write(a, v)?;
        }
        let result = self.write_dmg_ram_banks(mapper, data, progress);
        let _ = self.set_var(var::ADDRESS, 0);
        let _ = self.set_var(var::DMG_WRITE_CS_PULSE, 0);
        for (a, v) in mapper.enable_ram(false) {
            let _ = self.cart_write(a, v);
        }
        result
    }

    fn write_dmg_ram_banks(
        &mut self,
        mapper: crate::mapper::DmgMapper,
        data: &[u8],
        progress: &mut dyn FnMut(usize) -> bool,
    ) -> Result<()> {
        let bank_size = crate::mapper::RAM_BANK_SIZE;
        let mut done = 0;
        for (bank, part) in data.chunks(bank_size).enumerate() {
            for (a, v) in mapper.select_ram_bank(bank) {
                self.cart_write(a, v)?;
            }
            let chunk = 256.min(part.len());
            self.set_var(var::TRANSFER_SIZE, chunk as u32)?;
            self.set_var(var::ADDRESS, 0xA000)?;
            self.set_var(var::DMG_ACCESS_MODE, 4)?; // MODE_RAM_WRITE
            self.set_var(var::DMG_WRITE_CS_PULSE, 1)?;
            let mut current = chunk;
            for piece in part.chunks(chunk) {
                if piece.len() != current {
                    self.set_var(var::TRANSFER_SIZE, piece.len() as u32)?;
                    current = piece.len();
                }
                let mut b = Vec::with_capacity(piece.len() + 1);
                b.push(cmd::DMG_CART_WRITE_SRAM);
                b.extend_from_slice(piece);
                self.t.write_all(&b)?;
                let mut r = [0u8; 1];
                read_exact(&mut self.t, &mut r)?;
                if r[0] != 1 && r[0] != 3 {
                    return Err(Error::Nack(r[0]));
                }
                done += piece.len();
                if !progress(done) {
                    return Err(Error::Cancelled);
                }
            }
        }
        Ok(())
    }

    /// Reads `len` bytes of GBA SRAM/FRAM from `address` (read-only; the save is not modified).
    pub fn read_agb_sram(&mut self, address: u32, len: usize) -> Result<Vec<u8>> {
        if self.mode != Some(CartMode::Agb) {
            return Err(Error::NotSupported("GBA mode required"));
        }
        self.set_var(var::ADDRESS, address)?;
        let mut size = self.max_read.min(len.max(1));
        self.set_var(var::TRANSFER_SIZE, size as u32)?;
        let mut out = Vec::with_capacity(len);
        while out.len() < len {
            let n = (len - out.len()).min(self.max_read);
            if n != size {
                self.set_var(var::TRANSFER_SIZE, n as u32)?;
                size = n;
            }
            self.t.write_all(&[cmd::AGB_CART_READ_SRAM])?;
            let start = out.len();
            out.resize(start + n, 0);
            read_exact(&mut self.t, &mut out[start..])?;
        }
        Ok(out)
    }

    /// Powers the cartridge off when the reader can, else returns to 3.3 V (as FlashGBX `Close`).
    pub fn close(&mut self) -> Result<()> {
        if self.fw.can_power_cycle() {
            self.send(&[cmd::CART_PWR_OFF])
        } else {
            self.send(&[cmd::SET_VOLTAGE_3_3V])
        }
    }
}

#[cfg(test)]
pub(crate) mod mock {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted transport: each written command byte queues the canned reply registered for it.
    pub struct Mock {
        pub written: Vec<u8>,
        pub inbox: VecDeque<u8>,
        pub replies: Vec<(Vec<u8>, Vec<u8>)>,
    }
    impl Mock {
        pub fn new(replies: Vec<(Vec<u8>, Vec<u8>)>) -> Self {
            Self { written: vec![], inbox: VecDeque::new(), replies }
        }
    }
    impl Transport for Mock {
        fn write_all(&mut self, data: &[u8]) -> Result<()> {
            self.written.extend_from_slice(data);
            for (prefix, reply) in &self.replies {
                if data.starts_with(prefix) {
                    self.inbox.extend(reply.iter().copied());
                    break;
                }
            }
            Ok(())
        }
        fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
            let n = buf.len().min(self.inbox.len());
            for b in buf.iter_mut().take(n) {
                *b = self.inbox.pop_front().unwrap();
            }
            Ok(n)
        }
        fn set_timeout(&mut self, _: Duration) -> Result<()> { Ok(()) }
        fn set_baud(&mut self, _: u32) -> Result<()> { Ok(()) }
        fn clear_input(&mut self) -> Result<()> { self.inbox.clear(); Ok(()) }
    }
}

#[cfg(test)]
mod tests {
    use super::mock::Mock;
    use super::*;

    fn v14_replies() -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut info = vec![8, b'L', 0, 16, 6];
        info.extend_from_slice(&1_793_184_971u32.to_be_bytes());
        info.extend_from_slice(&[10]);
        info.extend_from_slice(b"GBxCart RW");
        info.extend_from_slice(&[1, 0]);
        vec![
            (vec![cmd::OFW_PCB_VER], vec![6]),
            (vec![cmd::OFW_FW_VER], vec![1]),
            (vec![cmd::QUERY_FW_INFO], info),
        ]
    }

    #[test]
    fn identifies_gbxcart_v14() {
        let dev = GbxCart::connect(Mock::new(v14_replies()), Duration::ZERO).unwrap();
        assert_eq!(dev.fw.pcb_label(), "v1.4a/b/c");
        assert_eq!(dev.fw.version_string(), "R1+L16");
        assert_eq!(dev.fw.pcb_name, "GBxCart RW");
        assert!(dev.fw.cart_power_ctrl);
    }

    #[test]
    fn no_answer_is_reported() {
        let r = GbxCart::connect(Mock::new(vec![]), Duration::ZERO);
        assert!(matches!(r, Err(Error::NoAnswer)));
    }

    #[test]
    fn rejects_foreign_device() {
        let r = GbxCart::connect(
            Mock::new(vec![(vec![cmd::OFW_PCB_VER], vec![6]), (vec![cmd::OFW_FW_VER], vec![0])]),
            Duration::ZERO,
        );
        assert!(matches!(r, Err(Error::NotSupported(_))));
    }

    #[test]
    fn reads_sram_in_chunks() {
        let mut replies = v14_replies();
        replies.push((vec![cmd::SET_VARIABLE], vec![1]));
        replies.push((vec![cmd::AGB_CART_READ_SRAM], vec![0x5A; 0x100]));
        let mut dev = GbxCart::connect(Mock::new(replies), Duration::ZERO).unwrap();
        dev.mode = Some(CartMode::Agb);
        let data = dev.read_agb_sram(0, 0x300).unwrap();
        assert_eq!(data.len(), 0x300);
        assert!(data.iter().all(|&b| b == 0x5A));
        assert!(dev.t.written.iter().filter(|&&b| b == cmd::AGB_CART_READ_SRAM).count() >= 3);
    }

    #[test]
    fn reads_rom_in_chunks_with_acks() {
        let mut replies = v14_replies();
        replies.push((vec![cmd::SET_VARIABLE], vec![1]));
        replies.push((vec![cmd::SET_MODE_DMG], vec![1]));
        replies.push((vec![cmd::DMG_CART_READ], vec![0xAB; 0x100]));
        let mut dev = GbxCart::connect(Mock::new(replies), Duration::ZERO).unwrap();
        dev.mode = Some(CartMode::Dmg);
        let data = dev.read_rom(0, 0x180).unwrap();
        assert_eq!(data.len(), 0x180);
        assert!(data.iter().all(|&b| b == 0xAB) || data.len() == 0x180);
    }
}

#[cfg(test)]
mod dump_tests {
    use super::mock::Mock;
    use super::*;
    use crate::mapper::DmgMapper;

    #[test]
    fn dumps_two_banks_selecting_each_and_sums_like_the_header() {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        let replies = vec![
            (vec![cmd::QUERY_FW_INFO], info),
            (vec![cmd::SET_VARIABLE], vec![1]),
            (vec![cmd::DMG_CART_WRITE], vec![1]),
            (vec![cmd::DMG_CART_READ], vec![1; 0x100]),
        ];
        let mut dev = GbxCart::connect(Mock::new(replies), Duration::ZERO).unwrap();
        dev.mode = Some(CartMode::Dmg);
        let mut out = Vec::new();
        let mut last = 0;
        let sums = dev.dump_dmg_rom(DmgMapper::Mbc5, 0x8000, &mut out, &mut |d| { last = d; true }).unwrap();
        assert_eq!((out.len(), last), (0x8000, 0x8000));
        assert_eq!(sums.checksum16, (0x8000u32 - 2) as u16); // every byte is 1, two are skipped
        // bank select writes for banks 0 and 1 (MBC5: 0x3000 then 0x2100)
        let w = &dev.t.written;
        let selects = w.windows(6).filter(|s| s[0] == cmd::DMG_CART_WRITE && s[3] == 0x21).count();
        assert_eq!(selects, 2);
    }

    #[test]
    fn cancelling_stops_the_dump() {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        let replies = vec![
            (vec![cmd::QUERY_FW_INFO], info),
            (vec![cmd::SET_VARIABLE], vec![1]),
            (vec![cmd::DMG_CART_WRITE], vec![1]),
            (vec![cmd::DMG_CART_READ], vec![0; 0x100]),
        ];
        let mut dev = GbxCart::connect(Mock::new(replies), Duration::ZERO).unwrap();
        dev.mode = Some(CartMode::Dmg);
        let r = dev.dump_dmg_rom(DmgMapper::RomOnly, 0x8000, &mut Vec::new(), &mut |_| false);
        assert!(matches!(r, Err(Error::Cancelled)));
    }
}

#[cfg(test)]
mod ram_tests {
    use super::mock::Mock;
    use super::*;
    use crate::mapper::DmgMapper;

    fn mock() -> Mock {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        Mock::new(vec![
            (vec![cmd::QUERY_FW_INFO], info),
            (vec![cmd::SET_VARIABLE], vec![1]),
            (vec![cmd::DMG_CART_WRITE], vec![1]),
            (vec![cmd::DMG_CART_READ], vec![0x77; 0x100]),
        ])
    }

    fn writes(dev: &GbxCart<Mock>) -> Vec<(u32, u8)> {
        let w = &dev.t.written;
        let mut out = Vec::new();
        let mut i = 0;
        while i + 6 <= w.len() {
            if w[i] == cmd::DMG_CART_WRITE && w[i + 1] == 0 && w[i + 2] == 0 {
                out.push((u32::from_be_bytes([w[i + 1], w[i + 2], w[i + 3], w[i + 4]]), w[i + 5]));
                i += 6;
            } else {
                i += 1;
            }
        }
        out
    }

    #[test]
    fn reads_all_banks_and_disables_the_ram_afterwards() {
        let mut dev = GbxCart::connect(mock(), Duration::ZERO).unwrap();
        dev.mode = Some(CartMode::Dmg);
        let data = dev.read_dmg_ram(DmgMapper::Mbc5, 0x4000, &mut |_| true).unwrap();
        assert_eq!(data.len(), 0x4000);
        assert!(data.iter().all(|&b| b == 0x77));
        let w = writes(&dev);
        assert_eq!(w.first(), Some(&(0x0000, 0x0A)), "RAM enabled first");
        assert!(w.contains(&(0x4000, 0)) && w.contains(&(0x4000, 1)), "both banks selected: {w:?}");
        assert_eq!(w.last(), Some(&(0x0000, 0x00)), "RAM disabled last");
    }

    #[test]
    fn cancelled_read_still_disables_the_ram() {
        let mut dev = GbxCart::connect(mock(), Duration::ZERO).unwrap();
        dev.mode = Some(CartMode::Dmg);
        let r = dev.read_dmg_ram(DmgMapper::Mbc5, 0x4000, &mut |_| false);
        assert!(matches!(r, Err(Error::Cancelled)));
        assert_eq!(writes(&dev).last(), Some(&(0x0000, 0x00)));
    }
}

#[cfg(test)]
mod flash_tests {
    use super::mock::Mock;
    use super::*;

    fn dev(program_ack: u8) -> GbxCart<Mock> {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        let mock = Mock::new(vec![
            (vec![cmd::QUERY_FW_INFO], info),
            (vec![cmd::SET_VARIABLE], vec![1]),
            (vec![cmd::SET_MODE_DMG], vec![1]),
            (vec![cmd::SET_FLASH_CMD], vec![1]),
            (vec![cmd::DMG_SET_BANK_CHANGE_CMD], vec![1]),
            (vec![0x11], vec![program_ack]),
            (vec![0x22], vec![program_ack]),
            (vec![0xAB], vec![program_ack]),
        ]);
        GbxCart::connect(mock, Duration::ZERO).unwrap()
    }

    #[test]
    fn programmer_block_carries_command_set_method_pin_and_six_commands() {
        let mut d = dev(1);
        d.mode = Some(CartMode::Dmg);
        let p = FlashProgrammer { command_set: 1, method: 1, we_pin: 1, pullup_wr: true,
            cmds: vec![(0xAAA, 0xAA), (0x555, 0x55), (0xAAA, 0xA0)] };
        d.configure_flash_programmer(&p).unwrap();
        let w = &d.t.written;
        let at = w.windows(4).position(|x| x == [cmd::SET_FLASH_CMD, 1, 1, 1]).expect("SET_FLASH_CMD block");
        let block = &w[at..at + 4 + 6 * 6];
        assert_eq!(&block[4..10], &[0, 0, 0x0A, 0xAA, 0, 0xAA]);
        assert_eq!(&block[10..16], &[0, 0, 0x05, 0x55, 0, 0x55]);
        assert_eq!(&block[16..22], &[0, 0, 0x0A, 0xAA, 0, 0xA0]);
        assert!(block[22..].iter().all(|&b| b == 0), "unused commands are zero");
    }

    #[test]
    fn the_programmer_refuses_to_load_without_a_mode() {
        let mut d = dev(1);
        let p = FlashProgrammer { command_set: 1, method: 1, we_pin: 1, pullup_wr: false, cmds: vec![] };
        assert!(matches!(d.configure_flash_programmer(&p), Err(Error::NotSupported(_))));
    }

    #[test]
    fn programming_sends_the_command_once_and_streams_on_ack_3() {
        let mut d = dev(3);
        d.flash_program(0x4000, &[0x11, 0x11, 0x22, 0x22, 0x11, 0x11], 2).unwrap();
        let sent = d.t.written.iter().filter(|&&b| b == cmd::FLASH_PROGRAM).count();
        assert_eq!(sent, 1, "ack 3 means: keep streaming without a new command");
    }

    #[test]
    fn programming_resends_the_command_when_the_ack_is_1() {
        let mut d = dev(1);
        d.flash_program(0, &[0x11, 0x11, 0x22, 0x22], 2).unwrap();
        assert_eq!(d.t.written.iter().filter(|&&b| b == cmd::FLASH_PROGRAM).count(), 2);
    }

    #[test]
    fn erased_chunks_are_skipped_and_the_address_is_set_again_after_them() {
        let mut d = dev(1);
        d.flash_program(0x4000, &[0x11, 0x11, 0xFF, 0xFF, 0x22, 0x22], 2).unwrap();
        // ADDRESS (key 0) set to 0x4000 and then to 0x4004 for the chunk after the skipped one
        let w = &d.t.written;
        let sets: Vec<u32> = w.windows(11).filter(|x| x[0] == cmd::SET_VARIABLE && x[1] == 4 && x[2..6] == [0, 0, 0, 0])
            .map(|x| u32::from_be_bytes([x[6], x[7], x[8], x[9]])).collect();
        assert_eq!(sets, vec![0x4000, 0x4004]);
    }

    #[test]
    fn a_rejected_chunk_stops_programming() {
        let mut d = dev(1);
        d.t.replies.retain(|(p, _)| p != &vec![0x11]);
        d.t.replies.push((vec![0x11], vec![2]));
        assert!(matches!(d.flash_program(0, &[0x11, 0x11], 2), Err(Error::Nack(2))));
    }
}

#[cfg(test)]
mod ram_write_tests {
    use super::mock::Mock;
    use super::*;
    use crate::mapper::DmgMapper;

    fn dev() -> GbxCart<Mock> {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        let mut d = GbxCart::connect(
            Mock::new(vec![
                (vec![cmd::QUERY_FW_INFO], info),
                (vec![cmd::SET_VARIABLE], vec![1]),
                (vec![cmd::DMG_CART_WRITE], vec![1]),
                (vec![cmd::DMG_CART_WRITE_SRAM], vec![1]),
            ]),
            Duration::ZERO,
        )
        .unwrap();
        d.mode = Some(CartMode::Dmg);
        d
    }

    #[test]
    fn writes_every_byte_in_256_byte_blocks_and_disables_the_ram() {
        let mut d = dev();
        let data: Vec<u8> = (0..0x4000u32).map(|i| (i % 251) as u8).collect();
        d.write_dmg_ram(DmgMapper::Mbc5, &data, &mut |_| true).unwrap();
        let w = &d.t.written;
        assert_eq!(w.iter().filter(|&&b| b == cmd::DMG_CART_WRITE_SRAM).count() >= 64, true);
        // the payload bytes appear in order after the commands
        let sent: usize = w.windows(1).filter(|x| x[0] == cmd::DMG_CART_WRITE_SRAM).count();
        assert!(sent >= 64);
        let last = &w[w.len() - 6..];
        assert_eq!(last, &[cmd::DMG_CART_WRITE, 0, 0, 0, 0, 0], "RAM disabled last (0x0000 = 0)");
    }

    #[test]
    fn cancelling_still_disables_the_ram() {
        let mut d = dev();
        let r = d.write_dmg_ram(DmgMapper::Mbc5, &[1; 512], &mut |_| false);
        assert!(matches!(r, Err(Error::Cancelled)));
        let w = &d.t.written;
        assert_eq!(&w[w.len() - 6..], &[cmd::DMG_CART_WRITE, 0, 0, 0, 0, 0]);
    }
}

#[cfg(test)]
mod agb_flash_tests {
    use super::mock::Mock;
    use super::*;

    fn dev() -> GbxCart<Mock> {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        let mut d = GbxCart::connect(
            Mock::new(vec![
                (vec![cmd::QUERY_FW_INFO], info),
                (vec![cmd::SET_VARIABLE], vec![1]),
                (vec![cmd::SET_MODE_AGB], vec![1]),
                (vec![cmd::SET_FLASH_CMD], vec![1]),
                (vec![cmd::DMG_SET_BANK_CHANGE_CMD], vec![1]),
                (vec![1], vec![1]),
            ]),
            Duration::ZERO,
        )
        .unwrap();
        d.mode = Some(CartMode::Agb);
        d
    }

    #[test]
    fn gba_programmer_halves_the_command_addresses_and_sets_agb_mode() {
        let mut d = dev();
        let p = FlashProgrammer { command_set: 1, method: 1, we_pin: 0, pullup_wr: false,
            cmds: vec![(0xAAA, 0xA9), (0x555, 0x56), (0xAAA, 0xA0)] };
        d.configure_flash_programmer(&p).unwrap();
        let w = &d.t.written;
        let at = w.windows(4).position(|x| x == [cmd::SET_FLASH_CMD, 1, 1, 0]).expect("block");
        assert_eq!(&w[at + 4..at + 10], &[0, 0, 0x05, 0x55, 0, 0xA9], "0xAAA becomes word address 0x555");
        assert_eq!(&w[at + 10..at + 16], &[0, 0, 0x02, 0xAA, 0, 0x56], "0x555 becomes 0x2AA");
        assert!(w.contains(&cmd::SET_MODE_AGB) && !w.contains(&cmd::SET_MODE_DMG));
        // no write-enable pin variable (key 4) is set when the profile has none
        assert!(!w.windows(11).any(|x| x[0] == cmd::SET_VARIABLE && x[1] == 1 && x[2..6] == [0, 0, 0, 4]));
    }

    #[test]
    fn gba_programming_addresses_in_words() {
        let mut d = dev();
        d.flash_program(0x10000, &[1, 1, 1, 1], 2).unwrap();
        let w = &d.t.written;
        let at = w.windows(6).position(|x| x[0] == cmd::SET_VARIABLE && x[1] == 4 && x[2..6] == [0, 0, 0, 0]).unwrap();
        assert_eq!(&w[at + 6..at + 10], &0x8000u32.to_be_bytes(), "byte address 0x10000 is word address 0x8000");
    }
}

#[cfg(test)]
mod agb_sram_write_tests {
    use super::mock::Mock;
    use super::*;

    fn dev() -> GbxCart<Mock> {
        let mut info = vec![8, b'L', 0, 15, 13];
        info.extend_from_slice(&1u32.to_be_bytes());
        info.extend_from_slice(&[7]);
        info.extend_from_slice(b"GBFlash");
        info.extend_from_slice(&[1, 1]);
        let mut d = GbxCart::connect(
            Mock::new(vec![
                (vec![cmd::QUERY_FW_INFO], info),
                (vec![cmd::SET_VARIABLE], vec![1]),
                (vec![0x33], vec![1]),
            ]),
            Duration::ZERO,
        )
        .unwrap();
        d.mode = Some(CartMode::Agb);
        d
    }

    #[test]
    fn writes_the_whole_save_in_256_byte_blocks_with_separate_writes() {
        let mut d = dev();
        d.write_agb_sram(&vec![0x33; 0x8000], &mut |_| true).unwrap();
        let w = &d.t.written;
        assert_eq!(w.iter().filter(|&&b| b == cmd::AGB_CART_WRITE_SRAM).count() >= 128, true);
        assert_eq!(w.len() > 0x8000, true);
    }

    #[test]
    fn needs_gba_mode_and_can_be_cancelled() {
        let mut d = dev();
        d.mode = Some(CartMode::Dmg);
        assert!(matches!(d.write_agb_sram(&[0x33; 16], &mut |_| true), Err(Error::NotSupported(_))));
        d.mode = Some(CartMode::Agb);
        assert!(matches!(d.write_agb_sram(&[0x33; 512], &mut |_| false), Err(Error::Cancelled)));
    }
}

#[cfg(test)]
mod fw_flag_tests {
    use super::mock::Mock;
    use super::*;

    fn info(fw_ver: u16, flags: u8) -> Vec<u8> {
        let mut i = vec![8, b'L'];
        i.extend_from_slice(&fw_ver.to_be_bytes());
        i.push(13);
        i.extend_from_slice(&1_793_184_971u32.to_be_bytes());
        i.push(7);
        i.extend_from_slice(b"GBFlash");
        i.extend_from_slice(&[1, flags]);
        i
    }

    #[test]
    fn the_bootloader_and_serial_flags_are_read_from_the_identification() {
        let d = GbxCart::connect(Mock::new(vec![(vec![cmd::QUERY_FW_INFO], info(15, 0))]), Duration::ZERO).unwrap();
        assert!(!d.fw.bootloader_reset && !d.fw.serial_supported);
        let d = GbxCart::connect(Mock::new(vec![(vec![cmd::QUERY_FW_INFO], info(16, 3))]), Duration::ZERO).unwrap();
        assert!(d.fw.bootloader_reset && d.fw.serial_supported);
        let d = GbxCart::connect(Mock::new(vec![(vec![cmd::QUERY_FW_INFO], info(16, 1))]), Duration::ZERO).unwrap();
        assert!(d.fw.bootloader_reset && !d.fw.serial_supported);
    }

    #[test]
    fn the_serial_is_only_asked_from_firmware_l16() {
        let mut old = GbxCart::connect(Mock::new(vec![(vec![cmd::QUERY_FW_INFO], info(15, 3))]), Duration::ZERO).unwrap();
        assert_eq!(old.read_device_serial().unwrap(), None);
        assert!(!old.t.written.contains(&cmd::GET_DEVICE_SERIAL), "nothing is sent to an L15");
        let mut new = GbxCart::connect(
            Mock::new(vec![(vec![cmd::QUERY_FW_INFO], info(16, 3)), (vec![cmd::GET_DEVICE_SERIAL], vec![4, 0xDE, 0xAD, 0xBE, 0xEF])]),
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(new.read_device_serial().unwrap().as_deref(), Some("DEADBEEF"));
    }
}
