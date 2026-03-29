//! `wtd-ui` — WinTermDriver UI process.
//!
//! Graphical Windows application. Creates native windows, renders the tab
//! strip and pane splitters, and renders terminal content using Direct2D +
//! DirectWrite (decision: ADR-001, wintermdriver-6en.1).
//!
//! UI rendering technology is Win32 + DirectWrite. See spec §8.2 and §24.

pub mod clipboard;
pub mod input;
pub mod mouse_handler;
pub mod pane_layout;
pub mod prefix_state;
pub mod renderer;
pub mod status_bar;
pub mod tab_strip;
pub mod window;
