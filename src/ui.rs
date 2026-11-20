//! Terminal styling helpers.

use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

pub fn set_color(on: bool) {
    COLOR.store(on, Ordering::Relaxed);
}

fn paint(code: &str, s: &str) -> String {
    if COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String { paint("1", s) }
pub fn dim(s: &str) -> String { paint("2", s) }
pub fn red(s: &str) -> String { paint("31", s) }
pub fn green(s: &str) -> String { paint("32", s) }
pub fn yellow(s: &str) -> String { paint("33", s) }
pub fn magenta(s: &str) -> String { paint("35", s) }
pub fn cyan(s: &str) -> String { paint("36", s) }
pub fn accent(s: &str) -> String { paint("38;5;43", s) }

/// Pads `s` to `w` visible columns (ignores ANSI sequences).
pub fn pad(s: &str, w: usize) -> String {
    let vis = visible_len(s);
    if vis >= w {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(w - vis))
    }
}

pub fn visible_len(s: &str) -> usize {
    let mut n = 0;
    let mut esc = false;
    for c in s.chars() {
        if esc {
            if c.is_ascii_alphabetic() {
                esc = false;
            }
        } else if c == '\x1b' {
            esc = true;
        } else {
            n += 1;
        }
    }
    n
}

pub fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub fn bytes(n: usize) -> String {
    if n < 1000 {
        format!("{} B", n)
    } else if n < 1_000_000 {
        format!("{:.1} KB", n as f64 / 1000.0)
    } else {
        format!("{:.1} MB", n as f64 / 1_000_000.0)
    }
}

pub fn ms(n: u64) -> String {
    if n < 1000 {
        format!("{}ms", n)
    } else {
        format!("{:.2}s", n as f64 / 1000.0)
    }
}
