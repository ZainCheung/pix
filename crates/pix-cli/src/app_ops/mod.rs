//! Terminal-free application operations shared by the explicit CLI and TUI.
//!
//! The command modules remain responsible for choosing text, JSON, or prompt
//! presentation.  Operations in this module only inspect or mutate Pix state
//! and return structured values.

pub(crate) mod device;
