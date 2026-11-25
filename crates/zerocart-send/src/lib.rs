// SPDX-License-Identifier: GPL-3.0-or-later
//! File sharing compatible with LocalSend (protocol v2), embedded in ZeroCart.
//!
//! Written from the open protocol (https://github.com/localsend/protocol) and from ZeroSend
//! (https://github.com/InstantComet/zerosend, MIT, see THIRD_PARTY.md), which does the same in Python for the
//! CardputerZero. Everything here works on plain `Read`/`Write` streams so that it can be tested without a network:
//! the TLS and UDP layers are added around it.
//!
//! - [`protocol`]: the JSON messages and the constants of the protocol.
//! - [`identity`]: the self-signed certificate of the device; its SHA-256 is the device's LocalSend fingerprint.
//! - [`http`]: the small part of HTTP/1.1 the protocol needs (requests with a body, responses).
//! - [`receive`]: the receiving side (register, prepare-upload, upload, cancel) with the user's accept/decline.
pub mod http;
pub mod identity;
pub mod protocol;
pub mod receive;
