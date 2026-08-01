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
pub fn cyan(s: &str) -> String {
    paint("36", s)
}
pub fn bold_yellow(s: &str) -> String {
    paint("1;33", s)
}
pub fn bold_red(s: &str) -> String {
    paint("1;31", s)
}
pub fn magenta(s: &str) -> String {
    paint("35", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn bold_cyan(s: &str) -> String {
    paint("1;36", s)
}

const BANNER: &str = r#"
           __               __
 _      __/ /_  __  _______/ /___ _      __
| | /| / / __ \/ / / / ___/ / __ \ | /| / /
| |/ |/ / / / / /_/ (__  ) / /_/ / |/ |/ /
|__/|__/_/ /_/\__, /____/_/\____/|__/|__/
             /____/                        "#;

pub fn print_banner() {
    if color_enabled() {
        println!("{}", cyan(BANNER));
    } else {
        println!("{BANNER}");
    }
    println!(
        "  {}",
        dim("debug why a Linux process was slow \u{2014} eBPF causal-chain tracing")
    );
    println!();
}
