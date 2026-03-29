//! `wtd-ui` — WinTermDriver UI process.
//!
//! Graphical Windows application. Creates native windows, renders the tab
//! strip and pane splitters, and renders terminal content using Direct2D +
//! DirectWrite (decision: ADR-001, wintermdriver-6en.1).
//!
//! UI rendering technology is Win32 + DirectWrite. See spec §8.2 and §24.

pub mod renderer;
pub mod tab_strip;
pub mod window;
