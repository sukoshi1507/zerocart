# ZeroCart

Game Boy / Game Boy Color / Game Boy Advance cartridge reader and flasher for the M5Stack **CardputerZero**.
A Rust port of the protocol and chip databases of [FlashGBX](https://github.com/lesserkuma/FlashGBX) by Lesserkuma,
with a 320x170 Famicom-style interface driven by the keyboard.

## Hardware

- A **GBFlash** (tested: v1.3, firmware L15) plugged into the CardputerZero USB port. GBxCart RW should work through
  the same protocol but has not been tried.
- The user must be allowed to open the serial port (group `dialout`, the default user is in it).

## What it does

| | Game Boy / Color | Game Boy Advance |
|---|---|---|
| Read ROM (checked against the FlashGBX database, or the header checksum) | yes | yes |
| Read save | yes (MBC1/2/3/5) | yes (SRAM) |
| Write save (read back, then re-read after a power cycle) | yes | yes (SRAM) |
| Flash a ROM on a rewritable cartridge | yes (MBC5, AMD flash, 8 KiB and 64 KiB sectors) | yes (AMD flash, whole-chip erase) |

Everything that changes a cartridge asks for a file, then for a confirmation with `Y`; before writing, the whole
ROM chip (or the current save) is backed up to `~/zerocart/backups` from two reads that must agree, and the result is
read back and compared. The 3.3 V GBA mode is the default; the Game Boy 5 V mode needs the explicit `G` key.

Not handled yet: other Game Boy mappers, flash chips with buffered writes or other command sets (they are refused,
not guessed), GBxCart-specific firmware features, the e-Reader and the Pocket Camera.

## Using it

Start **ZeroCart** from the APPLaunch menu. Put the ROMs (`.gb`, `.gbc`, `.gba`) and saves (`.sav`) you want to write
in `~/zerocart`; dumps are written there too.

| Key | |
|---|---|
| Left / Right | tab (Cartridge, Actions, Tools) |
| Up / Down, Enter | choose, run |
| `R` | read the cartridge again |
| `G` | Game Boy mode (5 V) |
| `Y` | confirm a write |
| Esc | cancel (only before the writing starts), quit |

Command line tools (`zerocart-chis` and `zerocart-chis-flash` are for the Chisflash reader, original firmware; `zerocart-md` is a read-only test tool for the Krikzz Mega Drive flasher): `zerocart --info | --dump-rom FILE | --dump-save FILE | --write-save FILE [--agb]`, and
`zerocart-flash --probe [--agb]`, `--rewrite-sector N --rom FILE`, `--flash-rom FILE --backup BACKUP [--yes]`
(dry run unless `--yes`).

## Build

```shell
cargo test --release
packaging/build-deb.sh        # needs the aarch64 Rust target and aarch64-linux-gnu-gcc; output in dist/
```

## Licence

GPL-3.0-or-later (the protocol and the databases come from FlashGBX, GPL-3.0). Some files copied from ZeroTag stay
MIT, see `THIRD_PARTY.md`.
