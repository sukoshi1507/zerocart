//! Flash cartridge profiles (the `fc_*.txt` files of FlashGBX) and chip detection.
pub mod agb;
pub mod cfi;
pub mod chis_agb;
pub mod chis_gb;
pub mod detect;
pub mod profile;
pub mod program;

pub use detect::{detect, FlashBus};
pub use profile::{load_dir, Profile};
