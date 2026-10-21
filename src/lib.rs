#![cfg_attr(not(test), deny(clippy::unwrap_used))]
//! omaquill's core: Scrivener projects read and written without a GUI.

pub mod compile;
pub mod enchant;
pub mod export;
pub mod fonts;
pub mod fsdir;
pub mod hyphen;
pub mod pdf;
pub mod project;
pub mod replace;
pub mod rtf;
pub mod sha1;
mod snapshot;
pub mod spell;
pub mod state;
pub mod xml;
pub mod zip;
