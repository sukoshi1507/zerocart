# ZeroCart

All-in-one cartridge reader and flasher for the M5Stack **CardputerZero**: Game Boy, Game Boy Color, Game Boy
Advance and Mega Drive, with a 320x170 Famicom-style interface driven by the keyboard, in French, English, Japanese
and Simplified Chinese. The Game Boy and GBA side is a Rust port of the protocol and chip databases of
[FlashGBX](https://github.com/lesserkuma/FlashGBX) by Lesserkuma; ZeroSend, built in, exchanges dumps and saves with
phones and computers running [LocalSend](https://localsend.org).

## Hardware

One reader at a time in the CardputerZero USB port; ZeroCart finds out which one is plugged in.

| Reader | For | Notes |
|---|---|---|
| **GBFlash** (tested: v1.3, firmware L15) / GBxCart RW | Game Boy, Game Boy Color, Game Boy Advance | sets the voltage itself (5 V / 3.3 V). GBxCart RW should work through the same protocol but has not been tried. |
| **Chisflash** (original firmware, USB 0483:0721; the Boost firmware is recognised and shown as "Chisflash Boost", but it is driven like the original one) | Game Boy, Game Boy Color, Game Boy Advance | cannot cut the power: saves are flagged "not verified". GBA ROM flashing with it (new in 1.0.2) was tried on one cartridge (Vampire Killer, 2 MiB, crossed data lines: 42 minutes). Use a cartridge you can afford to lose first. |
| **MD Flasher** (Krikzz FlashKit MD, FTDI 0403:6001) | Mega Drive | |

The user must be allowed to open the serial port (group `dialout`, the default user is in it).

## What it does

| | Game Boy / Color | Game Boy Advance | Mega Drive |
|---|---|---|---|
| Read ROM (checked against the FlashGBX database, or the header checksum) | yes | yes | yes (CRC32 and header checksum) |
| Read save | yes (MBC1/2/3/5) | yes (SRAM) | yes (declared in the header) |
| Write save (read back, then re-read after a power cycle) | yes | yes (SRAM) | yes, on a cartridge with a real save RAM (flash carts without RAM are refused; not yet tried on hardware) |
| Flash a ROM on a rewritable cartridge | yes (MBC5, AMD flash, 8 KiB and 64 KiB sectors) | yes (AMD flash, whole-chip erase; GBFlash only) | yes (AMD/Spansion flash, only the sectors that differ) |
| Identify the flash chip (CFI) | yes | yes | yes |

Everything that changes a cartridge asks for a file, then for a confirmation with `Y`; before writing, the whole
ROM chip (or the current save) is backed up to `~/zerocart/backups` from two reads that must agree, and the result is
read back and compared. A reader that cannot cut the cartridge's power cannot tell a dead save battery from a good
one: the saves it reads are flagged "not verified".

The system chosen on the first screen sets the mode and the voltage (Game Boy and Game Boy Color 5 V, Game Boy
Advance 3.3 V). When Game Boy is chosen on a reader that sets the voltage, ZeroCart first looks at 3.3 V whether the
cartridge is a Game Boy Advance one, and then does not go to 5 V.

Mega Drive: a standard flash cartridge is wired for 4 MiB. For a ROM larger than that, ZeroCart first tests the upper
half of the chip and refuses, before erasing anything, when it cannot be reached.

Not handled yet: other Game Boy mappers, flash chips with buffered writes or other command sets (they are refused,
not guessed), Mega Drive save writing, interleaved `.smd` files, GBxCart-specific firmware features, the e-Reader
and the Pocket Camera.

## Sharing with ZeroSend

`S` on the system screen opens ZeroSend, a Rust implementation of the LocalSend v2 protocol (TLS with a certificate
made on first use in `~/.config/zerocart/send`). The other devices on the Wi-Fi appear in a list (`A` adds one by
its address, `R` looks again); `Enter` picks a dump or a save from `~/zerocart` and sends it after a confirmation on
`Y`. What others send you is accepted or declined with `Y` / `N` and saved in `~/zerocart` (where the flash screens look for ROMs). The port is 53317;
LocalSend must not be running on the same machine.

## Languages

French, English, Japanese and Simplified Chinese. The first start follows the system's `LANG` (French if it is none
of these), `L` on the system screen changes it and remembers the choice in `~/.config/zerocart/lang`, and
`--lang fr|en|ja|zh` forces it for one run. Japanese and Chinese use the system's Noto Sans CJK font (the package
depends on `fonts-noto-cjk`). Every text is written once per language, next to the code that shows it, with the
`loc!` macro, so the compiler refuses a text that lacks a language; the messages of the worker thread are in
`crates/zerocart-app/src/msg.rs`.

## Using it

Start **ZeroCart** from the APPLaunch menu: logo, then the system (four icons), then the reader (checked live over
USB), then the tabs *Cartridge*, *Actions* and *Tools*. Put the ROMs (`.gb`, `.gbc`, `.gba`, `.bin`, `.md`, `.gen`) and
saves (`.sav`) you want to write in `~/zerocart`; dumps are written there too.

| Key | |
|---|---|
| `Z` / `C` (or Fn + arrows) | left / right: choose the system, change tab |
| `F` / `X` (or Fn + arrows) | up / down: choose a reader, an action, a file |
| Enter | confirm the choice, run |
| `S` / `L` (system screen) | ZeroSend / next language |
| `R` | read the cartridge again (after swapping it) |
| `Y` | confirm a write |
| Esc | back one level, cancel (only before the writing starts), quit |

## Command line tools

- `zerocart --info | --dump-rom FILE | --dump-save FILE | --write-save FILE [--agb]` (GBFlash).
- `zerocart-flash --probe [--agb]`, `--rewrite-sector N --rom FILE`, `--flash-rom FILE --backup BACKUP [--yes]`
  (GBFlash; a dry run unless `--yes`).
- `zerocart-chis --info | --gba-header | --gba-dump FILE | --gb-header | --gb-dump FILE` (Chisflash, read only).
- `zerocart-chis-flash --flash-rom --rom FILE --backup BACKUP.gb [--yes]` (Chisflash, Game Boy MBC5).
- `zerocart-chis-gba-flash --rom FILE --backup BACKUP.gba [--yes]` (Chisflash, GBA: backup, erase, program, verify; a dry run without `--yes`).
- `zerocart-md --info | --header | --dump FILE | --save FILE | --identify | --flash FILE --backup OUT [--yes] [--word-by-word]`
- `zerocart-share --list | --send TARGET FILE... | --text TARGET MESSAGE | --serve [--accept-all]` : ZeroSend (LocalSend) without the screen, to script it or test it (`--accept-all` skips the question: only on a network you trust)
  (MD Flasher; `--flash` is a dry run unless `--yes`).

## Build

```shell
cargo test --release
packaging/build-deb.sh        # needs the aarch64 Rust target and aarch64-linux-gnu-gcc; output in dist/
```

## Licence

GPL-3.0-or-later (the protocol and the databases come from FlashGBX, GPL-3.0). Some files copied from ZeroTag stay
MIT, see `THIRD_PARTY.md`.
