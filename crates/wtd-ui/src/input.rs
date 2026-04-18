//! Keyboard input classification for the terminal UI (§21.1, §21.4).
//!
//! Classifies each keyboard event into one of four categories:
//! 1. **Prefix key match** — enters prefix-active state
//! 2. **Chord key match** — dispatches chord action (when prefix active)
//! 3. **Single-stroke binding** — dispatches bound action
//! 4. **Raw terminal input** — forwarded as bytes to the focused session

use std::fmt;

use wtd_core::workspace::{ActionReference, BindingsDefinition};

// ── Modifiers ────────────────────────────────────────────────────────────────

/// Modifier key flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    pub const CTRL: Modifiers = Modifiers(0x01);
    pub const ALT: Modifiers = Modifiers(0x02);
    pub const SHIFT: Modifiers = Modifiers(0x04);

    pub fn ctrl(self) -> bool {
        self.0 & Self::CTRL.0 != 0
    }
    pub fn alt(self) -> bool {
        self.0 & Self::ALT.0 != 0
    }
    pub fn shift(self) -> bool {
        self.0 & Self::SHIFT.0 != 0
    }
}

impl std::ops::BitOr for Modifiers {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Modifiers(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

// ── KeyName ──────────────────────────────────────────────────────────────────

/// Recognized key names per §21.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyName {
    // Letters
    Char(char), // 'A'..'Z' (uppercase-normalized)
    // Digits
    Digit(u8), // 0..9
    // Function keys
    F(u8), // 1..12
    // Navigation / editing
    Enter,
    Tab,
    Escape,
    Space,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    // Arrow keys
    Up,
    Down,
    Left,
    Right,
    // Named punctuation (§21.2)
    Plus,
    Minus,
    Percent,
    DoubleQuote,
    Comma,
    Period,
    Slash,
    Backslash,
    LeftBracket,
    RightBracket,
    Semicolon,
    Apostrophe,
    Backtick,
}

impl KeyName {
    /// Parse a key name string from a KeySpec or chord key.
    pub fn parse(s: &str) -> Result<KeyName, KeySpecError> {
        // Single character
        if s.len() == 1 {
            let ch = s.chars().next().unwrap();
            return match ch {
                'A'..='Z' | 'a'..='z' => Ok(KeyName::Char(ch.to_ascii_uppercase())),
                '0'..='9' => Ok(KeyName::Digit(ch as u8 - b'0')),
                '%' => Ok(KeyName::Percent),
                '"' => Ok(KeyName::DoubleQuote),
                ',' => Ok(KeyName::Comma),
                '.' => Ok(KeyName::Period),
                '/' => Ok(KeyName::Slash),
                '\\' => Ok(KeyName::Backslash),
                '[' => Ok(KeyName::LeftBracket),
                ']' => Ok(KeyName::RightBracket),
                ';' => Ok(KeyName::Semicolon),
                '\'' => Ok(KeyName::Apostrophe),
                '`' => Ok(KeyName::Backtick),
                '+' => Ok(KeyName::Plus),
                '-' => Ok(KeyName::Minus),
                _ => Err(KeySpecError::InvalidKeyName(s.to_string())),
            };
        }

        // Named keys (case-insensitive match)
        match s.to_ascii_lowercase().as_str() {
            "enter" => Ok(KeyName::Enter),
            "tab" => Ok(KeyName::Tab),
            "escape" => Ok(KeyName::Escape),
            "space" => Ok(KeyName::Space),
            "backspace" => Ok(KeyName::Backspace),
            "delete" => Ok(KeyName::Delete),
            "insert" => Ok(KeyName::Insert),
            "home" => Ok(KeyName::Home),
            "end" => Ok(KeyName::End),
            "pageup" => Ok(KeyName::PageUp),
            "pagedown" => Ok(KeyName::PageDown),
            "up" => Ok(KeyName::Up),
            "down" => Ok(KeyName::Down),
            "left" => Ok(KeyName::Left),
            "right" => Ok(KeyName::Right),
            "plus" => Ok(KeyName::Plus),
            "minus" => Ok(KeyName::Minus),
            _ => {
                // F-keys: F1..F12
                if s.len() >= 2
                    && s.as_bytes()[0].to_ascii_uppercase() == b'F'
                    && s[1..].chars().all(|c| c.is_ascii_digit())
                {
                    if let Ok(n) = s[1..].parse::<u8>() {
                        if (1..=12).contains(&n) {
                            return Ok(KeyName::F(n));
                        }
                    }
                }
                Err(KeySpecError::InvalidKeyName(s.to_string()))
            }
        }
    }
}

impl fmt::Display for KeyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyName::Char(c) => write!(f, "{c}"),
            KeyName::Digit(d) => write!(f, "{d}"),
            KeyName::F(n) => write!(f, "F{n}"),
            KeyName::Enter => write!(f, "Enter"),
            KeyName::Tab => write!(f, "Tab"),
            KeyName::Escape => write!(f, "Escape"),
            KeyName::Space => write!(f, "Space"),
            KeyName::Backspace => write!(f, "Backspace"),
            KeyName::Delete => write!(f, "Delete"),
            KeyName::Insert => write!(f, "Insert"),
            KeyName::Home => write!(f, "Home"),
            KeyName::End => write!(f, "End"),
            KeyName::PageUp => write!(f, "PageUp"),
            KeyName::PageDown => write!(f, "PageDown"),
            KeyName::Up => write!(f, "Up"),
            KeyName::Down => write!(f, "Down"),
            KeyName::Left => write!(f, "Left"),
            KeyName::Right => write!(f, "Right"),
            KeyName::Plus => write!(f, "Plus"),
            KeyName::Minus => write!(f, "Minus"),
            KeyName::Percent => write!(f, "%"),
            KeyName::DoubleQuote => write!(f, "\""),
            KeyName::Comma => write!(f, ","),
            KeyName::Period => write!(f, "."),
            KeyName::Slash => write!(f, "/"),
            KeyName::Backslash => write!(f, "\\"),
            KeyName::LeftBracket => write!(f, "["),
            KeyName::RightBracket => write!(f, "]"),
            KeyName::Semicolon => write!(f, ";"),
            KeyName::Apostrophe => write!(f, "'"),
            KeyName::Backtick => write!(f, "`"),
        }
    }
}

// ── KeySpec ──────────────────────────────────────────────────────────────────

/// A parsed key specification (e.g. `Ctrl+Shift+T`, `F11`, `Alt+Shift+Minus`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub modifiers: Modifiers,
    pub key: KeyName,
}

impl KeySpec {
    /// Parse a key spec string per §21.2.
    ///
    /// Format: `[Modifier+[Modifier+...]]KeyName`
    /// Modifiers: `Ctrl`, `Alt`, `Shift` (order-insensitive, combinable).
    pub fn parse(s: &str) -> Result<KeySpec, KeySpecError> {
        if s.is_empty() {
            return Err(KeySpecError::Empty);
        }

        let parts: Vec<&str> = s.split('+').collect();
        let mut modifiers = Modifiers::NONE;

        // All parts except the last are modifiers; the last is the key name.
        // But we need to handle "Plus" and "Minus" which contain no '+' ambiguity
        // since they appear as the final segment.
        let mut key_part_idx = None;
        for (i, part) in parts.iter().enumerate() {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" => modifiers |= Modifiers::CTRL,
                "alt" => modifiers |= Modifiers::ALT,
                "shift" => modifiers |= Modifiers::SHIFT,
                _ => {
                    // This must be the key name (last segment).
                    if i != parts.len() - 1 {
                        return Err(KeySpecError::InvalidModifier(part.to_string()));
                    }
                    key_part_idx = Some(i);
                }
            }
        }

        let key_str = match key_part_idx {
            Some(i) => parts[i],
            None => {
                // All parts were modifiers — no key name.
                // Could happen with something like "Ctrl+" which splits to ["Ctrl", ""]
                return Err(KeySpecError::MissingKeyName);
            }
        };

        let key = KeyName::parse(key_str)?;
        Ok(KeySpec { modifiers, key })
    }

    /// Check if this key spec matches a given key event.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        self.modifiers == event.modifiers && self.key == event.key
    }
}

impl fmt::Display for KeySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.modifiers.ctrl() {
            write!(f, "Ctrl+")?;
        }
        if self.modifiers.alt() {
            write!(f, "Alt+")?;
        }
        if self.modifiers.shift() {
            write!(f, "Shift+")?;
        }
        write!(f, "{}", self.key)
    }
}

// ── KeyEvent ─────────────────────────────────────────────────────────────────

/// A keyboard event to be classified.
///
/// Constructed from Win32 `WM_KEYDOWN` / `WM_SYSKEYDOWN` messages in the
/// window module.
#[derive(Debug, Clone)]
pub struct KeyEvent {
    /// The key that was pressed (normalized).
    pub key: KeyName,
    /// Active modifier keys.
    pub modifiers: Modifiers,
    /// The character this keystroke would produce (if printable).
    /// Used for chord character matching and raw input forwarding.
    pub character: Option<char>,
}

// ── ChordKey ─────────────────────────────────────────────────────────────────

/// A parsed chord key — either a literal character or a named key.
///
/// Chord keys in the config are things like `%`, `o`, `Up`. Single printable
/// characters match on the produced character; named keys match on the key name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum ChordKey {
    /// Match on the produced character (e.g. `%`, `o`, `"`).
    Character(char),
    /// Match on the key name (e.g. `Up`, `Down`, `Left`, `Right`).
    Named(KeyName),
}

impl ChordKey {
    /// Parse a chord key string from the config.
    fn parse(s: &str) -> Result<ChordKey, KeySpecError> {
        // Single character → character match
        if s.len() == 1 {
            let ch = s.chars().next().unwrap();
            return Ok(ChordKey::Character(ch));
        }
        // Multi-character → named key
        let key = KeyName::parse(s)?;
        Ok(ChordKey::Named(key))
    }

    /// Check if this chord key matches a key event.
    fn matches(&self, event: &KeyEvent) -> bool {
        match self {
            ChordKey::Character(ch) => event.character == Some(*ch),
            ChordKey::Named(key) => event.key == *key,
        }
    }
}

// ── InputAction ──────────────────────────────────────────────────────────────

/// Result of classifying a keyboard event.
#[derive(Debug, Clone)]
pub enum InputAction {
    /// The prefix key was pressed (enter prefix-active state).
    PrefixKey,
    /// A chord binding matched (prefix was active).
    ChordBinding(ActionReference),
    /// A single-stroke binding matched.
    SingleStrokeBinding(ActionReference),
    /// Raw terminal input — forward these bytes to the focused session.
    RawInput(Vec<u8>),
}

// ── KeySpecError ─────────────────────────────────────────────────────────────

/// Errors from parsing key specifications or building the classifier.
#[derive(Debug, Clone, thiserror::Error)]
pub enum KeySpecError {
    #[error("empty key spec")]
    Empty,
    #[error("invalid modifier: {0}")]
    InvalidModifier(String),
    #[error("missing key name")]
    MissingKeyName,
    #[error("invalid key name: {0}")]
    InvalidKeyName(String),
}

// ── InputClassifier ──────────────────────────────────────────────────────────

/// Classifies keyboard events into actions per §21.1 and §21.4.
///
/// Built from a merged `BindingsDefinition` (global + workspace).
/// The caller tracks prefix state and passes `prefix_active` to `classify()`.
#[derive(Debug)]
pub struct InputClassifier {
    /// The prefix key spec (e.g. Ctrl+B).
    prefix: Option<KeySpec>,
    /// Prefix timeout in milliseconds.
    prefix_timeout_ms: u32,
    /// Chord bindings: chord key → action.
    chords: Vec<(ChordKey, ActionReference)>,
    /// Single-stroke bindings: key spec → action.
    single_strokes: Vec<(KeySpec, ActionReference)>,
}

impl InputClassifier {
    /// Build a classifier from a merged bindings definition.
    ///
    /// Any `preset` field in `bindings` is expanded first via
    /// [`wtd_core::effective_bindings`], so callers can pass a
    /// `BindingsDefinition` with only a `preset` set and get the full
    /// preset's keys, chords, and prefix.
    pub fn from_bindings(bindings: &BindingsDefinition) -> Result<Self, KeySpecError> {
        let expanded = wtd_core::effective_bindings(bindings);
        let bindings = &expanded;

        let prefix = match &bindings.prefix {
            Some(s) => Some(KeySpec::parse(s)?),
            None => None,
        };

        let prefix_timeout_ms = bindings.prefix_timeout.unwrap_or(2000);

        let mut chords = Vec::new();
        if let Some(chord_map) = &bindings.chords {
            for (key_str, action) in chord_map {
                let chord_key = ChordKey::parse(key_str)?;
                chords.push((chord_key, action.clone()));
            }
        }

        let mut single_strokes = Vec::new();
        if let Some(key_map) = &bindings.keys {
            for (spec_str, action) in key_map {
                let spec = KeySpec::parse(spec_str)?;
                single_strokes.push((spec, action.clone()));
            }
        }

        Ok(InputClassifier {
            prefix,
            prefix_timeout_ms,
            chords,
            single_strokes,
        })
    }

    /// The configured prefix timeout in milliseconds.
    pub fn prefix_timeout_ms(&self) -> u32 {
        self.prefix_timeout_ms
    }

    /// The parsed prefix key spec, if any.
    pub fn prefix_key(&self) -> Option<&KeySpec> {
        self.prefix.as_ref()
    }

    /// Classify a keyboard event per §21.1.
    ///
    /// `prefix_active`: whether the prefix key was previously pressed and we
    /// are waiting for a chord key. The caller manages prefix state (§21.3).
    ///
    /// **Precedence (§21.4):**
    /// 1. Prefix key > single-stroke binding for the same key
    /// 2. Chord bindings > single-stroke bindings when prefix is active
    /// 3. Single-stroke bindings apply when prefix is NOT active
    /// 4. Unbound keys → raw terminal input
    pub fn classify(&self, event: &KeyEvent, prefix_active: bool) -> InputAction {
        // §21.4: "If a key is configured as both a single-stroke binding and
        // the prefix key, the prefix key wins."
        if !prefix_active {
            if let Some(prefix) = &self.prefix {
                if prefix.matches(event) {
                    return InputAction::PrefixKey;
                }
            }
        }

        // §21.1 step 2: chord key match when prefix is active
        if prefix_active {
            for (chord_key, action) in &self.chords {
                if chord_key.matches(event) {
                    return InputAction::ChordBinding(action.clone());
                }
            }
        }

        // §21.1 step 3: single-stroke binding when prefix is NOT active
        if !prefix_active {
            for (spec, action) in &self.single_strokes {
                if spec.matches(event) {
                    return InputAction::SingleStrokeBinding(action.clone());
                }
            }
        }

        // §21.1 step 4: raw terminal input
        InputAction::RawInput(key_event_to_bytes(event))
    }

    /// Look up a chord binding for the given event (without full classification).
    pub fn find_chord(&self, event: &KeyEvent) -> Option<&ActionReference> {
        self.chords
            .iter()
            .find(|(ck, _)| ck.matches(event))
            .map(|(_, a)| a)
    }

    /// Look up a single-stroke binding for the given event.
    pub fn find_single_stroke(&self, event: &KeyEvent) -> Option<&ActionReference> {
        self.single_strokes
            .iter()
            .find(|(spec, _)| spec.matches(event))
            .map(|(_, a)| a)
    }
}

// ── Raw terminal byte conversion ─────────────────────────────────────────────

/// Convert a key event to the bytes a terminal session expects.
///
/// Handles:
/// - Printable characters → UTF-8
/// - Ctrl+letter → control codes (0x01..0x1A)
/// - Arrow keys → VT escape sequences (CSI A/B/C/D)
/// - Function keys → VT escape sequences
/// - Special keys (Enter, Tab, Escape, Backspace, etc.)
/// - Alt+key → ESC + key bytes (meta prefix)
pub fn key_event_to_bytes(event: &KeyEvent) -> Vec<u8> {
    let mods = event.modifiers;

    // Ctrl+letter → control codes
    if mods.ctrl() && !mods.alt() && !mods.shift() {
        if let KeyName::Char(c) = event.key {
            let code = c as u8 - b'A' + 1; // Ctrl+A = 0x01, etc.
            return vec![code];
        }
    }

    // Ctrl+Shift+letter → still control codes (Shift doesn't change the code)
    if mods.ctrl() && !mods.alt() && mods.shift() {
        if let KeyName::Char(c) = event.key {
            let code = c as u8 - b'A' + 1;
            return vec![code];
        }
    }

    // Special keys with optional modifier encoding
    let special_bytes = special_key_bytes(&event.key, mods);
    if let Some(bytes) = special_bytes {
        if mods.alt() && !matches!(event.key, KeyName::Escape | KeyName::Enter) {
            // Alt prefix: ESC + the sequence
            let mut result = vec![0x1B];
            result.extend_from_slice(&bytes);
            return result;
        }
        return bytes;
    }

    // Printable character from the event
    if let Some(ch) = event.character {
        if mods.alt() {
            // Alt+char → ESC + char
            let mut result = vec![0x1B];
            let mut buf = [0u8; 4];
            let s = ch.encode_utf8(&mut buf);
            result.extend_from_slice(s.as_bytes());
            return result;
        }
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        return s.as_bytes().to_vec();
    }

    // Fallback: no bytes for this event
    Vec::new()
}

/// Generate VT bytes for special (non-printable) keys.
fn special_key_bytes(key: &KeyName, mods: Modifiers) -> Option<Vec<u8>> {
    // Modifier parameter for xterm-style sequences: CSI 1;{mod} X
    // mod = 1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)
    let mod_param = 1
        + if mods.shift() { 1 } else { 0 }
        + if mods.alt() { 2 } else { 0 }
        + if mods.ctrl() { 4 } else { 0 };
    let has_mods = mod_param > 1;

    match key {
        KeyName::Enter => {
            if has_mods {
                Some(csi_u(13, mod_param))
            } else {
                Some(vec![0x0D])
            }
        }
        KeyName::Tab => {
            if mods.shift() {
                Some(vec![0x1B, b'[', b'Z']) // Shift+Tab → CSI Z (backtab)
            } else {
                Some(vec![0x09])
            }
        }
        KeyName::Escape => Some(vec![0x1B]),
        KeyName::Space => {
            if mods.ctrl() {
                Some(vec![0x00]) // Ctrl+Space → NUL
            } else {
                Some(vec![b' '])
            }
        }
        KeyName::Backspace => {
            if mods.ctrl() {
                Some(vec![0x08]) // Ctrl+Backspace → BS
            } else {
                Some(vec![0x7F]) // Backspace → DEL
            }
        }

        // Arrow keys: CSI A/B/C/D (with modifier: CSI 1;mod A/B/C/D)
        KeyName::Up => Some(csi_key(b'A', mod_param, has_mods)),
        KeyName::Down => Some(csi_key(b'B', mod_param, has_mods)),
        KeyName::Right => Some(csi_key(b'C', mod_param, has_mods)),
        KeyName::Left => Some(csi_key(b'D', mod_param, has_mods)),

        // Navigation keys: CSI n~ (with modifier: CSI n;mod ~)
        KeyName::Insert => Some(csi_tilde(2, mod_param, has_mods)),
        KeyName::Delete => Some(csi_tilde(3, mod_param, has_mods)),
        KeyName::PageUp => Some(csi_tilde(5, mod_param, has_mods)),
        KeyName::PageDown => Some(csi_tilde(6, mod_param, has_mods)),

        // Home/End: CSI H/F (with modifier: CSI 1;mod H/F)
        KeyName::Home => Some(csi_key(b'H', mod_param, has_mods)),
        KeyName::End => Some(csi_key(b'F', mod_param, has_mods)),

        // Function keys
        KeyName::F(1) => Some(ss3_or_csi(b'P', mod_param, has_mods)),
        KeyName::F(2) => Some(ss3_or_csi(b'Q', mod_param, has_mods)),
        KeyName::F(3) => Some(ss3_or_csi(b'R', mod_param, has_mods)),
        KeyName::F(4) => Some(ss3_or_csi(b'S', mod_param, has_mods)),
        KeyName::F(5) => Some(csi_tilde(15, mod_param, has_mods)),
        KeyName::F(6) => Some(csi_tilde(17, mod_param, has_mods)),
        KeyName::F(7) => Some(csi_tilde(18, mod_param, has_mods)),
        KeyName::F(8) => Some(csi_tilde(19, mod_param, has_mods)),
        KeyName::F(9) => Some(csi_tilde(20, mod_param, has_mods)),
        KeyName::F(10) => Some(csi_tilde(21, mod_param, has_mods)),
        KeyName::F(11) => Some(csi_tilde(23, mod_param, has_mods)),
        KeyName::F(12) => Some(csi_tilde(24, mod_param, has_mods)),

        _ => None,
    }
}

/// CSI {final} or CSI 1;{mod} {final}
fn csi_key(final_byte: u8, mod_param: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1B[1;{mod_param}{}", final_byte as char).into_bytes()
    } else {
        vec![0x1B, b'[', final_byte]
    }
}

/// CSI {num}~ or CSI {num};{mod}~
fn csi_tilde(num: u8, mod_param: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1B[{num};{mod_param}~").into_bytes()
    } else {
        format!("\x1B[{num}~").into_bytes()
    }
}

/// SS3 {ch} (no modifiers) or CSI 1;{mod} {ch} (with modifiers)
fn ss3_or_csi(ch: u8, mod_param: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1B[1;{mod_param}{}", ch as char).into_bytes()
    } else {
        vec![0x1B, b'O', ch]
    }
}

/// CSI {codepoint};{mod}u
fn csi_u(codepoint: u32, mod_param: u8) -> Vec<u8> {
    format!("\x1B[{codepoint};{mod_param}u").into_bytes()
}

// ── Win32 VK code mapping ────────────────────────────────────────────────────

/// Map a Win32 virtual key code to a `KeyName`.
///
/// Returns `None` for keys we don't handle (modifier-only keys, etc.).
#[cfg(windows)]
pub fn vk_to_key_name(vk: u16) -> Option<KeyName> {
    // VK constants from windows-rs
    match vk {
        // Letters A-Z (VK_A = 0x41 .. VK_Z = 0x5A)
        0x41..=0x5A => Some(KeyName::Char((vk as u8) as char)),
        // Digits 0-9 (VK_0 = 0x30 .. VK_9 = 0x39)
        0x30..=0x39 => Some(KeyName::Digit((vk - 0x30) as u8)),
        // Function keys (VK_F1 = 0x70 .. VK_F12 = 0x7B)
        0x70..=0x7B => Some(KeyName::F((vk - 0x70 + 1) as u8)),
        // Special keys
        0x0D => Some(KeyName::Enter),     // VK_RETURN
        0x09 => Some(KeyName::Tab),       // VK_TAB
        0x1B => Some(KeyName::Escape),    // VK_ESCAPE
        0x20 => Some(KeyName::Space),     // VK_SPACE
        0x08 => Some(KeyName::Backspace), // VK_BACK
        0x2E => Some(KeyName::Delete),    // VK_DELETE
        0x2D => Some(KeyName::Insert),    // VK_INSERT
        0x24 => Some(KeyName::Home),      // VK_HOME
        0x23 => Some(KeyName::End),       // VK_END
        0x21 => Some(KeyName::PageUp),    // VK_PRIOR
        0x22 => Some(KeyName::PageDown),  // VK_NEXT
        // Arrow keys
        0x26 => Some(KeyName::Up),    // VK_UP
        0x28 => Some(KeyName::Down),  // VK_DOWN
        0x25 => Some(KeyName::Left),  // VK_LEFT
        0x27 => Some(KeyName::Right), // VK_RIGHT
        // OEM keys (US layout mapping)
        0xBB => Some(KeyName::Plus),         // VK_OEM_PLUS (= / +)
        0xBD => Some(KeyName::Minus),        // VK_OEM_MINUS (- / _)
        0xBC => Some(KeyName::Comma),        // VK_OEM_COMMA (, / <)
        0xBE => Some(KeyName::Period),       // VK_OEM_PERIOD (. / >)
        0xBF => Some(KeyName::Slash),        // VK_OEM_2 (/ / ?)
        0xDC => Some(KeyName::Backslash),    // VK_OEM_5 (\ / |)
        0xDB => Some(KeyName::LeftBracket),  // VK_OEM_4 ([ / {)
        0xDD => Some(KeyName::RightBracket), // VK_OEM_6 (] / })
        0xBA => Some(KeyName::Semicolon),    // VK_OEM_1 (; / :)
        0xDE => Some(KeyName::Apostrophe),   // VK_OEM_7 (' / ")
        0xC0 => Some(KeyName::Backtick),     // VK_OEM_3 (` / ~)
        _ => None,
    }
}

/// Read current modifier state from Win32 `GetKeyState`.
#[cfg(windows)]
pub fn current_modifiers() -> Modifiers {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;

    let mut mods = Modifiers::NONE;
    unsafe {
        if GetKeyState(0x11) < 0 {
            // VK_CONTROL
            mods |= Modifiers::CTRL;
        }
        if GetKeyState(0x12) < 0 {
            // VK_MENU (Alt)
            mods |= Modifiers::ALT;
        }
        if GetKeyState(0x10) < 0 {
            // VK_SHIFT
            mods |= Modifiers::SHIFT;
        }
    }
    mods
}

/// Get the character a key press would produce using Win32 `ToUnicode`.
#[cfg(windows)]
pub fn vk_to_char(vk: u16, scan_code: u16) -> Option<char> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, ToUnicode};

    unsafe {
        let mut keyboard_state = [0u8; 256];
        let _ = GetKeyboardState(&mut keyboard_state);
        let mut buf = [0u16; 4];
        let result = ToUnicode(
            vk as u32,
            scan_code as u32,
            Some(&keyboard_state),
            &mut buf,
            0, // flags
        );
        if result == 1 {
            char::from_u32(buf[0] as u32)
        } else {
            None
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // Helper to create a key event
    fn key(name: KeyName, mods: Modifiers, ch: Option<char>) -> KeyEvent {
        KeyEvent {
            key: name,
            modifiers: mods,
            character: ch,
        }
    }

    // ── KeySpec parsing ──────────────────────────────────────────────────

    #[test]
    fn parse_simple_key() {
        let spec = KeySpec::parse("B").unwrap();
        assert_eq!(spec.modifiers, Modifiers::NONE);
        assert_eq!(spec.key, KeyName::Char('B'));
    }

    #[test]
    fn parse_ctrl_key() {
        let spec = KeySpec::parse("Ctrl+B").unwrap();
        assert_eq!(spec.modifiers, Modifiers::CTRL);
        assert_eq!(spec.key, KeyName::Char('B'));
    }

    #[test]
    fn parse_ctrl_shift_key() {
        let spec = KeySpec::parse("Ctrl+Shift+T").unwrap();
        assert_eq!(spec.modifiers, Modifiers::CTRL | Modifiers::SHIFT);
        assert_eq!(spec.key, KeyName::Char('T'));
    }

    #[test]
    fn parse_alt_shift_key() {
        let spec = KeySpec::parse("Alt+Shift+D").unwrap();
        assert_eq!(spec.modifiers, Modifiers::ALT | Modifiers::SHIFT);
        assert_eq!(spec.key, KeyName::Char('D'));
    }

    #[test]
    fn parse_function_key() {
        let spec = KeySpec::parse("F11").unwrap();
        assert_eq!(spec.modifiers, Modifiers::NONE);
        assert_eq!(spec.key, KeyName::F(11));
    }

    #[test]
    fn parse_ctrl_tab() {
        let spec = KeySpec::parse("Ctrl+Tab").unwrap();
        assert_eq!(spec.modifiers, Modifiers::CTRL);
        assert_eq!(spec.key, KeyName::Tab);
    }

    #[test]
    fn parse_alt_shift_minus() {
        let spec = KeySpec::parse("Alt+Shift+Minus").unwrap();
        assert_eq!(spec.modifiers, Modifiers::ALT | Modifiers::SHIFT);
        assert_eq!(spec.key, KeyName::Minus);
    }

    #[test]
    fn parse_case_insensitive_key_name() {
        let spec = KeySpec::parse("Ctrl+enter").unwrap();
        assert_eq!(spec.key, KeyName::Enter);
    }

    #[test]
    fn parse_case_insensitive_letter() {
        let s1 = KeySpec::parse("Ctrl+b").unwrap();
        let s2 = KeySpec::parse("Ctrl+B").unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn parse_error_empty() {
        assert!(KeySpec::parse("").is_err());
    }

    #[test]
    fn parse_error_invalid_modifier() {
        assert!(KeySpec::parse("Super+A").is_err());
    }

    #[test]
    fn parse_error_invalid_key_name() {
        assert!(KeySpec::parse("Ctrl+FooBar").is_err());
    }

    #[test]
    fn parse_display_round_trip() {
        let specs = [
            "Ctrl+B",
            "Ctrl+Shift+T",
            "Alt+Shift+D",
            "F11",
            "Alt+Shift+Minus",
            "Ctrl+Tab",
        ];
        for s in specs {
            let parsed = KeySpec::parse(s).unwrap();
            let displayed = parsed.to_string();
            let reparsed = KeySpec::parse(&displayed).unwrap();
            assert_eq!(parsed, reparsed, "round-trip failed for {s}");
        }
    }

    // ── KeySpec matching ─────────────────────────────────────────────────

    #[test]
    fn keyspec_matches_event() {
        let spec = KeySpec::parse("Ctrl+B").unwrap();
        let event = key(KeyName::Char('B'), Modifiers::CTRL, None);
        assert!(spec.matches(&event));
    }

    #[test]
    fn keyspec_no_match_different_modifier() {
        let spec = KeySpec::parse("Ctrl+B").unwrap();
        let event = key(KeyName::Char('B'), Modifiers::ALT, None);
        assert!(!spec.matches(&event));
    }

    #[test]
    fn keyspec_no_match_different_key() {
        let spec = KeySpec::parse("Ctrl+B").unwrap();
        let event = key(KeyName::Char('A'), Modifiers::CTRL, None);
        assert!(!spec.matches(&event));
    }

    // ── ChordKey matching ────────────────────────────────────────────────

    #[test]
    fn chord_character_match() {
        let ck = ChordKey::parse("%").unwrap();
        assert_eq!(ck, ChordKey::Character('%'));

        let event = key(KeyName::Digit(5), Modifiers::SHIFT, Some('%'));
        assert!(ck.matches(&event));
    }

    #[test]
    fn chord_character_no_match() {
        let ck = ChordKey::Character('o');
        let event = key(KeyName::Char('O'), Modifiers::NONE, Some('p'));
        assert!(!ck.matches(&event));
    }

    #[test]
    fn chord_named_key_match() {
        let ck = ChordKey::parse("Up").unwrap();
        assert_eq!(ck, ChordKey::Named(KeyName::Up));

        let event = key(KeyName::Up, Modifiers::NONE, None);
        assert!(ck.matches(&event));
    }

    #[test]
    fn chord_letter_match_via_character() {
        let ck = ChordKey::parse("o").unwrap();
        assert_eq!(ck, ChordKey::Character('o'));

        // pressing 'O' key with no shift produces lowercase 'o'
        let event = key(KeyName::Char('O'), Modifiers::NONE, Some('o'));
        assert!(ck.matches(&event));
    }

    // ── InputClassifier ──────────────────────────────────────────────────

    fn test_bindings() -> BindingsDefinition {
        let mut keys = HashMap::new();
        keys.insert(
            "Ctrl+Shift+T".to_string(),
            ActionReference::Simple("new-tab".to_string()),
        );
        keys.insert(
            "F11".to_string(),
            ActionReference::Simple("toggle-fullscreen".to_string()),
        );

        let mut chords = HashMap::new();
        chords.insert(
            "%".to_string(),
            ActionReference::Simple("split-right".to_string()),
        );
        chords.insert(
            "o".to_string(),
            ActionReference::Simple("focus-next-pane".to_string()),
        );
        chords.insert(
            "Up".to_string(),
            ActionReference::Simple("focus-pane-up".to_string()),
        );

        BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(2000),
            chords: Some(chords),
            keys: Some(keys),
        }
    }

    #[test]
    fn classify_prefix_key_when_not_active() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Char('B'), Modifiers::CTRL, None);

        match classifier.classify(&event, false) {
            InputAction::PrefixKey => {} // expected
            other => panic!("expected PrefixKey, got {other:?}"),
        }
    }

    #[test]
    fn classify_prefix_key_when_active_returns_raw() {
        // When prefix IS active, pressing prefix again is raw input
        // (the state machine in w0y.2 handles the "send prefix to session" logic;
        //  from the classifier's perspective, it's not a prefix match)
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Char('B'), Modifiers::CTRL, None);

        match classifier.classify(&event, true) {
            InputAction::RawInput(_) => {} // expected
            other => panic!("expected RawInput, got {other:?}"),
        }
    }

    #[test]
    fn classify_chord_when_prefix_active() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        // '%' chord
        let event = key(KeyName::Digit(5), Modifiers::SHIFT, Some('%'));

        match classifier.classify(&event, true) {
            InputAction::ChordBinding(action) => {
                assert_eq!(action, ActionReference::Simple("split-right".to_string()));
            }
            other => panic!("expected ChordBinding, got {other:?}"),
        }
    }

    #[test]
    fn classify_chord_letter_when_prefix_active() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Char('O'), Modifiers::NONE, Some('o'));

        match classifier.classify(&event, true) {
            InputAction::ChordBinding(action) => {
                assert_eq!(
                    action,
                    ActionReference::Simple("focus-next-pane".to_string())
                );
            }
            other => panic!("expected ChordBinding, got {other:?}"),
        }
    }

    #[test]
    fn classify_chord_named_key_when_prefix_active() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Up, Modifiers::NONE, None);

        match classifier.classify(&event, true) {
            InputAction::ChordBinding(action) => {
                assert_eq!(action, ActionReference::Simple("focus-pane-up".to_string()));
            }
            other => panic!("expected ChordBinding, got {other:?}"),
        }
    }

    #[test]
    fn classify_single_stroke() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Char('T'), Modifiers::CTRL | Modifiers::SHIFT, None);

        match classifier.classify(&event, false) {
            InputAction::SingleStrokeBinding(action) => {
                assert_eq!(action, ActionReference::Simple("new-tab".to_string()));
            }
            other => panic!("expected SingleStrokeBinding, got {other:?}"),
        }
    }

    #[test]
    fn classify_single_stroke_ignored_when_prefix_active() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Char('T'), Modifiers::CTRL | Modifiers::SHIFT, None);

        // Single-stroke bindings are NOT checked when prefix is active
        match classifier.classify(&event, true) {
            InputAction::RawInput(_) => {} // expected
            other => panic!("expected RawInput, got {other:?}"),
        }
    }

    #[test]
    fn classify_function_key_binding() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::F(11), Modifiers::NONE, None);

        match classifier.classify(&event, false) {
            InputAction::SingleStrokeBinding(action) => {
                assert_eq!(
                    action,
                    ActionReference::Simple("toggle-fullscreen".to_string())
                );
            }
            other => panic!("expected SingleStrokeBinding, got {other:?}"),
        }
    }

    #[test]
    fn classify_raw_input_character() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Char('H'), Modifiers::NONE, Some('h'));

        match classifier.classify(&event, false) {
            InputAction::RawInput(bytes) => {
                assert_eq!(bytes, b"h");
            }
            other => panic!("expected RawInput, got {other:?}"),
        }
    }

    #[test]
    fn classify_raw_input_arrow_key() {
        let classifier = InputClassifier::from_bindings(&test_bindings()).unwrap();
        let event = key(KeyName::Up, Modifiers::NONE, None);

        // Up arrow is NOT a chord when prefix is not active → raw
        match classifier.classify(&event, false) {
            InputAction::RawInput(bytes) => {
                assert_eq!(bytes, b"\x1B[A");
            }
            other => panic!("expected RawInput, got {other:?}"),
        }
    }

    // ── Conflict resolution (§21.4) ──────────────────────────────────────

    #[test]
    fn prefix_key_wins_over_single_stroke() {
        // §21.4: "If a key is configured as both a single-stroke binding and
        // the prefix key, the prefix key wins."
        let mut keys = HashMap::new();
        keys.insert(
            "Ctrl+B".to_string(),
            ActionReference::Simple("some-action".to_string()),
        );

        let bindings = BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(2000),
            chords: None,
            keys: Some(keys),
        };

        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        let event = key(KeyName::Char('B'), Modifiers::CTRL, None);

        // Prefix wins
        match classifier.classify(&event, false) {
            InputAction::PrefixKey => {} // expected
            other => panic!("expected PrefixKey, got {other:?}"),
        }
    }

    #[test]
    fn chord_takes_priority_over_single_stroke_when_prefix_active() {
        // §21.4: Chord bindings take priority over single-stroke when prefix active
        let mut keys = HashMap::new();
        keys.insert(
            "F11".to_string(),
            ActionReference::Simple("single-action".to_string()),
        );

        let mut chords = HashMap::new();
        // Hypothetical: F11 is also a chord key
        chords.insert(
            "F11".to_string(),
            ActionReference::Simple("chord-action".to_string()),
        );

        let bindings = BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(2000),
            chords: Some(chords),
            keys: Some(keys),
        };

        let classifier = InputClassifier::from_bindings(&bindings).unwrap();

        // F11 parsed as Named chord key (since it's multi-char)
        // which matches on KeyName::F(11)
        let event = key(KeyName::F(11), Modifiers::NONE, None);

        // When prefix is active → chord wins
        match classifier.classify(&event, true) {
            InputAction::ChordBinding(action) => {
                assert_eq!(action, ActionReference::Simple("chord-action".to_string()));
            }
            other => panic!("expected ChordBinding, got {other:?}"),
        }

        // When prefix is NOT active → single-stroke wins
        match classifier.classify(&event, false) {
            InputAction::SingleStrokeBinding(action) => {
                assert_eq!(action, ActionReference::Simple("single-action".to_string()));
            }
            other => panic!("expected SingleStrokeBinding, got {other:?}"),
        }
    }

    #[test]
    fn unbound_key_passes_through() {
        let bindings = BindingsDefinition {
            preset: None,
            prefix: Some("Ctrl+B".to_string()),
            prefix_timeout: Some(2000),
            chords: Some(HashMap::new()),
            keys: Some(HashMap::new()),
        };

        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        let event = key(KeyName::Char('X'), Modifiers::NONE, Some('x'));

        match classifier.classify(&event, false) {
            InputAction::RawInput(bytes) => {
                assert_eq!(bytes, b"x");
            }
            other => panic!("expected RawInput, got {other:?}"),
        }
    }

    // ── Preset expansion via from_bindings ──────────────────────────────

    #[test]
    fn from_tmux_preset_bindings() {
        // Passing a BindingsDefinition with preset: Tmux expands to tmux bindings.
        let bindings = wtd_core::tmux_bindings();
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();

        assert!(classifier.prefix_key().is_some());
        assert_eq!(classifier.prefix_timeout_ms(), 2000);

        // Ctrl+B → prefix
        let event = key(KeyName::Char('B'), Modifiers::CTRL, None);
        assert!(matches!(
            classifier.classify(&event, false),
            InputAction::PrefixKey
        ));

        // Ctrl+Shift+T → new-tab
        let event = key(KeyName::Char('T'), Modifiers::CTRL | Modifiers::SHIFT, None);
        assert!(matches!(
            classifier.classify(&event, false),
            InputAction::SingleStrokeBinding(_)
        ));
    }

    #[test]
    fn from_windows_terminal_preset_bindings_is_empty() {
        // Default (windows-terminal preset placeholder) has no active bindings.
        let bindings = wtd_core::default_bindings();
        let classifier = InputClassifier::from_bindings(&bindings).unwrap();

        // No prefix configured for the windows-terminal placeholder.
        assert!(
            classifier.prefix_key().is_none(),
            "windows-terminal preset has no prefix yet"
        );
    }

    // ── No prefix configured ────────────────────────────────────────────

    #[test]
    fn no_prefix_configured() {
        let mut keys = HashMap::new();
        keys.insert(
            "Ctrl+B".to_string(),
            ActionReference::Simple("some-action".to_string()),
        );

        let bindings = BindingsDefinition {
            preset: None,
            prefix: None,
            prefix_timeout: None,
            chords: None,
            keys: Some(keys),
        };

        let classifier = InputClassifier::from_bindings(&bindings).unwrap();
        assert!(classifier.prefix_key().is_none());

        // Ctrl+B → single-stroke (no prefix to intercept)
        let event = key(KeyName::Char('B'), Modifiers::CTRL, None);
        match classifier.classify(&event, false) {
            InputAction::SingleStrokeBinding(action) => {
                assert_eq!(action, ActionReference::Simple("some-action".to_string()));
            }
            other => panic!("expected SingleStrokeBinding, got {other:?}"),
        }
    }

    // ── Raw byte conversion ──────────────────────────────────────────────

    #[test]
    fn raw_bytes_printable_character() {
        let event = key(KeyName::Char('A'), Modifiers::NONE, Some('a'));
        assert_eq!(key_event_to_bytes(&event), b"a");
    }

    #[test]
    fn raw_bytes_ctrl_a() {
        let event = key(KeyName::Char('A'), Modifiers::CTRL, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x01]);
    }

    #[test]
    fn raw_bytes_ctrl_c() {
        let event = key(KeyName::Char('C'), Modifiers::CTRL, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x03]);
    }

    #[test]
    fn raw_bytes_enter() {
        let event = key(KeyName::Enter, Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x0D]);
    }

    #[test]
    fn raw_bytes_shift_enter_uses_csi_u() {
        let event = key(KeyName::Enter, Modifiers::SHIFT, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[13;2u");
    }

    #[test]
    fn raw_bytes_alt_enter_uses_csi_u_without_extra_escape_prefix() {
        let event = key(KeyName::Enter, Modifiers::ALT, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[13;3u");
    }

    #[test]
    fn raw_bytes_ctrl_enter_uses_csi_u() {
        let event = key(KeyName::Enter, Modifiers::CTRL, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[13;5u");
    }

    #[test]
    fn raw_bytes_tab() {
        let event = key(KeyName::Tab, Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x09]);
    }

    #[test]
    fn raw_bytes_shift_tab() {
        let event = key(KeyName::Tab, Modifiers::SHIFT, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[Z");
    }

    #[test]
    fn raw_bytes_escape() {
        let event = key(KeyName::Escape, Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x1B]);
    }

    #[test]
    fn raw_bytes_backspace() {
        let event = key(KeyName::Backspace, Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x7F]);
    }

    #[test]
    fn raw_bytes_arrow_keys() {
        assert_eq!(
            key_event_to_bytes(&key(KeyName::Up, Modifiers::NONE, None)),
            b"\x1B[A"
        );
        assert_eq!(
            key_event_to_bytes(&key(KeyName::Down, Modifiers::NONE, None)),
            b"\x1B[B"
        );
        assert_eq!(
            key_event_to_bytes(&key(KeyName::Right, Modifiers::NONE, None)),
            b"\x1B[C"
        );
        assert_eq!(
            key_event_to_bytes(&key(KeyName::Left, Modifiers::NONE, None)),
            b"\x1B[D"
        );
    }

    #[test]
    fn raw_bytes_ctrl_arrow() {
        // Ctrl+Up → CSI 1;5 A
        let event = key(KeyName::Up, Modifiers::CTRL, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[1;5A");
    }

    #[test]
    fn raw_bytes_delete() {
        let event = key(KeyName::Delete, Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[3~");
    }

    #[test]
    fn raw_bytes_f1() {
        let event = key(KeyName::F(1), Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1BOP");
    }

    #[test]
    fn raw_bytes_f5() {
        let event = key(KeyName::F(5), Modifiers::NONE, None);
        assert_eq!(key_event_to_bytes(&event), b"\x1B[15~");
    }

    #[test]
    fn raw_bytes_alt_a() {
        let event = key(KeyName::Char('A'), Modifiers::ALT, Some('a'));
        assert_eq!(key_event_to_bytes(&event), b"\x1Ba");
    }

    #[test]
    fn raw_bytes_ctrl_alt_without_character_produces_no_meta_text() {
        let event = key(KeyName::Char('Q'), Modifiers::CTRL | Modifiers::ALT, None);
        assert!(key_event_to_bytes(&event).is_empty());
    }

    #[test]
    fn raw_bytes_unicode() {
        let event = key(KeyName::Char('A'), Modifiers::NONE, Some('ñ'));
        let bytes = key_event_to_bytes(&event);
        assert_eq!(bytes, "ñ".as_bytes());
    }

    #[test]
    fn raw_bytes_home_end() {
        assert_eq!(
            key_event_to_bytes(&key(KeyName::Home, Modifiers::NONE, None)),
            b"\x1B[H"
        );
        assert_eq!(
            key_event_to_bytes(&key(KeyName::End, Modifiers::NONE, None)),
            b"\x1B[F"
        );
    }

    #[test]
    fn raw_bytes_page_up_down() {
        assert_eq!(
            key_event_to_bytes(&key(KeyName::PageUp, Modifiers::NONE, None)),
            b"\x1B[5~"
        );
        assert_eq!(
            key_event_to_bytes(&key(KeyName::PageDown, Modifiers::NONE, None)),
            b"\x1B[6~"
        );
    }

    #[test]
    fn raw_bytes_space() {
        let event = key(KeyName::Space, Modifiers::NONE, Some(' '));
        assert_eq!(key_event_to_bytes(&event), b" ");
    }

    #[test]
    fn raw_bytes_ctrl_space() {
        let event = key(KeyName::Space, Modifiers::CTRL, None);
        assert_eq!(key_event_to_bytes(&event), vec![0x00]);
    }

    // ── VK code mapping ─────────────────────────────────────────────────

    #[cfg(windows)]
    #[test]
    fn vk_mapping_letters() {
        assert_eq!(vk_to_key_name(0x41), Some(KeyName::Char('A')));
        assert_eq!(vk_to_key_name(0x5A), Some(KeyName::Char('Z')));
    }

    #[cfg(windows)]
    #[test]
    fn vk_mapping_digits() {
        assert_eq!(vk_to_key_name(0x30), Some(KeyName::Digit(0)));
        assert_eq!(vk_to_key_name(0x39), Some(KeyName::Digit(9)));
    }

    #[cfg(windows)]
    #[test]
    fn vk_mapping_function_keys() {
        assert_eq!(vk_to_key_name(0x70), Some(KeyName::F(1)));
        assert_eq!(vk_to_key_name(0x7B), Some(KeyName::F(12)));
    }

    #[cfg(windows)]
    #[test]
    fn vk_mapping_special_keys() {
        assert_eq!(vk_to_key_name(0x0D), Some(KeyName::Enter));
        assert_eq!(vk_to_key_name(0x09), Some(KeyName::Tab));
        assert_eq!(vk_to_key_name(0x1B), Some(KeyName::Escape));
        assert_eq!(vk_to_key_name(0x20), Some(KeyName::Space));
        assert_eq!(vk_to_key_name(0x08), Some(KeyName::Backspace));
    }

    #[cfg(windows)]
    #[test]
    fn vk_mapping_arrows() {
        assert_eq!(vk_to_key_name(0x26), Some(KeyName::Up));
        assert_eq!(vk_to_key_name(0x28), Some(KeyName::Down));
        assert_eq!(vk_to_key_name(0x25), Some(KeyName::Left));
        assert_eq!(vk_to_key_name(0x27), Some(KeyName::Right));
    }

    #[cfg(windows)]
    #[test]
    fn vk_mapping_unknown() {
        // Modifier-only keys return None
        assert_eq!(vk_to_key_name(0x10), None); // VK_SHIFT
        assert_eq!(vk_to_key_name(0x11), None); // VK_CONTROL
        assert_eq!(vk_to_key_name(0x12), None); // VK_MENU
    }
}
