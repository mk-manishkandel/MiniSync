//! Tiny terminal styling for the menu: colours, a framed header and
//! status lines. Falls back to plain text if the console can't do colour
//! (or NO_COLOR is set).

use std::sync::atomic::{AtomicBool, Ordering};

static COLOR: AtomicBool = AtomicBool::new(false);

pub fn init() {
    let wanted = std::env::var_os("NO_COLOR").is_none();
    COLOR.store(wanted && enable_vt(), Ordering::Relaxed);
}

#[cfg(windows)]
fn enable_vt() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n: u32) -> *mut std::ffi::c_void;
        fn GetConsoleMode(h: *mut std::ffi::c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: *mut std::ffi::c_void, mode: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        GetConsoleMode(h, &mut mode) != 0 && SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
    }
}

#[cfg(not(windows))]
fn enable_vt() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn paint(code: &str, s: &str) -> String {
    if COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String { paint("1", s) }
pub fn dim(s: &str) -> String { paint("2", s) }
pub fn green(s: &str) -> String { paint("32", s) }
pub fn red(s: &str) -> String { paint("31", s) }
pub fn yellow(s: &str) -> String { paint("33", s) }
pub fn cyan(s: &str) -> String { paint("36", s) }
pub fn accent(s: &str) -> String { paint("1;36", s) }

pub fn clear() {
    if COLOR.load(Ordering::Relaxed) {
        print!("\x1b[2J\x1b[3J\x1b[H");
    } else {
        println!("\n");
    }
}

/// A rounded box around a few lines. Each line is (plain text, styled text)
/// so the width can be measured without the colour codes.
pub fn boxed(lines: &[(String, String)]) {
    let width = lines.iter().map(|(p, _)| p.chars().count()).max().unwrap_or(0) + 4;
    println!("  {}", dim(&format!("╭{}╮", "─".repeat(width))));
    for (plain, styled) in lines {
        let pad = width - 2 - plain.chars().count();
        println!("  {}  {}{}{}", dim("│"), styled, " ".repeat(pad), dim("│"));
    }
    println!("  {}", dim(&format!("╰{}╯", "─".repeat(width))));
}

/// Starts a fresh screen for one menu action, so its questions start at
/// the top instead of scrolling below the menu.
pub fn page(title: &str) {
    clear();
    println!("\n  {}  {}  {}", accent("Mini Sync"), dim("›"), bold(title));
    println!("  {}", dim(&"─".repeat(64)));
}

pub fn section(title: &str) {
    println!("\n  {}", dim(&title.to_uppercase()));
}

/// "  label        value"
pub fn field(label: &str, value: &str) {
    println!("  {}  {}", dim(&format!("{label:<12}")), value);
}

pub fn ok(msg: &str) {
    println!("  {} {msg}", green("✓"));
}

pub fn err(msg: &str) {
    println!("  {} {msg}", red("✗"));
}

pub fn warn(msg: &str) {
    println!("  {} {msg}", yellow("!"));
}

pub fn info(msg: &str) {
    println!("  {}", dim(msg));
}

pub fn prompt(q: &str) {
    print!("  {} {q}", cyan("›"));
}
