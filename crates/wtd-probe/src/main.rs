use std::env;
use std::io::{self, Read, Write};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct ProbeConfig {
    keyboard_mode: Option<KeyboardMode>,
    bracketed_paste: Option<bool>,
    alt_screen: bool,
    title: Option<String>,
    cursor_hidden: bool,
    cursor_style: Option<u8>,
    mouse_mode: bool,
    hyperlink: Option<(String, String)>,
    request_image_probe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyboardMode {
    CsiU,
    Kitty,
}

fn main() -> io::Result<()> {
    let config = ProbeConfig::parse(env::args().skip(1))?;
    enable_raw_vt_input()?;

    let mut stdout = io::stdout().lock();
    stdout.write_all(&startup_bytes(&config))?;
    stdout.write_all(b"[wtd-probe] ready\r\n")?;
    stdout.flush()?;

    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 1024];
    loop {
        let read = stdin.read(&mut buf)?;
        if read == 0 {
            break;
        }

        let line = format_input_log(&buf[..read]);
        stdout.write_all(line.as_bytes())?;
        stdout.flush()?;
    }

    Ok(())
}

impl ProbeConfig {
    fn parse<I>(args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = String>,
    {
        let mut config = ProbeConfig::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--keyboard-mode" => {
                    let value = args
                        .next()
                        .ok_or_else(|| invalid_input("missing keyboard mode"))?;
                    config.keyboard_mode = Some(match value.as_str() {
                        "csi-u" => KeyboardMode::CsiU,
                        "kitty" => KeyboardMode::Kitty,
                        other => {
                            return Err(invalid_input(&format!("unknown keyboard mode '{other}'")))
                        }
                    });
                }
                "--enable-bracketed-paste" => config.bracketed_paste = Some(true),
                "--disable-bracketed-paste" => config.bracketed_paste = Some(false),
                "--alt-screen" => config.alt_screen = true,
                "--title" => {
                    config.title = Some(args.next().ok_or_else(|| invalid_input("missing title"))?);
                }
                "--cursor-hidden" => config.cursor_hidden = true,
                "--cursor-style" => {
                    let value = args
                        .next()
                        .ok_or_else(|| invalid_input("missing cursor style"))?;
                    let style = value
                        .parse::<u8>()
                        .map_err(|_| invalid_input("cursor style must be an integer"))?;
                    config.cursor_style = Some(style);
                }
                "--mouse-mode" => config.mouse_mode = true,
                "--hyperlink" => {
                    let url = args
                        .next()
                        .ok_or_else(|| invalid_input("missing hyperlink URL"))?;
                    let text = args
                        .next()
                        .ok_or_else(|| invalid_input("missing hyperlink text"))?;
                    config.hyperlink = Some((url, text));
                }
                "--request-image-probe" => config.request_image_probe = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => return Err(invalid_input(&format!("unknown argument '{other}'"))),
            }
        }

        Ok(config)
    }
}

fn startup_bytes(config: &ProbeConfig) -> Vec<u8> {
    let mut out = Vec::new();

    match config.keyboard_mode {
        Some(KeyboardMode::CsiU) => out.extend_from_slice(b"\x1b[>1u"),
        Some(KeyboardMode::Kitty) => out.extend_from_slice(b"\x1b[>31u"),
        None => {}
    }

    match config.bracketed_paste {
        Some(true) => out.extend_from_slice(b"\x1b[?2004h"),
        Some(false) => out.extend_from_slice(b"\x1b[?2004l"),
        None => {}
    }

    if config.alt_screen {
        out.extend_from_slice(b"\x1b[?1049h");
    }

    if let Some(title) = &config.title {
        out.extend_from_slice(format!("\x1b]2;{title}\x1b\\").as_bytes());
    }

    if config.cursor_hidden {
        out.extend_from_slice(b"\x1b[?25l");
    }

    if let Some(style) = config.cursor_style {
        out.extend_from_slice(format!("\x1b[{style} q").as_bytes());
    }

    if config.mouse_mode {
        out.extend_from_slice(b"\x1b[?1002h\x1b[?1006h");
    }

    if let Some((url, text)) = &config.hyperlink {
        out.extend_from_slice(format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\\r\n").as_bytes());
    }

    if config.request_image_probe {
        out.extend_from_slice(b"\x1b_Gi=1,a=q,t=d,f=100;wtd-probe\x1b\\");
    }

    out
}

fn format_input_log(bytes: &[u8]) -> String {
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ");
    let escaped = bytes
        .iter()
        .flat_map(|byte| std::ascii::escape_default(*byte))
        .map(char::from)
        .collect::<String>();
    format!("[wtd-probe] input hex={hex} text={escaped}\r\n")
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn print_help() {
    eprintln!(
        "wtd-probe [--keyboard-mode csi-u|kitty] [--enable-bracketed-paste|--disable-bracketed-paste] [--alt-screen] [--title TEXT] [--cursor-hidden] [--cursor-style N] [--mouse-mode] [--hyperlink URL TEXT] [--request-image-probe]"
    );
}

#[cfg(windows)]
fn enable_raw_vt_input() -> io::Result<()> {
    use windows::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT,
        STD_INPUT_HANDLE,
    };

    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE).map_err(windows_error_to_io)?;
        let mut mode = CONSOLE_MODE(0);
        GetConsoleMode(handle, &mut mode).map_err(windows_error_to_io)?;
        const RAW_MASK: u32 = 0x0001 | 0x0002 | 0x0004 | 0x0020; // processed, line, echo, insert
        let new_mode = CONSOLE_MODE((mode.0 | ENABLE_VIRTUAL_TERMINAL_INPUT.0) & !RAW_MASK);
        SetConsoleMode(handle, new_mode).map_err(windows_error_to_io)?;
    }

    Ok(())
}

#[cfg(not(windows))]
fn enable_raw_vt_input() -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn windows_error_to_io(error: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_bytes_enable_requested_capabilities() {
        let config = ProbeConfig {
            keyboard_mode: Some(KeyboardMode::Kitty),
            bracketed_paste: Some(true),
            alt_screen: true,
            title: Some("pi host".to_string()),
            cursor_hidden: true,
            cursor_style: Some(5),
            mouse_mode: true,
            hyperlink: Some(("https://example.com".to_string(), "docs".to_string())),
            request_image_probe: true,
        };

        let bytes = startup_bytes(&config);
        assert!(bytes.starts_with(b"\x1b[>31u\x1b[?2004h\x1b[?1049h"));
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\x1b]2;pi host\x1b\\"));
        assert!(text.contains("\x1b[?25l"));
        assert!(text.contains("\x1b[5 q"));
        assert!(text.contains("\x1b[?1002h\x1b[?1006h"));
        assert!(text.contains("https://example.com"));
        assert!(bytes
            .windows(11)
            .any(|window| window == b"docs\x1b]8;;\x1b\\"));
        assert!(bytes.ends_with(b"\x1b_Gi=1,a=q,t=d,f=100;wtd-probe\x1b\\"));
    }

    #[test]
    fn parse_hyperlink_and_modes() {
        let config = ProbeConfig::parse([
            "--keyboard-mode".to_string(),
            "csi-u".to_string(),
            "--disable-bracketed-paste".to_string(),
            "--hyperlink".to_string(),
            "https://pi.ai".to_string(),
            "pi".to_string(),
        ])
        .unwrap();

        assert_eq!(config.keyboard_mode, Some(KeyboardMode::CsiU));
        assert_eq!(config.bracketed_paste, Some(false));
        assert_eq!(
            config.hyperlink,
            Some(("https://pi.ai".to_string(), "pi".to_string()))
        );
    }

    #[test]
    fn format_input_log_renders_hex_and_escaped_text() {
        let line = format_input_log(b"A\x1b[13;2u");
        assert_eq!(
            line,
            "[wtd-probe] input hex=41 1B 5B 31 33 3B 32 75 text=A\\x1b[13;2u\r\n"
        );
    }
}
