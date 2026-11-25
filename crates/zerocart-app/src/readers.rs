// SPDX-License-Identifier: GPL-3.0-or-later
//! Which readers are plugged in, from their USB ids only (nothing is opened or sent: a cheap check that can run
//! every half second). The real handshake happens when the user confirms a reader.
use zerocart_core::{chis, krikzz, CartKind, ReaderKind};
use zerocart_ui::System;

/// A reader found on the USB bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub kind: ReaderKind,
    pub port: String,
}

pub const ALL: [ReaderKind; 3] = [ReaderKind::GbxCart, ReaderKind::Chisflash, ReaderKind::KrikzzMd];

/// The readers of the USB bus, GBFlash / GBxCart RW first.
pub fn scan() -> Vec<Detected> {
    let mut out = Vec::new();
    for port in zerocart_core::find_ports() {
        out.push(Detected { kind: ReaderKind::GbxCart, port });
    }
    for port in chis::find_ports() {
        out.push(Detected { kind: ReaderKind::Chisflash, port });
    }
    for port in krikzz::find_ports() {
        out.push(Detected { kind: ReaderKind::KrikzzMd, port });
    }
    out
}

pub fn cart_kind(system: System) -> CartKind {
    match system {
        System::Gb | System::Gbc => CartKind::Gb,
        System::Gba => CartKind::Gba,
        System::MegaDrive => CartKind::MegaDrive,
    }
}

/// The readers that can take a cartridge of this system, in display order.
pub fn compatible(system: System) -> Vec<ReaderKind> {
    ALL.iter().copied().filter(|k| k.cart_kinds().contains(&cart_kind(system))).collect()
}

/// The USB ids to plug it into, as a hint when it is not found.
pub fn usb_hint(kind: ReaderKind) -> &'static str {
    match kind {
        ReaderKind::GbxCart => "USB 1a86:7523",
        ReaderKind::Chisflash => "USB 0483:0721",
        ReaderKind::KrikzzMd => "USB 0403:6001",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_boy_systems_take_the_two_gb_readers_and_the_mega_drive_the_krikzz() {
        for s in [System::Gb, System::Gbc, System::Gba] {
            assert_eq!(compatible(s), vec![ReaderKind::GbxCart, ReaderKind::Chisflash]);
        }
        assert_eq!(compatible(System::MegaDrive), vec![ReaderKind::KrikzzMd]);
    }

    #[test]
    fn gb_and_gbc_are_the_same_kind_of_cartridge_for_the_readers() {
        assert_eq!(cart_kind(System::Gb), cart_kind(System::Gbc));
        assert_ne!(cart_kind(System::Gb), cart_kind(System::Gba));
    }
}
