//! Regression coverage for international keyboard text handling.

use wtd_ui::input::text_char_to_key_event;

#[test]
fn german_altgr_style_characters_stay_on_text_lane() {
    for ch in ['@', '{', '}', '|', '~'] {
        assert!(
            text_char_to_key_event(ch).is_none(),
            "{ch:?} should stay on the text lane rather than synthesize a key event"
        );
    }
}

#[test]
fn dead_key_composed_characters_stay_on_text_lane() {
    for ch in ['é', 'ü', 'ñ'] {
        assert!(
            text_char_to_key_event(ch).is_none(),
            "{ch:?} should remain committed text rather than a synthesized key event"
        );
    }
}

#[test]
fn punctuation_used_by_prefix_bindings_can_still_form_key_events() {
    for ch in ['%', '"', '[', ']', '\\', ';', '\'', '`'] {
        let event = text_char_to_key_event(ch)
            .unwrap_or_else(|| panic!("expected key event mapping for {ch:?}"));
        assert_eq!(event.character, Some(ch));
    }
}
