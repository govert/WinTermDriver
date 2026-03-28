//! PTY handle and child process stubs.
//!
//! Full `CreatePseudoConsole` lifecycle is implemented in wintermdriver-mtz.1.

/// Size of a pseudo-console in columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_size_fields() {
        let sz = PtySize::new(80, 24);
        assert_eq!(sz.cols, 80);
        assert_eq!(sz.rows, 24);
    }
}
