use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    poll, read, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, MouseEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn main() -> io::Result<()> {
    let options = Options::parse();
    let mut stdout = io::stdout();
    let mut log = options
        .log_path
        .as_ref()
        .map(|path| OpenOptions::new().create(true).append(true).open(path))
        .transpose()?;

    enable_raw_mode()?;
    let mut cleanup = Cleanup {
        alt_screen: options.alt_screen,
        mouse_capture: options.mouse_capture,
    };

    if options.alt_screen {
        execute!(stdout, EnterAlternateScreen)?;
    }
    if options.mouse_capture {
        execute!(stdout, EnableMouseCapture)?;
    }
    execute!(stdout, Hide)?;

    let started = Instant::now();
    let initial_size = terminal::size().unwrap_or((0, 0));
    let mut current_size = initial_size;
    let mut last_polled_size = initial_size;
    let mut last_resize_event: Option<(u16, u16)> = None;
    let mut resize_events = 0u64;
    let mut key_events = 0u64;
    let mut mouse_events = 0u64;
    let mut focus_events = 0u64;
    let mut paste_events = 0u64;
    let other_events = 0u64;
    let mut frame = 0u64;
    let mut exit_reason = String::from("running");
    let mut lines = VecDeque::with_capacity(24);

    push_log(
        &mut lines,
        &mut log,
        format!("startup initial_size={}x{}", initial_size.0, initial_size.1),
    )?;

    loop {
        frame += 1;
        let polled_size = terminal::size().unwrap_or((0, 0));
        if polled_size != last_polled_size {
            push_log(
                &mut lines,
                &mut log,
                format!(
                    "terminal::size changed {}x{} -> {}x{}",
                    last_polled_size.0, last_polled_size.1, polled_size.0, polled_size.1
                ),
            )?;
            last_polled_size = polled_size;
            current_size = polled_size;
        }

        if poll(Duration::from_millis(options.tick_ms))? {
            loop {
                match read()? {
                    Event::Resize(cols, rows) => {
                        resize_events += 1;
                        current_size = (cols, rows);
                        last_resize_event = Some((cols, rows));
                        push_log(
                            &mut lines,
                            &mut log,
                            format!("resize event -> {}x{}", cols, rows),
                        )?;
                    }
                    Event::Key(key) => {
                        key_events += 1;
                        let description = describe_key(&key);
                        push_log(&mut lines, &mut log, format!("key event -> {description}"))?;
                        if matches!(key.code, KeyCode::Esc)
                            || matches!(key.code, KeyCode::Char('q' | 'Q'))
                        {
                            exit_reason = format!("exit key: {description}");
                            draw(
                                &mut stdout,
                                &lines,
                                &State {
                                    initial_size,
                                    current_size,
                                    last_polled_size,
                                    last_resize_event,
                                    resize_events,
                                    key_events,
                                    mouse_events,
                                    focus_events,
                                    paste_events,
                                    other_events,
                                    frame,
                                    started,
                                    exit_reason: &exit_reason,
                                    mouse_capture_enabled: options.mouse_capture,
                                    alt_screen_enabled: options.alt_screen,
                                },
                            )?;
                            cleanup.run(&mut stdout)?;
                            return Ok(());
                        }
                    }
                    Event::Mouse(mouse) => {
                        mouse_events += 1;
                        push_log(
                            &mut lines,
                            &mut log,
                            format!("mouse event -> {}", describe_mouse(&mouse)),
                        )?;
                    }
                    Event::FocusGained => {
                        focus_events += 1;
                        push_log(&mut lines, &mut log, "focus event -> gained".to_string())?;
                    }
                    Event::FocusLost => {
                        focus_events += 1;
                        push_log(&mut lines, &mut log, "focus event -> lost".to_string())?;
                    }
                    Event::Paste(text) => {
                        paste_events += 1;
                        push_log(
                            &mut lines,
                            &mut log,
                            format!("paste event -> {} chars", text.chars().count()),
                        )?;
                    }
                }

                if !poll(Duration::from_millis(0))? {
                    break;
                }
            }
        }

        draw(
            &mut stdout,
            &lines,
            &State {
                initial_size,
                current_size,
                last_polled_size,
                last_resize_event,
                resize_events,
                key_events,
                mouse_events,
                focus_events,
                paste_events,
                other_events,
                frame,
                started,
                exit_reason: &exit_reason,
                mouse_capture_enabled: options.mouse_capture,
                alt_screen_enabled: options.alt_screen,
            },
        )?;
    }
}

struct State<'a> {
    initial_size: (u16, u16),
    current_size: (u16, u16),
    last_polled_size: (u16, u16),
    last_resize_event: Option<(u16, u16)>,
    resize_events: u64,
    key_events: u64,
    mouse_events: u64,
    focus_events: u64,
    paste_events: u64,
    other_events: u64,
    frame: u64,
    started: Instant,
    exit_reason: &'a str,
    mouse_capture_enabled: bool,
    alt_screen_enabled: bool,
}

fn draw(stdout: &mut io::Stdout, lines: &VecDeque<String>, state: &State<'_>) -> io::Result<()> {
    let size = terminal::size().unwrap_or(state.last_polled_size);
    let width = size.0.max(20) as usize;
    let height = size.1.max(8) as usize;
    let elapsed = state.started.elapsed();
    let event_size = state
        .last_resize_event
        .map(|(w, h)| format!("{w}x{h}"))
        .unwrap_or_else(|| "none".to_string());

    execute!(
        stdout,
        Clear(ClearType::All),
        crossterm::cursor::MoveTo(0, 0)
    )?;

    let header = vec![
        fit(
            &format!(
                "WTD-CROSSTERM-PROBE frame={} elapsed={:02}:{:02}",
                state.frame,
                elapsed.as_secs() / 60,
                elapsed.as_secs() % 60
            ),
            width,
        ),
        fit(
            &format!(
                "initial_size={}x{} current_size={}x{} last_polled={}x{} last_resize_event={}",
                state.initial_size.0,
                state.initial_size.1,
                state.current_size.0,
                state.current_size.1,
                state.last_polled_size.0,
                state.last_polled_size.1,
                event_size
            ),
            width,
        ),
        fit(
            &format!(
                "resize_events={} key_events={} mouse_events={} focus_events={} paste_events={} other_events={}",
                state.resize_events,
                state.key_events,
                state.mouse_events,
                state.focus_events,
                state.paste_events,
                state.other_events
            ),
            width,
        ),
        fit(
            &format!(
                "Press q or Esc to quit. Mouse capture is {}. Alt screen is {}.",
                on_off(state.mouse_capture_enabled),
                on_off(state.alt_screen_enabled)
            ),
            width,
        ),
        fit(&format!("exit_reason={}", state.exit_reason), width),
        fit(&"-".repeat(width.min(80)), width),
    ];

    for line in header {
        writeln!(stdout, "{line}")?;
    }

    let remaining = height.saturating_sub(6);
    let log_rows = remaining.min(12);

    for line in lines.iter().rev().take(log_rows).rev() {
        writeln!(stdout, "{}", fit(line, width))?;
    }

    let pattern_rows = remaining.saturating_sub(log_rows);
    if pattern_rows > 0 {
        draw_resize_pattern(stdout, width, pattern_rows, state.current_size)?;
    }

    stdout.flush()
}

fn draw_resize_pattern(
    stdout: &mut io::Stdout,
    width: usize,
    rows: usize,
    current_size: (u16, u16),
) -> io::Result<()> {
    if rows == 0 {
        return Ok(());
    }

    let banner = fit(
        &format!(
            "= RESIZE FIELD {}x{} {}",
            current_size.0,
            current_size.1,
            "=".repeat(width)
        ),
        width,
    );
    writeln!(stdout, "{banner}")?;

    if rows == 1 {
        return Ok(());
    }

    let inner_rows = rows.saturating_sub(2);
    for row in 0..inner_rows {
        let mut line = vec![' '; width];
        if width >= 2 {
            line[0] = '|';
            line[width - 1] = '|';
        }

        for col in (8..width.saturating_sub(8)).step_by(8) {
            if line[col] == ' ' {
                line[col] = ':';
            }
        }
        if row % 4 == 3 {
            for ch in line.iter_mut().take(width.saturating_sub(1)).skip(1) {
                if *ch == ' ' {
                    *ch = '-';
                }
            }
        }

        let diagonal_left = if inner_rows <= 1 {
            1
        } else {
            1 + row * width.saturating_sub(2) / inner_rows.saturating_sub(1)
        };
        let diagonal_right = width
            .saturating_sub(2)
            .saturating_sub(diagonal_left.saturating_sub(1));
        if diagonal_left < width.saturating_sub(1) && line[diagonal_left] == ' ' {
            line[diagonal_left] = '\\';
        }
        if diagonal_right < width.saturating_sub(1) && line[diagonal_right] == ' ' {
            line[diagonal_right] = '/';
        }

        if row == 1 {
            place_text(&mut line, 2, "top-left");
            place_text(&mut line, width.saturating_sub(12), "top-right");
        } else if row == inner_rows.saturating_sub(2) {
            place_text(&mut line, 2, "bottom-left");
            place_text(&mut line, width.saturating_sub(14), "bottom-right");
        } else if row == inner_rows / 2 {
            let label = format!("center {}x{}", current_size.0, current_size.1);
            let start = width.saturating_sub(label.len()) / 2;
            place_text(&mut line, start, &label);
        }

        writeln!(stdout, "{}", line.into_iter().collect::<String>())?;
    }

    let footer = fit(
        &format!("= BOTTOM EDGE row={} {}", inner_rows, "=".repeat(width)),
        width,
    );
    writeln!(stdout, "{footer}")?;
    Ok(())
}

fn place_text(line: &mut [char], start: usize, text: &str) {
    if start >= line.len() {
        return;
    }
    for (offset, ch) in text.chars().enumerate() {
        let idx = start + offset;
        if idx >= line.len() {
            break;
        }
        line[idx] = ch;
    }
}

fn fit(text: &str, width: usize) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    if chars.len() > width {
        chars.truncate(width);
    }
    let mut output: String = chars.into_iter().collect();
    let pad = width.saturating_sub(output.chars().count());
    if pad > 0 {
        output.push_str(&" ".repeat(pad));
    }
    output
}

fn push_log(
    lines: &mut VecDeque<String>,
    log: &mut Option<std::fs::File>,
    line: String,
) -> io::Result<()> {
    const MAX_LOG_LINES: usize = 24;
    lines.push_back(line.clone());
    while lines.len() > MAX_LOG_LINES {
        let _ = lines.pop_front();
    }
    if let Some(file) = log {
        writeln!(file, "{line}")?;
        file.flush()?;
    }
    Ok(())
}

fn on_off(value: bool) -> &'static str {
    if value {
        "ON"
    } else {
        "OFF"
    }
}

fn describe_key(key: &KeyEvent) -> String {
    format!("{:?}", key)
}

fn describe_mouse(mouse: &MouseEvent) -> String {
    format!("{:?}", mouse)
}

struct Cleanup {
    alt_screen: bool,
    mouse_capture: bool,
}

impl Cleanup {
    fn run(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        execute!(stdout, Show)?;
        if self.mouse_capture {
            execute!(stdout, DisableMouseCapture)?;
            self.mouse_capture = false;
        }
        if self.alt_screen {
            execute!(stdout, LeaveAlternateScreen)?;
            self.alt_screen = false;
        }
        disable_raw_mode()
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show);
        if self.mouse_capture {
            let _ = execute!(stdout, DisableMouseCapture);
            self.mouse_capture = false;
        }
        if self.alt_screen {
            let _ = execute!(stdout, LeaveAlternateScreen);
            self.alt_screen = false;
        }
    }
}

struct Options {
    tick_ms: u64,
    alt_screen: bool,
    mouse_capture: bool,
    log_path: Option<PathBuf>,
}

impl Options {
    fn parse() -> Self {
        let mut tick_ms = 100u64;
        let mut alt_screen = true;
        let mut mouse_capture = true;
        let mut log_path = None;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--tick-ms" => {
                    if let Some(value) = args.next() {
                        tick_ms = value.parse().unwrap_or(100);
                    }
                }
                "--no-alt" => alt_screen = false,
                "--no-mouse" => mouse_capture = false,
                "--log" => {
                    if let Some(value) = args.next() {
                        log_path = Some(PathBuf::from(value));
                    }
                }
                _ => {}
            }
        }

        Self {
            tick_ms,
            alt_screen,
            mouse_capture,
            log_path,
        }
    }
}
