# Changelog

## 1.2.1

- Mega Drive: a cartridge without a SEGA header (blank, or erased by a failed flash) can be flashed by hand again. "Flash ROM" no longer wants a header, only that the flash chip answers (it says so when it does not), and the Tools tab analyses the chip without a header too. The banner now says that flashing is still possible. Reading still needs a cartridge.

## 1.2.0

- Serial flash mode, new process: no more automatic detection of the cartridge. You put it in, press `Enter`, and ZeroCart looks for the chip, erases, writes and verifies; the verdict stays on screen until `Enter` goes on to the next cartridge (`R` retries a failed one). The connector is no longer polled while waiting (it powered the cartridge on and off every second, and started the write as soon as the chip answered, possibly before the cartridge was fully in).
- A cartridge whose chip does not answer is not touched: the screen says so and `Enter` tries again, without counting a failure.
- Mega Drive: a failed verification now says how many bytes differ, the first address with the expected and the read value, and whether a second read gives the same bytes (bad write) or others (bad contact). If the second read is right, the cartridge is counted as good and the message warns about the contact.

## 1.1.1

- The footer of the main screen offers a single `R` read action. It used to list "read GB" and "read GBA" whatever the reader (the MD Flasher included), although the system is chosen at the start and `G` does nothing.

## 1.1.0

- Serial flash mode (`I` on the system screen): one ROM written on cartridge after cartridge, for blank cartridges, or any cartridge you accept to overwrite (the contents are never looked at, nothing is backed up: at the user's own risk). Pick the system and the reader as usual, then the ROM; after that the screen only says what to do. A jingle asks for a cartridge (ding), the flash chip answering is the proof that one is in (a blank cartridge has no header), a double ding starts the work (erase, write, verify, no backup), a fanfare says "done, eject" (or a low buzz says it failed), and as soon as the connector is seen empty the next cartridge is asked for. `Enter` on the eject screen goes on when a quick swap went unnoticed; `Esc` stops (not while a cartridge is being written). Counts of good and failed cartridges and the time of the last one are shown. Works for Game Boy / Color (taken as MBC5), Game Boy Advance and Mega Drive.
- The screen is a small cracktro: copper bars, a shadowed logo with a glint, a starfield and a sine-wave scroller. The jingles are chiptune synthesised in memory and played with `aplay` (the package now depends on `alsa-utils`); `--no-sound` silences them.
- Limit: a reader that cannot cut the power (Chisflash) keeps the cartridge powered while it is swapped; the GBFlash is powered down between looks.

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
