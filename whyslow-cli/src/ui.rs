//! Terminal cosmetics: banner + ANSI color helpers. Colors are only emitted
//! to an actual terminal (`NO_COLOR` respected, non-tty output like a pipe or
//! file redirect gets plain text) -- this is also why the integration test's
//! substring matching on `whyslow run`'s piped stdout never sees escape codes
//! and doesn't need to know this module exists.

use std::io::IsTerminal;
use std::sync::OnceLock;

fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal())
}

fn paint(code: &str, s: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn bold_yellow(s: &str) -> String {
    paint("1;93", s)
}
pub fn bold_red(s: &str) -> String {
    paint("1;91", s)
}
pub fn magenta(s: &str) -> String {
    paint("1;95", s)
}
pub fn cyan(s: &str) -> String {
    paint("1;96", s)
}
pub fn green(s: &str) -> String {
    paint("1;92", s)
}
pub fn bold_cyan(s: &str) -> String {
    paint("1;96", s)
}
pub fn blue(s: &str) -> String {
    paint("1;94", s)
}

/// One bright color per banner line -- rainbow gradient, cycling through the
/// same 256-color palette `lolcat`-style tools use for this exact effect.
const BANNER_COLORS: &[&str] = &["1;91", "1;93", "1;92", "1;96", "1;94", "1;95"];

const BANNER_LINES: &[&str] = &[
    r#"           __               __"#,
    r#" _      __/ /_  __  _______/ /___ _      __"#,
    r#"| | /| / / __ \/ / / / ___/ / __ \ | /| / /"#,
    r#"| |/ |/ / / / / /_/ (__  ) / /_/ / |/ |/ /"#,
    r#"|__/|__/_/ /_/\__, /____/_/\____/|__/|__/"#,
    r#"             /____/                        "#,
];

pub fn print_banner() {
    println!();
    for (line, code) in BANNER_LINES.iter().zip(BANNER_COLORS.iter().cycle()) {
        println!("{}", paint(code, line));
    }
    println!(
        "  {}",
        bold("debug why a Linux process was slow \u{2014} eBPF causal-chain tracing")
    );
    println!();
}
