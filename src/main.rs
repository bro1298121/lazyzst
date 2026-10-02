mod app;
mod compress;
mod extract;
mod i18n;
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

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(lang);
    if let Some(notice) = notice {
        app.status = notice;
    }
    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
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