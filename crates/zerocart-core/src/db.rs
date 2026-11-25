//! Game database (`db_AGB.json`, `db_DMG.json` from FlashGBX), keyed by the SHA-1 of the ROM header.
use serde::Deserialize;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// Game name
    pub gn: String,
    /// Name suffix (region, flags)
    #[serde(default)]
    pub ne: String,
    /// Game code
    #[serde(default)]
    pub gc: String,
    /// ROM CRC32
    pub rc: u32,
    /// ROM size in bytes
    pub rs: u64,
    /// Save type index (GBA: see [`agb_save_name`])
    #[serde(default)]
    pub st: u32,
    /// Save size in bytes
    #[serde(default)]
    pub ss: u64,
    #[serde(default)]
    pub rg: String,
}

pub struct Database(HashMap<String, Entry>);

impl Database {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let map = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self(map))
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn get(&self, header_sha1: &str) -> Option<&Entry> {
        self.0.get(header_sha1)
    }
    /// Looks a GBA header up as FlashGBX does (first 0x180 bytes; 0x100 for "M" game codes).
    pub fn lookup_agb(&self, header: &[u8]) -> Option<&Entry> {
        let end = if header.len() >= 0xB0 && header[0xAC] == b'M' { 0x100 } else { 0x180 };
        self.get(&sha1_hex(header.get(..end)?))
    }
    pub fn lookup_dmg(&self, header: &[u8]) -> Option<&Entry> {
        self.get(&sha1_hex(header.get(..0x180)?))
    }
}

pub fn sha1_hex(data: &[u8]) -> String {
    Sha1::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

/// Streaming CRC32 (IEEE), for ROMs written to disk block by block.
pub use crc32fast::Hasher as Crc32;

pub fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// FlashGBX GBA save type indexes.
pub fn agb_save_name(st: u32) -> &'static str {
    match st {
        1 => "4K EEPROM",
        2 => "64K EEPROM",
        3 => "256K SRAM/FRAM",
        4 => "512K FLASH",
        5 => "1M FLASH",
        6 => "8M DACS",
        7 => "Unlicensed 512K SRAM",
        8 => "Unlicensed 1M SRAM",
        9 => "Unlicensed Batteryless SRAM",
        _ => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_and_crc_known_values() {
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(crc32(b"123456789"), 0xCBF43926);
    }

    #[test]
    fn parses_entries() {
        let json = r#"{"7ea8":{"gn":"Mr. Driller 2","ne":"(Europe)","gc":"AD2P","rc":3236556478,"rs":4194304,"st":3,"ss":32768,"lg":"En","rg":"Europe"}}"#;
        let db = Database(serde_json::from_str(json).unwrap());
        let e = db.get("7ea8").unwrap();
        assert_eq!((e.rs, e.st, e.ss), (4194304, 3, 32768));
        assert_eq!(agb_save_name(e.st), "256K SRAM/FRAM");
    }

    #[test]
    fn bundled_databases_load() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../configs");
        assert!(Database::load(&dir.join("db_AGB.json")).unwrap().len() > 2000);
        assert!(Database::load(&dir.join("db_DMG.json")).unwrap().len() > 1000);
    }
}
