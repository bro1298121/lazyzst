mod app;
mod compress;
mod extract;
mod i18n;
mod icons;
mod ui;

use std::time::Instant;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    Terminal,
};

use crate::{
    app::{App, TICK, VISIBLE_ROWS},
    i18n::Lang,
    ui::ui,
};

fn main() -> Result<()> {
    // Read the language before the alternate screen goes up: a broken config
    // degrades to the built-in copy and says so in the status bar
    let (lang, notice) = Lang::load();
    // Same reasoning for the icons: a terminal with no Nerd Font would draw
    // empty boxes, so the switch has to be settled before the first frame and
    // handed to the App rather than re-read while drawing
    let use_nerd_icons = nerd_icons_enabled(
        &std::env::args().collect::<Vec<String>>(),
        std::env::var(NERD_ICONS_ENV).ok().as_deref(),
    );

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(lang, use_nerd_icons);
    if let Some(notice) = notice {
        app.status = notice;
    }
    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

/// Environment variable that turns the Nerd Font icons off
const NERD_ICONS_ENV: &str = "LAZYZIP_NERD_ICONS";

/// Command line flag that turns the Nerd Font icons off
const NO_NERD_ICONS_FLAG: &str = "--no-nerd-icons";

/// Whether the listing leads its rows with Nerd Font glyphs.
///
/// On unless something says otherwise, so a terminal that has the font gets the
/// per-type glyphs and every other terminal quietly keeps the plain emoji
/// instead of a column of empty boxes. Either switch turns them off.
///
/// When both are given, off wins. A variable left over in a shell profile is
/// the one that is easy to forget about and hard to see, so it must not be able
/// to overrule a flag somebody typed in front of them
fn nerd_icons_enabled(args: &[String], env: Option<&str>) -> bool {
    if args.iter().any(|arg| arg == NO_NERD_ICONS_FLAG) {
        return false;
    }
    match env {
        // An unset or empty variable says nothing, so the default stands
        None => true,
        Some(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        ),
    }
}

fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let result = event_loop(terminal, app);
    // Whether we quit normally or bail out mid-loop, reap the child process
    // before leaving the alternate screen
    app.cancel_job();
    result
}

fn event_loop<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| ui(f, app))?;

        // Sleep until the next tick while still answering key presses
        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? && let Event::Key(key) = event::read()? {
            // One physical key press produces both a Press and a Release event
            // on Windows; act on the Press only
            if !matches!(key.kind, KeyEventKind::Press) {
                continue;
            }
            // The dialog is modal: focus belongs to it, only Enter / Esc get
            // through. Every other key (q and z..m included) is swallowed so a
            // stray press cannot delete anything or quit the program
            if app.dialog.is_some() {
                match key.code {
                    KeyCode::Enter => app.confirm_dialog(),
                    KeyCode::Esc => app.dismiss_dialog(),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('j') | KeyCode::Down => {
                        if app.selected + 1 < app.entries.len() {
                            app.selected += 1;
                            if app.selected >= app.scroll_offset + VISIBLE_ROWS {
                                app.scroll_offset += 1;
                            }
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if app.selected > 0 {
                            app.selected -= 1;
                            if app.selected < app.scroll_offset {
                                app.scroll_offset -= 1;
                            }
                        }
                    }
                    KeyCode::Enter => app.enter_dir(),
                    KeyCode::Backspace => app.go_up(),
                    KeyCode::Char('z') => app.compress("tar"),
                    KeyCode::Char('x') => app.compress("zip"),
                    KeyCode::Char('c') => app.compress("wim"),
                    KeyCode::Char('v') => app.compress("7z"),
                    KeyCode::Char('b') => app.compress("zst"),
                    KeyCode::Char('n') => app.compress("gz"),
                    KeyCode::Char('m') => app.compress("xz"),
                    KeyCode::Char('d') => app.request_delete(),
                    KeyCode::Char('e') => app.extract(),
                    KeyCode::Char('h') => {
                        app.selected = 0;
                        app.scroll_offset = 0;
                    }
                    KeyCode::Char('l') => {
                        app.selected = app.entries.len().saturating_sub(1);
                        app.scroll_offset = app.selected.saturating_sub(VISIBLE_ROWS - 1);
                    }
                    // Marks are how a batch is built up: Space adds or removes
                    // one, A takes everything in this directory, u drops just the
                    // entry under the cursor and U drops all of them
                    KeyCode::Char(' ') => app.toggle_mark(),
                    KeyCode::Char('A') => app.mark_all(),
                    KeyCode::Char('u') => app.unmark_selected(),
                    KeyCode::Char('U') => app.clear_marks(),
                    _ => {}
                }
            }
        }

        // Tick reached: advance the progress animation and collect the job result
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            app.tick_job();
            app.poll_job();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arguments a shell would hand over, program name included
    fn args(extra: &[&str]) -> Vec<String> {
        std::iter::once("lazyzst")
            .chain(extra.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn the_icons_are_on_unless_something_says_otherwise() {
        // Nothing at all: the glyphs are the point of the feature, so a plain
        // run must not need a flag to get them
        assert!(nerd_icons_enabled(&args(&[]), None));
        assert!(nerd_icons_enabled(&args(&[]), Some("")));
        assert!(nerd_icons_enabled(&args(&[]), Some("1")));
        assert!(nerd_icons_enabled(&args(&[]), Some("on")));
        // An unrelated flag must not switch them off by accident
        assert!(nerd_icons_enabled(&args(&["--lang", "en-us"]), None));
    }

    #[test]
    fn either_switch_turns_the_icons_off() {
        // The flag, wherever it sits among the other arguments
        assert!(!nerd_icons_enabled(&args(&["--no-nerd-icons"]), None));
        assert!(!nerd_icons_enabled(&args(&["--lang", "en-us", "--no-nerd-icons"]), None));

        // The environment, in every spelling a shell script is likely to write
        for value in ["0", "off", "OFF", "False", "no", " off ", "\tfalse\n"] {
            assert!(
                !nerd_icons_enabled(&args(&[]), Some(value)),
                "{value:?} should turn the icons off"
            );
        }

        // Both switches present is still off: the flag is the one somebody typed
        // in front of them, so it has the last word
        assert!(!nerd_icons_enabled(&args(&["--no-nerd-icons"]), Some("0")));
        assert!(!nerd_icons_enabled(&args(&["--no-nerd-icons"]), Some("1")));
    }
}
