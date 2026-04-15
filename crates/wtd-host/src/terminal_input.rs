#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
struct Modifiers(u8);

impl Modifiers {
    const CTRL: Modifiers = Modifiers(0x01);
    const ALT: Modifiers = Modifiers(0x02);
    const SHIFT: Modifiers = Modifiers(0x04);

    fn ctrl(self) -> bool {
        self.0 & Self::CTRL.0 != 0
    }

    fn alt(self) -> bool {
        self.0 & Self::ALT.0 != 0
    }

    fn shift(self) -> bool {
        self.0 & Self::SHIFT.0 != 0
    }
}

impl std::ops::BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum KeyName {
    Char(char),
    Digit(u8),
    F(u8),
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
    Up,
    Down,
    Left,
    Right,
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
    fn parse(s: &str) -> Result<Self, KeySpecError> {
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

        match s.to_ascii_lowercase().as_str() {
            "enter" => Ok(KeyName::Enter),
            "tab" => Ok(KeyName::Tab),
            "escape" | "esc" => Ok(KeyName::Escape),
            "space" => Ok(KeyName::Space),
            "backspace" => Ok(KeyName::Backspace),
            "delete" | "del" => Ok(KeyName::Delete),
            "insert" | "ins" => Ok(KeyName::Insert),
            "home" => Ok(KeyName::Home),
            "end" => Ok(KeyName::End),
            "pageup" | "pgup" => Ok(KeyName::PageUp),
            "pagedown" | "pgdn" => Ok(KeyName::PageDown),
            "up" => Ok(KeyName::Up),
            "down" => Ok(KeyName::Down),
            "left" => Ok(KeyName::Left),
            "right" => Ok(KeyName::Right),
            "plus" => Ok(KeyName::Plus),
            "minus" => Ok(KeyName::Minus),
            _ => {
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct KeySpec {
    modifiers: Modifiers,
    key: KeyName,
}

impl KeySpec {
    fn parse(s: &str) -> Result<Self, KeySpecError> {
        if s.is_empty() {
            return Err(KeySpecError::Empty);
        }

        let parts: Vec<&str> = s.split('+').collect();
        let mut modifiers = Modifiers::default();
        let mut key_part_idx = None;
        for (i, part) in parts.iter().enumerate() {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" => modifiers |= Modifiers::CTRL,
                "alt" => modifiers |= Modifiers::ALT,
                "shift" => modifiers |= Modifiers::SHIFT,
                _ => {
                    if i != parts.len() - 1 {
                        return Err(KeySpecError::InvalidModifier(part.to_string()));
                    }
                    key_part_idx = Some(i);
                }
            }
        }

        let key_str = match key_part_idx {
            Some(i) => parts[i],
            None => return Err(KeySpecError::MissingKeyName),
        };

        Ok(Self {
            modifiers,
            key: KeyName::parse(key_str)?,
        })
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
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

pub fn encode_key_spec(spec: &str) -> Result<Vec<u8>, KeySpecError> {
    let spec = KeySpec::parse(spec)?;
    Ok(key_spec_to_bytes(&spec))
}

pub fn encode_key_specs(specs: &[String]) -> Result<Vec<u8>, KeySpecError> {
    let mut bytes = Vec::new();
    for spec in specs {
        bytes.extend_from_slice(&encode_key_spec(spec)?);
    }
    Ok(bytes)
}

fn key_spec_to_bytes(spec: &KeySpec) -> Vec<u8> {
    let mods = spec.modifiers;

    if mods.ctrl() {
        if let KeyName::Char(c) = spec.key {
            let code = c as u8 - b'A' + 1;
            if mods.alt() {
                return vec![0x1B, code];
            }
            return vec![code];
        }
    }

    if let Some(bytes) = special_key_bytes(&spec.key, mods) {
        if mods.alt() && !matches!(spec.key, KeyName::Escape | KeyName::Enter) {
            let mut result = vec![0x1B];
            result.extend_from_slice(&bytes);
            return result;
        }
        return bytes;
    }

    let mut bytes = literal_key_bytes(&spec.key, mods);
    if mods.alt() {
        let mut result = vec![0x1B];
        result.append(&mut bytes);
        return result;
    }
    bytes
}

fn literal_key_bytes(key: &KeyName, mods: Modifiers) -> Vec<u8> {
    let ch = match key {
        KeyName::Char(c) => {
            if mods.shift() {
                *c
            } else {
                c.to_ascii_lowercase()
            }
        }
        KeyName::Digit(d) => (b'0' + d) as char,
        KeyName::Plus => '+',
        KeyName::Minus => '-',
        KeyName::Percent => '%',
        KeyName::DoubleQuote => '"',
        KeyName::Comma => ',',
        KeyName::Period => '.',
        KeyName::Slash => '/',
        KeyName::Backslash => '\\',
        KeyName::LeftBracket => '[',
        KeyName::RightBracket => ']',
        KeyName::Semicolon => ';',
        KeyName::Apostrophe => '\'',
        KeyName::Backtick => '`',
        _ => return Vec::new(),
    };

    let mut buf = [0u8; 4];
    ch.encode_utf8(&mut buf).as_bytes().to_vec()
}

fn special_key_bytes(key: &KeyName, mods: Modifiers) -> Option<Vec<u8>> {
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
                Some(vec![0x1B, b'[', b'Z'])
            } else {
                Some(vec![0x09])
            }
        }
        KeyName::Escape => Some(vec![0x1B]),
        KeyName::Space => {
            if mods.ctrl() {
                Some(vec![0x00])
            } else {
                Some(vec![b' '])
            }
        }
        KeyName::Backspace => {
            if mods.ctrl() {
                Some(vec![0x08])
            } else {
                Some(vec![0x7F])
            }
        }
        KeyName::Up => Some(csi_key(b'A', mod_param, has_mods)),
        KeyName::Down => Some(csi_key(b'B', mod_param, has_mods)),
        KeyName::Right => Some(csi_key(b'C', mod_param, has_mods)),
        KeyName::Left => Some(csi_key(b'D', mod_param, has_mods)),
        KeyName::Insert => Some(csi_tilde(2, mod_param, has_mods)),
        KeyName::Delete => Some(csi_tilde(3, mod_param, has_mods)),
        KeyName::PageUp => Some(csi_tilde(5, mod_param, has_mods)),
        KeyName::PageDown => Some(csi_tilde(6, mod_param, has_mods)),
        KeyName::Home => Some(csi_key(b'H', mod_param, has_mods)),
        KeyName::End => Some(csi_key(b'F', mod_param, has_mods)),
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

fn csi_key(final_byte: u8, mod_param: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1B[1;{mod_param}{}", final_byte as char).into_bytes()
    } else {
        vec![0x1B, b'[', final_byte]
    }
}

fn csi_tilde(num: u8, mod_param: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1B[{num};{mod_param}~").into_bytes()
    } else {
        format!("\x1B[{num}~").into_bytes()
    }
}

fn ss3_or_csi(ch: u8, mod_param: u8, has_mods: bool) -> Vec<u8> {
    if has_mods {
        format!("\x1B[1;{mod_param}{}", ch as char).into_bytes()
    } else {
        vec![0x1B, b'O', ch]
    }
}

fn csi_u(codepoint: u32, mod_param: u8) -> Vec<u8> {
    format!("\x1B[{codepoint};{mod_param}u").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_maps_to_cr() {
        assert_eq!(encode_key_spec("Enter").unwrap(), vec![0x0D]);
    }

    #[test]
    fn shift_enter_uses_csi_u() {
        assert_eq!(encode_key_spec("Shift+Enter").unwrap(), b"\x1B[13;2u");
    }

    #[test]
    fn alt_enter_uses_csi_u_without_extra_escape_prefix() {
        assert_eq!(encode_key_spec("Alt+Enter").unwrap(), b"\x1B[13;3u");
    }

    #[test]
    fn ctrl_enter_uses_csi_u() {
        assert_eq!(encode_key_spec("Ctrl+Enter").unwrap(), b"\x1B[13;5u");
    }

    #[test]
    fn ctrl_c_maps_to_etx() {
        assert_eq!(encode_key_spec("Ctrl+C").unwrap(), vec![0x03]);
    }

    #[test]
    fn alt_x_prefixes_escape() {
        assert_eq!(encode_key_spec("Alt+X").unwrap(), vec![0x1B, b'x']);
    }

    #[test]
    fn shifted_arrow_uses_csi_modifier_encoding() {
        assert_eq!(encode_key_spec("Shift+Up").unwrap(), b"\x1B[1;2A");
    }

    #[test]
    fn invalid_modifier_is_rejected() {
        assert_eq!(
            encode_key_spec("Meta+X").unwrap_err(),
            KeySpecError::InvalidModifier("Meta".to_string())
        );
    }
}
