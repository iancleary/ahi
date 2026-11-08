//! Terminal setup and teardown, the part of the `ratatui` facade ahi used.
//! Raw mode and the alternate screen are entered together and left together,
//! and a panic hook restores the terminal before the default hook prints,
//! so a panic never leaves the shell in raw mode.

use std::io::{self, stdout, Stdout};
use std::sync::Once;

use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui_core::terminal::Terminal;
use ratatui_crossterm::CrosstermBackend;

pub type DefaultTerminal = Terminal<CrosstermBackend<Stdout>>;

static PANIC_HOOK: Once = Once::new();

/// Enter raw mode and the alternate screen and return a terminal on stdout.
/// Installs the restoring panic hook once per process.
pub fn try_init() -> io::Result<DefaultTerminal> {
    PANIC_HOOK.call_once(|| {
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            hook(info);
        }));
    });
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout()))
}

/// Leave raw mode and the alternate screen. Failing to restore is reported
/// on stderr and otherwise ignored: there is nothing further to do.
pub fn restore() {
    if let Err(err) = try_restore() {
        eprintln!("failed to restore the terminal: {err}");
    }
}

fn try_restore() -> io::Result<()> {
    // Raw mode has more side effects than the alternate screen; leave it first.
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}
