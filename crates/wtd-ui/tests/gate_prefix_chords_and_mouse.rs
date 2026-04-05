//! Gate integration test: Prefix chords and mouse interactions (§21.3, §21.6).
//!
//! Proves the interactive keyboard and mouse pipeline:
//! 1. Ctrl+B,% dispatches `split-right` via prefix chord
//! 2. Ctrl+B,o dispatches `focus-next-pane` via prefix chord
//! 3. Mouse click on a different pane changes focus
//! 4. Splitter drag between split panes produces resize actions
//! 5. Full chord sequence: activate prefix, verify active state, dispatch chord, verify idle
//! 6. Mouse and keyboard interact correctly (chord then click)

#![cfg(windows)]

use wtd_core::global_settings::tmux_bindings;
use wtd_core::ids::PaneId;
use wtd_core::layout::{LayoutTree, Rect};
use wtd_core::workspace::ActionReference;
use wtd_ui::input::{InputClassifier, KeyEvent, KeyName, Modifiers};
use wtd_ui::pane_layout::{PaneLayout, PaneLayoutAction};
use wtd_ui::prefix_state::{PrefixOutput, PrefixStateMachine};

// ── Helpers ──────────────────────────────────────────────────────────────

fn make_key(key: KeyName, mods: Modifiers, character: Option<char>) -> KeyEvent {
    KeyEvent {
        key,
        modifiers: mods,
        character,
    }
}

fn action_name(action: &ActionReference) -> &str {
    match action {
        ActionReference::Simple(s) => s.as_str(),
        ActionReference::WithArgs { action, .. } => action.as_str(),
        ActionReference::Removed => "",
    }
}

fn ctrl_b() -> KeyEvent {
    make_key(KeyName::Char('B'), Modifiers::CTRL, None)
}

fn percent() -> KeyEvent {
    // '%' is Shift+5 on US keyboard layout
    KeyEvent {
        key: KeyName::Digit(5),
        modifiers: Modifiers::SHIFT,
        character: Some('%'),
    }
}

fn letter_o() -> KeyEvent {
    KeyEvent {
        key: KeyName::Char('O'),
        modifiers: Modifiers::NONE,
        character: Some('o'),
    }
}

/// Create a default PrefixStateMachine from the built-in bindings.
fn default_psm() -> PrefixStateMachine {
    let bindings = tmux_bindings();
    let classifier = InputClassifier::from_bindings(&bindings).unwrap();
    PrefixStateMachine::new(classifier)
}

/// Build a two-pane split layout with pane layout component.
/// Returns (tree, pane1, pane2, pane_layout) where pane1 is on the left, pane2 on the right.
fn setup_split_layout() -> (LayoutTree, PaneId, PaneId, PaneLayout) {
    let mut tree = LayoutTree::new();
    let pane1 = tree.focus();
    let pane2 = tree.split_right(pane1.clone()).unwrap();

    // Use fixed cell dimensions
    let cell_w = 8.0_f32;
    let cell_h = 16.0_f32;

    let mut pane_layout = PaneLayout::new(cell_w, cell_h);
    // Content area: 80 cols x 24 rows, starting at y=32 (below tab strip)
    pane_layout.update(&tree, 0.0, 32.0, 80, 24);

    (tree, pane1, pane2, pane_layout)
}

// ── Test 1: Ctrl+B,% dispatches split-right ──────────────────────────────

#[test]
fn prefix_chord_percent_dispatches_split_right() {
    let mut psm = default_psm();

    // Press Ctrl+B → enters prefix mode
    let result = psm.process(&ctrl_b());
    assert!(
        matches!(result, PrefixOutput::Consumed),
        "Ctrl+B must be consumed to enter prefix mode"
    );
    assert!(psm.is_prefix_active(), "prefix must be active after Ctrl+B");

    // Press % → dispatches split-right chord
    let result = psm.process(&percent());
    assert!(
        !psm.is_prefix_active(),
        "prefix must return to idle after chord dispatch"
    );
    match result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(
                action_name(&action),
                "split-right",
                "Ctrl+B,% must dispatch 'split-right'"
            );
        }
        other => panic!(
            "expected DispatchAction(split-right) for Ctrl+B,%, got: {:?}",
            other
        ),
    }
}

// ── Test 2: Ctrl+B,o dispatches focus-next-pane ──────────────────────────

#[test]
fn prefix_chord_o_dispatches_focus_next_pane() {
    let mut psm = default_psm();

    // Press Ctrl+B → enters prefix mode
    let result = psm.process(&ctrl_b());
    assert!(matches!(result, PrefixOutput::Consumed));
    assert!(psm.is_prefix_active());

    // Press 'o' → dispatches focus-next-pane chord
    let result = psm.process(&letter_o());
    assert!(!psm.is_prefix_active());
    match result {
        PrefixOutput::DispatchAction(action) => {
            assert_eq!(
                action_name(&action),
                "focus-next-pane",
                "Ctrl+B,o must dispatch 'focus-next-pane'"
            );
        }
        other => panic!(
            "expected DispatchAction(focus-next-pane) for Ctrl+B,o, got: {:?}",
            other
        ),
    }
}

// ── Test 3: Mouse click on a different pane produces FocusPane ───────────

#[test]
fn mouse_click_changes_pane_focus() {
    let (_tree, pane1, pane2, mut pane_layout) = setup_split_layout();

    // Get pixel rects for both panes
    let rect1 = pane_layout.pane_pixel_rect(&pane1).unwrap();
    let rect2 = pane_layout.pane_pixel_rect(&pane2).unwrap();

    // Pane1 is on the left, pane2 is on the right (from split_right)
    assert!(rect2.x > rect1.x, "pane2 must be to the right of pane1");

    // Create a stub TabStrip (we need a DirectWrite factory — use the pane_layout approach instead)
    // Instead, test via PaneLayout.on_mouse_down directly, which is what MouseHandler delegates to.

    // Click in the center of pane2
    let click_x = rect2.x + rect2.width / 2.0;
    let click_y = rect2.y + rect2.height / 2.0;

    let action = pane_layout.on_mouse_down(click_x, click_y);
    match action {
        Some(PaneLayoutAction::FocusPane(id)) => {
            assert_eq!(
                id, pane2,
                "clicking in pane2's area must produce FocusPane(pane2)"
            );
        }
        other => panic!("expected FocusPane(pane2), got: {:?}", other),
    }

    // Click in the center of pane1
    let click_x = rect1.x + rect1.width / 2.0;
    let click_y = rect1.y + rect1.height / 2.0;

    let action = pane_layout.on_mouse_down(click_x, click_y);
    match action {
        Some(PaneLayoutAction::FocusPane(id)) => {
            assert_eq!(
                id, pane1,
                "clicking in pane1's area must produce FocusPane(pane1)"
            );
        }
        other => panic!("expected FocusPane(pane1), got: {:?}", other),
    }
}

// ── Test 4: Splitter drag produces resize actions ────────────────────────

#[test]
fn splitter_drag_resizes_panes() {
    let (tree, pane1, _pane2, mut pane_layout) = setup_split_layout();

    let cell_w = 8.0_f32;

    // Verify we have a splitter between the two panes
    assert!(
        pane_layout.splitter_count() > 0,
        "split layout must have at least one splitter"
    );

    // Find the splitter position: it's at the boundary between pane1 and pane2.
    // Pane1 is on the left half, pane2 is on the right half.
    let rect1 = pane_layout.pane_pixel_rect(&pane1).unwrap();
    let splitter_x = rect1.x + rect1.width; // right edge of pane1
    let splitter_y = rect1.y + rect1.height / 2.0; // vertically centered

    // Mouse down on the splitter
    let action = pane_layout.on_mouse_down(splitter_x, splitter_y);
    // Clicking on a splitter starts a drag — returns None (no immediate action)
    assert!(
        action.is_none(),
        "clicking on splitter should start drag, not produce an action: {:?}",
        action
    );
    assert!(
        pane_layout.is_dragging(),
        "pane_layout must be in drag mode after clicking splitter"
    );

    // Drag to the right by enough pixels to produce a resize (>= 1 cell width)
    let drag_x = splitter_x + cell_w * 2.0; // move 2 cells right
    let resize_action = pane_layout.on_mouse_move(drag_x, splitter_y);

    match resize_action {
        Some(PaneLayoutAction::Resize {
            pane_id,
            direction,
            cells,
        }) => {
            assert_eq!(pane_id, pane1, "resize must reference pane_before (pane1)");
            assert_eq!(
                direction,
                wtd_core::layout::ResizeDirection::GrowRight,
                "dragging right must produce GrowRight"
            );
            assert!(
                cells >= 1,
                "dragging 2 cell widths must produce at least 1 cell of resize"
            );
        }
        None => {
            // In case the remainder hasn't accumulated enough, try a larger drag
            let drag_x2 = splitter_x + cell_w * 4.0;
            let resize_action2 = pane_layout.on_mouse_move(drag_x2, splitter_y);
            assert!(
                resize_action2.is_some(),
                "dragging splitter far enough must produce a resize action"
            );
        }
        other => panic!("expected Resize action, got: {:?}", other),
    }

    // Release the mouse
    pane_layout.on_mouse_up(drag_x, splitter_y);
    assert!(!pane_layout.is_dragging(), "drag must end on mouse up");

    // Verify the resize actually changed the layout by applying it to the tree
    let mut tree_mut = tree;
    let total_rect = Rect::new(0, 0, 80, 24);
    let rects_before = tree_mut.compute_rects(total_rect);
    let pane1_width_before = rects_before[&pane1].width;

    // Apply a resize
    tree_mut
        .resize_pane(
            pane1.clone(),
            wtd_core::layout::ResizeDirection::GrowRight,
            2,
            total_rect,
        )
        .unwrap();

    let rects_after = tree_mut.compute_rects(total_rect);
    let pane1_width_after = rects_after[&pane1].width;

    assert!(
        pane1_width_after > pane1_width_before,
        "pane1 must be wider after GrowRight resize: before={}, after={}",
        pane1_width_before,
        pane1_width_after
    );
}

// ── Test 5: Full prefix chord lifecycle with state verification ──────────

#[test]
fn prefix_chord_full_lifecycle() {
    let mut psm = default_psm();

    // Initial state: idle
    assert!(!psm.is_prefix_active(), "must start idle");
    assert_eq!(psm.prefix_label(), "Ctrl+B");

    // Step 1: activate prefix
    let result = psm.process(&ctrl_b());
    assert!(matches!(result, PrefixOutput::Consumed));
    assert!(psm.is_prefix_active(), "must be active after Ctrl+B");

    // Step 2: dispatch chord (%)
    let result = psm.process(&percent());
    assert!(!psm.is_prefix_active(), "must be idle after chord");
    assert!(
        matches!(result, PrefixOutput::DispatchAction(ref a) if action_name(a) == "split-right")
    );

    // Step 3: re-activate and use a different chord (o)
    psm.process(&ctrl_b());
    assert!(psm.is_prefix_active());

    let result = psm.process(&letter_o());
    assert!(!psm.is_prefix_active());
    assert!(
        matches!(result, PrefixOutput::DispatchAction(ref a) if action_name(a) == "focus-next-pane")
    );

    // Step 4: prefix key still works after previous chord sequences
    psm.process(&ctrl_b());
    assert!(psm.is_prefix_active());

    // Cancel with Escape
    let esc = make_key(KeyName::Escape, Modifiers::NONE, None);
    let result = psm.process(&esc);
    assert!(matches!(result, PrefixOutput::Consumed));
    assert!(!psm.is_prefix_active(), "must be idle after Escape cancel");
}

// ── Test 6: Mouse click through MouseHandler produces FocusPane ──────────

#[test]
fn mouse_handler_click_produces_focus_output() {
    let (_tree, pane1, pane2, mut pane_layout) = setup_split_layout();

    let tab_strip_height = 32.0_f32;

    // MouseHandler delegates to PaneLayout.on_mouse_down for the content area.
    // We test the PaneLayout interaction directly since TabStrip::new() requires
    // a DirectWrite factory (only available on windows with D2D init).

    // Click in pane2 (right side)
    let rect2 = pane_layout.pane_pixel_rect(&pane2).unwrap();
    let click_x = rect2.x + rect2.width / 2.0;
    let click_y = rect2.y + rect2.height / 2.0;

    // Verify the click is in the content area (below tab strip)
    assert!(click_y >= tab_strip_height);

    // Use PaneLayout directly (MouseHandler delegates to it)
    let action = pane_layout.on_mouse_down(click_x, click_y);
    match action {
        Some(PaneLayoutAction::FocusPane(id)) => {
            assert_eq!(id, pane2, "click must focus pane2");
        }
        other => panic!("expected FocusPane(pane2), got: {:?}", other),
    }
    // Release
    pane_layout.on_mouse_up(click_x, click_y);

    // Now click in pane1 (left side)
    let rect1 = pane_layout.pane_pixel_rect(&pane1).unwrap();
    let click_x = rect1.x + rect1.width / 2.0;
    let click_y = rect1.y + rect1.height / 2.0;

    let action = pane_layout.on_mouse_down(click_x, click_y);
    match action {
        Some(PaneLayoutAction::FocusPane(id)) => {
            assert_eq!(id, pane1, "click must focus pane1");
        }
        other => panic!("expected FocusPane(pane1), got: {:?}", other),
    }
}

// ── Test 7: Splitter drag in reverse direction produces ShrinkRight ──────

#[test]
fn splitter_drag_left_produces_shrink() {
    let (_tree, pane1, _pane2, mut pane_layout) = setup_split_layout();

    let cell_w = 8.0_f32;

    let rect1 = pane_layout.pane_pixel_rect(&pane1).unwrap();
    let splitter_x = rect1.x + rect1.width;
    let splitter_y = rect1.y + rect1.height / 2.0;

    // Start drag on splitter
    pane_layout.on_mouse_down(splitter_x, splitter_y);
    assert!(pane_layout.is_dragging());

    // Drag to the left by enough for resize
    let drag_x = splitter_x - cell_w * 3.0;
    let action = pane_layout.on_mouse_move(drag_x, splitter_y);

    match action {
        Some(PaneLayoutAction::Resize { direction, .. }) => {
            assert_eq!(
                direction,
                wtd_core::layout::ResizeDirection::ShrinkRight,
                "dragging left must produce ShrinkRight"
            );
        }
        None => {
            // Try even larger drag
            let drag_x2 = splitter_x - cell_w * 6.0;
            let action2 = pane_layout.on_mouse_move(drag_x2, splitter_y);
            match action2 {
                Some(PaneLayoutAction::Resize { direction, .. }) => {
                    assert_eq!(
                        direction,
                        wtd_core::layout::ResizeDirection::ShrinkRight,
                        "dragging left must produce ShrinkRight"
                    );
                }
                other => panic!("expected Resize(ShrinkRight), got: {:?}", other),
            }
        }
        other => panic!("expected Resize(ShrinkRight), got: {:?}", other),
    }

    pane_layout.on_mouse_up(drag_x, splitter_y);
    assert!(!pane_layout.is_dragging());
}

// ── Test 8: Combined keyboard and mouse interaction ──────────────────────

#[test]
fn chord_then_mouse_click_works_independently() {
    let mut psm = default_psm();
    let (_tree, _pane1, pane2, mut pane_layout) = setup_split_layout();

    // First: execute a prefix chord to split-right
    psm.process(&ctrl_b());
    let result = psm.process(&percent());
    assert!(
        matches!(result, PrefixOutput::DispatchAction(ref a) if action_name(a) == "split-right"),
        "Ctrl+B,% must dispatch split-right"
    );
    assert!(!psm.is_prefix_active(), "must be idle after chord");

    // Then: mouse click on pane2 to focus it
    let rect2 = pane_layout.pane_pixel_rect(&pane2).unwrap();
    let click_x = rect2.x + rect2.width / 2.0;
    let click_y = rect2.y + rect2.height / 2.0;

    let action = pane_layout.on_mouse_down(click_x, click_y);
    assert!(
        matches!(action, Some(PaneLayoutAction::FocusPane(ref id)) if *id == pane2),
        "click after chord must still focus pane2: {:?}",
        action
    );

    // Then: another chord works after mouse interaction
    psm.process(&ctrl_b());
    assert!(psm.is_prefix_active());
    let result = psm.process(&letter_o());
    assert!(
        matches!(result, PrefixOutput::DispatchAction(ref a) if action_name(a) == "focus-next-pane"),
        "Ctrl+B,o after mouse click must dispatch focus-next-pane"
    );
}
