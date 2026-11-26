# Changelog

## 1.0.5

- New icon: the ZeroStack family logo (neon cartridge with the slashed zero, pixel-notched tile), shared with ZeroTag and ZeroTier. Source in `assets/logo/zerocart.svg`, rendered by `packaging/make_icon.py`.

## 1.0.4

- Chisflash: the ChisFlash Beggar Boost firmware (same USB ids as the original one) is recognised by a `55 AA` probe and shown as "Chisflash Boost" with its firmware text; the reader is resynchronised after the probe, nothing is written to the cartridge. It is still driven with the original commands, which that firmware keeps. `zerocart-chis --info` prints which firmware answered.
- The backup before a ROM flash is optional: `B` on the confirmation screen writes without it, with a red raster-bar scrolling warning ("no backup, at your own risk") and a confirmation line saying the old contents will be lost. The result message says that no backup was made. Saves are always backed up.

## 1.0.3

- Chisflash, GBA: cartridges whose flash chip needs another programming sequence than the one the reader's firmware sends (crossed data lines, `A9`/`56` unlock) are now programmed word by word with the profile's own sequence. Slow (about 4.5 ms a word, 42 minutes for 2 MiB) but verified on hardware: a 2 MiB cartridge was flashed and read back identical. Cartridges the firmware command suits keep the fast path.
- New `zerocart-chis-gba-flash` tool (backup, erase, program, verify; a dry run without `--yes`).

## 1.0.2

- Chisflash: flashing a GBA ROM is now supported. Backup of the whole chip from two agreeing reads, chip erase with the profile's commands, programming with the reader's own command, read back and repair. Chips whose programming sequence is not the one the firmware sends are refused before anything is erased. New and not yet tried on real hardware: use a cartridge you can afford to lose first.
- Chisflash: the Tools tab analysed a GBA cartridge on the Game Boy bus; it now uses the GBA bus.

## 1.0.1

- ZeroSend now saves the files it receives in `~/zerocart` (the folder the flash screens look in) instead of `~/Downloads`: a ROM sent from a phone or a computer is listed at once.
- The "no file" message of the flash screens names every accepted extension (`.gb / .gbc`, `.bin / .md / .gen`, ...), so a Game Boy Color `.gbc` ROM is no longer thought to be refused.

## 1.0.0

First release of ZeroCart: an all-in-one cartridge reader and flasher for the M5Stack CardputerZero.

**Systems and readers**
- Game Boy, Game Boy Color, Game Boy Advance and Mega Drive, chosen on the first screen (it sets the mode and the voltage).
- Readers: GBFlash / GBxCart RW, Chisflash (original firmware) and the MD Flasher (Krikzz FlashKit MD), found by USB.

**Reading**
- ROM dumps checked against the FlashGBX database or the header checksum (Mega Drive: CRC32 and header checksum).
- A Game Boy read that comes back wrong (a burst of bad bytes seen on hardware) is put right on its own: second pass, then a
  vote per 16 KiB bank, and the result says what happened.
- Saves for Game Boy (MBC1/2/3/5), GBA (SRAM) and Mega Drive; a reader that cannot cut the power flags them "not verified".

**Writing** (always a file picker, a confirmation on `Y`, a backup from two agreeing reads, and a read back)
- Game Boy and GBA: flash a ROM on rewritable cartridges (CFI, AMD flash, sector by sector); save writing with a power-cycle check
  where the reader can cut the power.
- Mega Drive: flash a ROM (Spansion S29GL064N tested, 1 MiB in about 25 s with the chip's write buffer; a cartridge wired for 4 MiB is
  detected before anything is erased) and write a save to a cartridge with a real save RAM.

**ZeroSend (built in)**: LocalSend v2 file sharing in Rust (HTTPS with certificates, multicast and subnet-scan discovery): send dumps and
saves to a phone or a computer, receive files, text messages; every transfer is accepted or confirmed on screen. Tested with the LocalSend app.

**Interface**: 320x170 Famicom-style screens in French, English, Japanese and Simplified Chinese (key `L`).

**Tools**: `zerocart`, `zerocart-flash`, `zerocart-chis`, `zerocart-chis-flash`, `zerocart-md`, `zerocart-share`.

Based on the protocol and chip databases of FlashGBX by Lesserkuma (GPL-3.0), flashkit-md-dotnet (MIT) and ZeroSend (MIT); see THIRD_PARTY.md.
