// SPDX-License-Identifier: GPL-3.0-or-later
//! Pixel-art sprites of the start screens: the ZeroCart logo and one icon per system, drawn from small character
//! grids with the palette of the interface.
use crate::canvas::{Canvas, Rgb};

/// The systems offered on the first screen. Game Boy and Game Boy Color share the reader path and the 5 V
/// mode but get their own icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    Gb,
    Gbc,
    Gba,
    MegaDrive,
}

impl System {
    pub const ALL: [System; 4] = [System::Gb, System::Gbc, System::Gba, System::MegaDrive];

    pub fn label(self) -> &'static str {
        match self {
            System::Gb => "Game Boy",
            System::Gbc => "GB Color",
            System::Gba => "GB Advance",
            System::MegaDrive => "Mega Drive",
        }
    }

    pub fn sprite(self) -> Sprite {
        match self {
            System::Gb => Sprite::Gb,
            System::Gbc => Sprite::Gbc,
            System::Gba => Sprite::Gba,
            System::MegaDrive => Sprite::MegaDrive,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sprite {
    Logo,
    Gb,
    Gbc,
    Gba,
    MegaDrive,
}

fn color(ch: char) -> Option<Rgb> {
    Some(match ch {
        '.' => return None,
        'D' => 0x101014, // outline
        'W' => 0xECE6D3, // cream
        'L' => 0xC9C3B0, // light grey shell
        'G' => 0xA79F8E, // grey
        'K' => 0x4A3A3C, // dark grey
        'R' => 0xC8202F, // red
        'Y' => 0xF3C93A, // gold
        'Q' => 0x6B2A30, // dark red
        'B' => 0x3A56C8, // GBA indigo
        'b' => 0x8DA0F0, // light indigo
        'P' => 0x7B4FA8, // GBC purple
        'T' => 0x3FA7A0, // teal
        'M' => 0x306230, // dark LCD green
        'N' => 0x9BBC0F, // LCD green
        'S' => 0x1A1518, // black plastic
        _ => return None,
    })
}

pub const LOGO: [&str; 20] = [
    "....................",
    ".....DDDDDDDDDD.....",
    "....DWWWWWWWWWWD....",
    "...DWWWWWWWWWWWWD...",
    "...DWGGGGGGGGGGWD...",
    "...DWWWWWWWWWWWWD...",
    "...DWDDDDDDDDDDWD...",
    "...DWDRRRRRRRRDWD...",
    "...DWDRRRYYRRRDWD...",
    "...DWDRRYYYYRRDWD...",
    "...DWDRYYRRYYRDWD...",
    "...DWDRRRRRRRRDWD...",
    "...DWDDDDDDDDDDWD...",
    "...DWWWWWWWWWWWWD...",
    "...DWGGWWWWWWGGWD...",
    "...DDDDDDDDDDDDDD...",
    "....DYQYQYQYQYQD....",
    "....DYQYQYQYQYQD....",
    "....DDDDDDDDDDDD....",
    "....................",
];

const GB: [&str; 16] = [
    "................",
    "....DDDDDDDD....",
    "...DLLLLLLLLD...",
    "...DLDDDDDDLD...",
    "...DLDMMMMDLD...",
    "...DLDMNNMDLD...",
    "...DLDMNNMDLD...",
    "...DLDMMMMDLD...",
    "...DLDDDDDDLD...",
    "...DLLLLLLLLD...",
    "...DLLKLLLLRD...",
    "...DLKKKLLRRD...",
    "...DLLKLLLLRD...",
    "...DLLLLLLLLD...",
    "....DDDDDDDD....",
    "................",
];

const GBC: [&str; 16] = [
    "................",
    "....DDDDDDDD....",
    "...DPPPPPPPPD...",
    "...DPDDDDDDPD...",
    "...DPDTTTTDPD...",
    "...DPDTWWTDPD...",
    "...DPDTWWTDPD...",
    "...DPDTTTTDPD...",
    "...DPDDDDDDPD...",
    "...DPPPPPPPPD...",
    "...DPPWPPPPYD...",
    "...DPWWWPPYYD...",
    "...DPPWPPPPYD...",
    "...DPPPPPPPPD...",
    "....DDDDDDDD....",
    "................",
];

const GBA: [&str; 16] = [
    "................",
    "................",
    "................",
    "..DDDDDDDDDDDD..",
    ".DBBBBBBBBBBBBD.",
    ".DBbDDDDDDDDbRD.",
    ".DBKDMMMMMMDBRD.",
    ".DKKKDMNNNMDBBD.",
    ".DBKDDMNNNMDDBD.",
    ".DBBBDDDDDDDBBD.",
    ".DBBBBBBBBBBBBD.",
    "..DDDDDDDDDDDD..",
    "................",
    "................",
    "................",
    "................",
];

const MD: [&str; 16] = [
    "................",
    "................",
    "....DDDDDDDD....",
    "...DSSSSSSSSD...",
    "...DSRRRRRRSD...",
    "...DSYYYYYYSD...",
    "...DSRRRRRRSD...",
    "...DSSSSSSSSD...",
    "..DDDDDDDDDDDD..",
    ".DSSSSSSSSSSSSD.",
    ".DSKKKKKKKKKKSD.",
    ".DSKGGGGGGGGKSD.",
    ".DSSSSSSSSSSSSD.",
    "..DDDDDDDDDDDD..",
    "................",
    "................",
];

fn grid(s: Sprite) -> &'static [&'static str] {
    match s {
        Sprite::Logo => &LOGO,
        Sprite::Gb => &GB,
        Sprite::Gbc => &GBC,
        Sprite::Gba => &GBA,
        Sprite::MegaDrive => &MD,
    }
}

/// Side of the sprite grid in cells (the logo is 20x20, the system icons 16x16).
pub fn cells(s: Sprite) -> i32 {
    grid(s).len() as i32
}

/// Draws a sprite with its top-left corner at (x, y), each grid cell being `cell` x `cell` pixels.
pub fn draw(canvas: &mut Canvas, s: Sprite, x: i32, y: i32, cell: i32) {
    for (row, line) in grid(s).iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            if let Some(c) = color(ch) {
                canvas.fill_rect(x + col as i32 * cell, y + row as i32 * cell, cell, cell, c);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sprite_is_a_square_grid_of_known_colours() {
        for s in [Sprite::Logo, Sprite::Gb, Sprite::Gbc, Sprite::Gba, Sprite::MegaDrive] {
            let g = grid(s);
            assert!(g.iter().all(|r| r.chars().count() == g.len()), "{s:?} is not square");
            assert!(g.iter().flat_map(|r| r.chars()).all(|c| c == '.' || color(c).is_some()), "{s:?} has an unknown colour");
            assert!(g.iter().flat_map(|r| r.chars()).any(|c| c != '.'), "{s:?} is empty");
        }
    }

    #[test]
    fn the_four_systems_have_distinct_icons_and_labels() {
        let mut seen = std::collections::HashSet::new();
        for s in System::ALL {
            assert!(seen.insert(grid(s.sprite()).concat()), "{s:?} shares its icon");
        }
        assert_eq!(System::ALL.map(System::label), ["Game Boy", "GB Color", "GB Advance", "Mega Drive"]);
    }

    #[test]
    fn drawing_paints_pixels_inside_the_cell_box() {
        let mut c = Canvas::new(100, 100, 0);
        draw(&mut c, Sprite::Gb, 10, 10, 2);
        assert_eq!(c.get(0, 0), Some(0), "nothing outside");
        assert_eq!(c.get(10 + 4 * 2, 10 + 1 * 2), Some(0x101014), "the first outline cell (col 4, row 1)");
    }
}
