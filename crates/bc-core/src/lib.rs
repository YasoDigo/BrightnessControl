//! BrightnessControl core: hardware access, settings and session logic.
//! Nothing in here depends on the UI toolkit.

pub mod backend;
pub mod ddc;
pub mod discovery;
pub mod internal;
pub mod legacy;
pub mod logic;
pub mod paths;
pub mod session;
pub mod settings;
pub mod vcp;
pub mod worker;
