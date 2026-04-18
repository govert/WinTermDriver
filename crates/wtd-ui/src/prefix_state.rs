//! Prefix chord state machine (§21.3, §27.4).
//!
//! Wraps [`InputClassifier`] and manages the prefix-active / idle state
//! transitions, timeout, and visual indicator state.
//!
//! ```text
//! Idle ──► PrefixActive(timestamp)
//!            │         │         │         │         │
//!            ▼         ▼         ▼         ▼         ▼
//!       ChordMatch  Timeout  EscCancel  UnboundKey  PrefixAgain
//!            │         │         │         │         │
//!            ▼         ▼         ▼         ▼         ▼
//!           Idle      Idle      Idle      Idle      Idle
//! ```
//!
//! The caller drives the machine by calling [`PrefixStateMachine::process`]
//! for each key event and [`PrefixStateMachine::check_timeout`] periodically
//! in the UI event loop.

use std::time::{Duration, Instant};

use wtd_core::workspace::ActionReference;

use crate::input::{
    key_event_to_bytes, text_char_to_key_event, InputAction, InputClassifier, KeyEvent, KeyName,
    KeySpec, Modifiers,
};

// ── Output ──────────────────────────────────────────────────────────────────

/// Result of processing a key event through the prefix state machine.
#[derive(Debug, Clone)]
pub enum PrefixOutput {
    /// Dispatch a bound action (chord or single-stroke).
    DispatchAction(ActionReference),
    /// Send raw bytes to the focused session.
    SendToSession(Vec<u8>),
    /// Keystroke consumed — no further action needed.
    ///
    /// Returned when entering prefix mode, cancelling via Escape, or on timeout.
    Consumed,
}

// ── State Machine ───────────────────────────────────────────────────────────

/// Prefix chord state machine per §21.3 and §27.4.
///
/// Manages the two-state lifecycle:
/// - **Idle** — normal input mode; prefix key enters PrefixActive.
/// - **PrefixActive** — waiting for a chord key; timeout or Escape returns
///   to Idle.
///
/// Update the status bar's prefix indicator whenever [`is_prefix_active`]
/// changes:
///
/// ```ignore
/// status_bar.set_prefix_active(state_machine.is_prefix_active());
/// status_bar.set_prefix_label(state_machine.prefix_label().to_string());
/// ```
pub struct PrefixStateMachine {
    classifier: InputClassifier,
    active: bool,
    activated_at: Option<Instant>,
    timeout: Duration,
    /// Pre-computed bytes for the prefix key (double-press and unbound-key forwarding).
    prefix_bytes: Vec<u8>,
    /// Display label for the prefix key (e.g. "Ctrl+B").
    label: String,
}

impl PrefixStateMachine {
    /// Create a new state machine wrapping the given classifier.
    pub fn new(classifier: InputClassifier) -> Self {
        let timeout = Duration::from_millis(classifier.prefix_timeout_ms() as u64);
        let (prefix_bytes, label) = match classifier.prefix_key() {
            Some(spec) => (key_spec_to_bytes(spec), spec.to_string()),
            None => (Vec::new(), String::new()),
        };

        PrefixStateMachine {
            classifier,
            active: false,
            activated_at: None,
            timeout,
            prefix_bytes,
            label,
        }
    }

    /// Process a key event and return the resulting action.
    ///
    /// State transitions per §21.3:
    ///
    /// | Current state | Event | Output | New state |
    /// |---|---|---|---|
    /// | Idle | Prefix key | Consumed | PrefixActive |
    /// | Idle | Single-stroke binding | DispatchAction | Idle |
    /// | Idle | Any other key | SendToSession(bytes) | Idle |
    /// | PrefixActive | Chord key | DispatchAction | Idle |
    /// | PrefixActive | Prefix key again | SendToSession(prefix) | Idle |
    /// | PrefixActive | Escape | Consumed | Idle |
    /// | PrefixActive | Unbound key | SendToSession(prefix+key) | Idle |
    pub fn process(&mut self, event: &KeyEvent) -> PrefixOutput {
        if self.active {
            self.process_active(event)
        } else {
            self.process_idle(event)
        }
    }

    /// Check if the prefix has timed out.
    ///
    /// Returns `true` if a timeout occurred and the state was reset to Idle.
    /// Call this periodically in the event loop (e.g. on each iteration).
    pub fn check_timeout(&mut self) -> bool {
        if let Some(at) = self.activated_at {
            if at.elapsed() >= self.timeout {
                self.deactivate();
                return true;
            }
        }
        false
    }

    /// Whether the prefix is currently active (for status bar indicator).
    pub fn is_prefix_active(&self) -> bool {
        self.active
    }

    /// Display label for the prefix key (e.g. "Ctrl+B").
    pub fn prefix_label(&self) -> &str {
        &self.label
    }

    /// The configured timeout duration.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Access the inner classifier.
    pub fn classifier(&self) -> &InputClassifier {
        &self.classifier
    }

    /// Process committed text input under the same prefix semantics as key events.
    pub fn process_text(&mut self, text: &str) -> Vec<PrefixOutput> {
        let mut outputs = Vec::new();
        for ch in text.chars() {
            if let Some(event) = text_char_to_key_event(ch) {
                outputs.push(self.process(&event));
            } else {
                let mut bytes = ch.to_string().into_bytes();
                if self.active {
                    self.deactivate();
                    let mut prefixed = self.prefix_bytes.clone();
                    prefixed.append(&mut bytes);
                    outputs.push(PrefixOutput::SendToSession(prefixed));
                } else {
                    outputs.push(PrefixOutput::SendToSession(bytes));
                }
            }
        }
        outputs
    }

    // ── Internal ────────────────────────────────────────────────────────────

    fn activate(&mut self) {
        self.active = true;
        self.activated_at = Some(Instant::now());
    }

    fn deactivate(&mut self) {
        self.active = false;
        self.activated_at = None;
    }

    fn process_idle(&mut self, event: &KeyEvent) -> PrefixOutput {
        let action = self.classifier.classify(event, false);
        match action {
            InputAction::PrefixKey => {
                self.activate();
                PrefixOutput::Consumed
            }
            InputAction::SingleStrokeBinding(action_ref) => {
                PrefixOutput::DispatchAction(action_ref)
            }
            InputAction::RawInput(bytes) => PrefixOutput::SendToSession(bytes),
            // Should not occur when prefix_active=false.
            InputAction::ChordBinding(_) => PrefixOutput::Consumed,
        }
    }

    fn process_active(&mut self, event: &KeyEvent) -> PrefixOutput {
        // §21.3: Escape cancels prefix (plain Escape, no modifiers).
        if event.key == KeyName::Escape && event.modifiers == Modifiers::NONE {
            self.deactivate();
            return PrefixOutput::Consumed;
        }

        // §21.3: prefix key pressed again → send one literal prefix key.
        // Must check before classify(), which skips prefix detection when active.
        if let Some(prefix) = self.classifier.prefix_key() {
            if prefix.matches(event) {
                self.deactivate();
                return PrefixOutput::SendToSession(self.prefix_bytes.clone());
            }
        }

        let action = self.classifier.classify(event, true);
        match action {
            InputAction::ChordBinding(action_ref) => {
                self.deactivate();
                PrefixOutput::DispatchAction(action_ref)
            }
            InputAction::RawInput(key_bytes) => {
                // §21.3: unbound key → send prefix key + this key to session.
                self.deactivate();
                let mut bytes = self.prefix_bytes.clone();
                bytes.extend(key_bytes);
                PrefixOutput::SendToSession(bytes)
            }
            // Should not occur in active state, handle gracefully.
            InputAction::PrefixKey | InputAction::SingleStrokeBinding(_) => {
                self.deactivate();
                PrefixOutput::Consumed
            }
        }
    }
}

/// Convert a `KeySpec` to the raw terminal bytes it would produce.
fn key_spec_to_bytes(spec: &KeySpec) -> Vec<u8> {
    let event = KeyEvent {
        key: spec.key,
        modifiers: spec.modifiers,
        character: None,
    };
    key_event_to_bytes(&event)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use wtd_core::workspace::BindingsDefinition;

    /// Build a classifier with Ctrl+B prefix, one chord ("%"→"split-right"),
    /// one single-stroke (Ctrl+T→"new-tab"), and a short timeout for tests.
    fn test_bindings(timeout_ms: u32) -> BindingsDefinition {
        let mut chords = HashMap::new();
        chords.insert(
            "%".to_string(),
            ActionReference::Simple("split-right".to_string()),
        );
        chords.insert(
            "o".to_string(),
            ActionReference::Simple("zoom-pane".to_string()),
        );

        let mut keys = HashMap::new();
        keys.insert(
            "Ctrl+T".to_string(),
            ActionReference::Simple("new-tab".to_string()),
        );

        BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(timeout_ms),
            chords: Some(chords),
            keys: Some(keys),
        }
    }

    fn make_sm(timeout_ms: u32) -> PrefixStateMachine {
        let bindings = test_bindings(timeout_ms);
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        PrefixStateMachine::new(classifier)
    }

    fn ctrl_b() -> KeyEvent {
        KeyEvent {
            key: KeyName::Char('B'),
            modifiers: Modifiers::CTRL,
            character: None,
        }
    }

    fn escape() -> KeyEvent {
        KeyEvent {
            key: KeyName::Escape,
            modifiers: Modifiers::NONE,
            character: None,
        }
    }

    fn percent() -> KeyEvent {
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

    fn letter_x() -> KeyEvent {
        KeyEvent {
            key: KeyName::Char('X'),
            modifiers: Modifiers::NONE,
            character: Some('x'),
        }
    }

    fn ctrl_t() -> KeyEvent {
        KeyEvent {
            key: KeyName::Char('T'),
            modifiers: Modifiers::CTRL,
            character: None,
        }
    }

    // ── State transition tests ──────────────────────────────────────────────

    #[test]
    fn idle_prefix_key_enters_active() {
        let mut sm = make_sm(2000);
        assert!(!sm.is_prefix_active());

        let out = sm.process(&ctrl_b());
        assert!(sm.is_prefix_active());
        assert!(matches!(out, PrefixOutput::Consumed));
    }

    #[test]
    fn active_chord_dispatches_action() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());
        assert!(sm.is_prefix_active());

        let out = sm.process(&percent());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::DispatchAction(ActionReference::Simple(name)) => {
                assert_eq!(name, "split-right");
            }
            other => panic!("expected DispatchAction, got {:?}", other),
        }
    }

    #[test]
    fn active_second_chord_dispatches() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());

        let out = sm.process(&letter_o());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::DispatchAction(ActionReference::Simple(name)) => {
                assert_eq!(name, "zoom-pane");
            }
            other => panic!("expected DispatchAction, got {:?}", other),
        }
    }

    #[test]
    fn active_double_prefix_sends_literal() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());

        let out = sm.process(&ctrl_b());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::SendToSession(bytes) => {
                // Ctrl+B = 0x02 (STX)
                assert_eq!(bytes, vec![0x02]);
            }
            other => panic!("expected SendToSession, got {:?}", other),
        }
    }

    #[test]
    fn active_escape_cancels() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());
        assert!(sm.is_prefix_active());

        let out = sm.process(&escape());
        assert!(!sm.is_prefix_active());
        assert!(matches!(out, PrefixOutput::Consumed));
    }

    #[test]
    fn active_unbound_key_sends_prefix_plus_key() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());

        let out = sm.process(&letter_x());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::SendToSession(bytes) => {
                // Prefix bytes (Ctrl+B = 0x02) followed by 'x'
                assert_eq!(bytes, vec![0x02, b'x']);
            }
            other => panic!("expected SendToSession, got {:?}", other),
        }
    }

    #[test]
    fn active_unmapped_text_sends_prefix_plus_utf8_text() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());

        let outputs = sm.process_text("{");
        assert!(!sm.is_prefix_active());
        assert_eq!(outputs.len(), 1);
        match &outputs[0] {
            PrefixOutput::SendToSession(bytes) => {
                assert_eq!(bytes, &vec![0x02, b'{']);
            }
            other => panic!("expected SendToSession, got {other:?}"),
        }
    }

    #[test]
    fn active_composed_text_sends_prefix_plus_utf8_text() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());

        let outputs = sm.process_text("é");
        assert!(!sm.is_prefix_active());
        assert_eq!(outputs.len(), 1);
        match &outputs[0] {
            PrefixOutput::SendToSession(bytes) => {
                assert_eq!(bytes, &vec![0x02, 0xC3, 0xA9]);
            }
            other => panic!("expected SendToSession, got {other:?}"),
        }
    }

    #[test]
    fn idle_single_stroke_dispatches() {
        let mut sm = make_sm(2000);

        let out = sm.process(&ctrl_t());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::DispatchAction(ActionReference::Simple(name)) => {
                assert_eq!(name, "new-tab");
            }
            other => panic!("expected DispatchAction, got {:?}", other),
        }
    }

    #[test]
    fn idle_unbound_key_sends_raw() {
        let mut sm = make_sm(2000);

        let out = sm.process(&letter_x());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::SendToSession(bytes) => {
                assert_eq!(bytes, vec![b'x']);
            }
            other => panic!("expected SendToSession, got {:?}", other),
        }
    }

    // ── Timeout tests ───────────────────────────────────────────────────────

    #[test]
    fn check_timeout_returns_false_when_idle() {
        let mut sm = make_sm(2000);
        assert!(!sm.check_timeout());
    }

    #[test]
    fn check_timeout_returns_false_before_expiry() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());
        assert!(sm.is_prefix_active());
        // Immediately after activation — should not have timed out.
        assert!(!sm.check_timeout());
        assert!(sm.is_prefix_active());
    }

    #[test]
    fn check_timeout_returns_true_after_expiry() {
        let mut sm = make_sm(50); // 50ms timeout
        sm.process(&ctrl_b());
        assert!(sm.is_prefix_active());

        std::thread::sleep(Duration::from_millis(80));

        assert!(sm.check_timeout());
        assert!(!sm.is_prefix_active());
    }

    #[test]
    fn timeout_resets_after_chord_dispatch() {
        let mut sm = make_sm(50);
        sm.process(&ctrl_b());
        sm.process(&percent()); // chord dispatched → idle

        std::thread::sleep(Duration::from_millis(80));
        // Already idle, so check_timeout should return false.
        assert!(!sm.check_timeout());
    }

    // ── Label and metadata tests ────────────────────────────────────────────

    #[test]
    fn prefix_label_is_correct() {
        let sm = make_sm(2000);
        assert_eq!(sm.prefix_label(), "Ctrl+B");
    }

    #[test]
    fn timeout_duration_matches_config() {
        let sm = make_sm(1500);
        assert_eq!(sm.timeout(), Duration::from_millis(1500));
    }

    // ── No-prefix configuration ─────────────────────────────────────────────

    #[test]
    fn no_prefix_configured_all_keys_pass_through() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: None,
        };
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        let mut sm = PrefixStateMachine::new(classifier);

        assert!(!sm.is_prefix_active());
        assert_eq!(sm.prefix_label(), "");

        // Ctrl+B is just raw input when no prefix is configured.
        let out = sm.process(&ctrl_b());
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::SendToSession(bytes) => {
                assert_eq!(bytes, vec![0x02]);
            }
            other => panic!("expected SendToSession, got {:?}", other),
        }
    }

    // ── Cycling: activate then re-activate ──────────────────────────────────

    #[test]
    fn can_reactivate_after_escape() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());
        sm.process(&escape()); // cancel
        assert!(!sm.is_prefix_active());

        sm.process(&ctrl_b()); // re-enter
        assert!(sm.is_prefix_active());

        let out = sm.process(&percent());
        assert!(!sm.is_prefix_active());
        assert!(matches!(out, PrefixOutput::DispatchAction(_)));
    }

    #[test]
    fn can_reactivate_after_double_prefix() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());
        sm.process(&ctrl_b()); // double prefix → literal
        assert!(!sm.is_prefix_active());

        sm.process(&ctrl_b()); // re-enter
        assert!(sm.is_prefix_active());
    }

    #[test]
    fn can_reactivate_after_unbound() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());
        sm.process(&letter_x()); // unbound → prefix + x
        assert!(!sm.is_prefix_active());

        sm.process(&ctrl_b());
        assert!(sm.is_prefix_active());
    }

    // ── Escape with modifiers does NOT cancel ───────────────────────────────

    #[test]
    fn ctrl_escape_is_unbound_not_cancel() {
        let mut sm = make_sm(2000);
        sm.process(&ctrl_b());

        let ctrl_esc = KeyEvent {
            key: KeyName::Escape,
            modifiers: Modifiers::CTRL,
            character: None,
        };
        let out = sm.process(&ctrl_esc);
        // Should be treated as unbound key, not escape cancel.
        assert!(!sm.is_prefix_active());
        match out {
            PrefixOutput::SendToSession(bytes) => {
                // prefix (0x02) + ESC (0x1B)
                assert_eq!(bytes[0], 0x02);
                assert_eq!(bytes[1], 0x1B);
            }
            other => panic!("expected SendToSession, got {:?}", other),
        }
    }
}
