//! Monitor layout, edge switching and config for INTmouse. No OS input hooks.

mod config;
mod layout;
mod switching;

pub use config::*;
pub use layout::*;
pub use switching::*;
