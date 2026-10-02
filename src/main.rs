mod app;
mod compress;
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
    ui::ui,
};

fn main() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let result = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let result = event_loop(terminal, app);
    // 正常退出或中途出错，都要在离开 alternate screen 前回收子进程
    app.cancel_job();
    result
}

fn event_loop<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let mut last_tick = Instant::now();
    loop {
        terminal.draw(|f| ui(f, app))?;

        // 等待下一次节拍，期间仍可响应按键
        let timeout = TICK.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? && let Event::Key(key) = event::read()? {
            // Windows 上一次物理按键会同时产生 Press 与 Release，只处理 Press
            if !matches!(key.kind, KeyEventKind::Press) {
                continue;
            }
            // 弹窗是模态的：焦点在弹窗上，只有 Enter / Esc 能穿透，
            // 其余按键（含 q 和 z~m）一律吞掉，避免误触直接删掉或退出程序
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

        // 到点：推进进度动画并收取后台任务结果
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            app.tick_job();
            app.poll_job();
        }
    }

    Ok(())
}