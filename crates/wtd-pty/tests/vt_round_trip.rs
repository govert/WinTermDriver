//! VT round-trip tests: feed a VT snapshot into a ScreenBuffer, re-snapshot,
//! feed into a second buffer, and verify the two buffers match cell-by-cell.
//!
//! Fixtures captured from FrankenTUI demo-showcase running inside WTD.

use wtd_pty::ScreenBuffer;

const COLS: u16 = 80;
const ROWS: u16 = 24;

fn load_fixture(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn load_fixture_text(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Feed VT data into a fresh 80×24 ScreenBuffer.
fn screen_from_vt(data: &[u8]) -> ScreenBuffer {
    let mut buf = ScreenBuffer::new(COLS, ROWS, 0);
    buf.advance(data);
    buf
}

/// Extract plain text from a single row.
fn row_text(screen: &ScreenBuffer, row: usize) -> String {
    let mut line = String::new();
    for col in 0..screen.cols() {
        if let Some(cell) = screen.cell(row, col) {
            if !cell.attrs.is_wide_continuation() {
                line.push_str(cell.text.as_str());
            }
        }
    }
    line
}

/// Extract visible plain text from the screen (one line per row, trailing
/// whitespace trimmed, with a trailing newline on each row).
fn visible_text(screen: &ScreenBuffer) -> String {
    let mut out = String::new();
    for row in 0..screen.rows() {
        let mut line = String::new();
        for col in 0..screen.cols() {
            if let Some(cell) = screen.cell(row, col) {
                if !cell.attrs.is_wide_continuation() {
                    line.push_str(cell.text.as_str());
                }
            }
        }
        out.push_str(line.trim_end_matches(' '));
        out.push('\n');
    }
    out
}

// ── Plain text fidelity ─────────────────────────────────────────────────────

#[test]
fn widget_gallery_plain_text_matches_fixture() {
    let vt = load_fixture("ftui_widget_gallery.vt");
    let screen = screen_from_vt(&vt);
    let actual = visible_text(&screen);
    let expected = load_fixture_text("ftui_widget_gallery.txt");

    // Compare line by line for readable diffs.
    // Trim trailing whitespace from both sides since the fixture may have
    // trailing spaces from the capture tool.
    let actual_lines: Vec<&str> = actual.lines().map(|l| l.trim_end()).collect();
    let expected_lines: Vec<&str> = expected.lines().map(|l| l.trim_end()).collect();

    assert_eq!(
        actual_lines.len(),
        expected_lines.len(),
        "row count mismatch: got {} expected {}",
        actual_lines.len(),
        expected_lines.len()
    );

    for (i, (a, e)) in actual_lines.iter().zip(expected_lines.iter()).enumerate() {
        assert_eq!(a, e, "row {i} text mismatch");
    }
}

// ── VT round-trip: snapshot → parse → re-snapshot → compare ─────────────────

#[test]
fn widget_gallery_vt_round_trip() {
    let vt = load_fixture("ftui_widget_gallery.vt");

    // First pass: VT fixture → screen A.
    let screen_a = screen_from_vt(&vt);

    // Re-snapshot screen A.
    let snapshot_a = screen_a.to_vt_snapshot();

    // Second pass: snapshot A → screen B.
    let screen_b = screen_from_vt(&snapshot_a);

    // Compare every cell.
    let rows = screen_a.rows();
    let cols = screen_a.cols();
    assert_eq!(rows, screen_b.rows());
    assert_eq!(cols, screen_b.cols());

    let mut mismatches = Vec::new();

    for row in 0..rows {
        for col in 0..cols {
            let ca = screen_a.cell(row, col).unwrap();
            let cb = screen_b.cell(row, col).unwrap();

            let mut diffs = Vec::new();
            if ca.text.as_str() != cb.text.as_str() {
                diffs.push(format!(
                    "text: {:?} vs {:?}",
                    ca.text.as_str(),
                    cb.text.as_str()
                ));
            }
            if ca.fg != cb.fg {
                diffs.push(format!("fg: {:?} vs {:?}", ca.fg, cb.fg));
            }
            if ca.bg != cb.bg {
                diffs.push(format!("bg: {:?} vs {:?}", ca.bg, cb.bg));
            }
            if ca.attrs != cb.attrs {
                diffs.push(format!("attrs: {:?} vs {:?}", ca.attrs, cb.attrs));
            }

            if !diffs.is_empty() {
                mismatches.push(format!("  [{row},{col}]: {}", diffs.join("; ")));
            }
        }
    }

    if !mismatches.is_empty() {
        let count = mismatches.len();
        // Show first 30 mismatches to keep output readable.
        let shown: Vec<&str> = mismatches.iter().take(30).map(|s| s.as_str()).collect();
        panic!(
            "VT round-trip: {count} cell mismatches (showing first {}):\n{}",
            shown.len(),
            shown.join("\n")
        );
    }
}

// ── Re-snapshot byte equality ───────────────────────────────────────────────

#[test]
fn widget_gallery_re_snapshot_is_stable() {
    let vt = load_fixture("ftui_widget_gallery.vt");
    let screen_a = screen_from_vt(&vt);
    let snap_a = screen_a.to_vt_snapshot();

    let screen_b = screen_from_vt(&snap_a);
    let snap_b = screen_b.to_vt_snapshot();

    // Once the snapshot is fed and re-emitted, the bytes should be identical.
    // (The first fixture → snapshot may differ because the original app output
    // uses different SGR grouping, but snapshot → snapshot must be stable.)
    assert_eq!(
        snap_a, snap_b,
        "re-snapshot bytes differ (len {} vs {})",
        snap_a.len(),
        snap_b.len()
    );
}

// ── Spot checks on specific cells ───────────────────────────────────────────

#[test]
fn widget_gallery_spot_checks() {
    let vt = load_fixture("ftui_widget_gallery.vt");
    let screen = screen_from_vt(&vt);

    // Row 1 should contain "Widget Gallery" title (inside the box).
    let row1_text = row_text(&screen, 1);
    assert!(
        row1_text.contains("Widget Gallery"),
        "row 1 should contain 'Widget Gallery', got: {row1_text:?}"
    );

    // Row 0 should contain tab labels.
    let row0_text = row_text(&screen, 0);
    assert!(
        row0_text.contains("Widgets"),
        "row 0 should contain 'Widgets' tab label, got: {row0_text:?}"
    );

    // Top-left of the outer box (row 1, col 0) should be ╭.
    let corner = screen.cell(1, 0).unwrap();
    assert_eq!(corner.text.as_str(), "╭", "top-left corner should be ╭");

    // Bottom-right of the outer box (row 22, col 79) should be ╯.
    let corner_br = screen.cell(22, 79).unwrap();
    assert_eq!(
        corner_br.text.as_str(),
        "╯",
        "bottom-right corner should be ╯"
    );
}
