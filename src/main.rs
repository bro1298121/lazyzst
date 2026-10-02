use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

/// 文件列表可见行数
const VISIBLE_ROWS: usize = 20;
/// 主循环节拍：驱动进度动画并限制空转
const TICK: Duration = Duration::from_millis(100);
/// 动画步进，到 100 后回绕
const PROGRESS_STEP: u16 = 3;

/// 后台压缩任务的状态；只保存 Send 的数据，便于主线程随时读取
struct JobState {
    format: String,
    target: PathBuf,
    started: Instant,
    /// 动画式不确定进度（外部工具拿不到真实百分比）
    progress: u16,
}

struct App {
    current_dir: PathBuf,
    entries: Vec<PathBuf>,
    selected: usize,
    scroll_offset: usize,
    status: String,
    job: Option<JobState>,
    job_rx: Option<Receiver<Result<String>>>,
}

impl App {
    fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let entries = Self::read_dir(&current_dir);
        Self {
            current_dir,
            entries,
            selected: 0,
            scroll_offset: 0,
            status: "就绪".to_string(),
            job: None,
            job_rx: None,
        }
    }

    fn read_dir(path: &Path) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            let a_dir = a.is_dir();
            let b_dir = b.is_dir();
            b_dir.cmp(&a_dir).then(a.file_name().cmp(&b.file_name()))
        });
        entries
    }

    fn enter_dir(&mut self) {
        let entry = self.entries.get(self.selected).cloned();
        if let Some(entry) = entry
            && entry.is_dir()
        {
            let path = entry.clone();
            self.current_dir = path;
            self.entries = Self::read_dir(&self.current_dir);
            self.selected = 0;
            self.scroll_offset = 0;
            self.status = format!("进入 {}", entry.display());
        }
    }

    fn go_up(&mut self) {
        let parent = self.current_dir.parent().map(|p| p.to_path_buf());
        if let Some(parent) = parent {
            self.current_dir = parent.clone();
            self.entries = Self::read_dir(&self.current_dir);
            self.selected = 0;
            self.scroll_offset = 0;
            self.status = format!("进入 {}", parent.display());
        }
    }

    fn get_selected_path(&self) -> Option<PathBuf> {
        self.entries.get(self.selected).cloned()
    }

    fn compress(&mut self, format: &str) {
        // 同一时间只允许一个后台任务
        if self.job.is_some() {
            self.status = "已有任务进行中".to_string();
            return;
        }

        let Some(path) = self.get_selected_path() else {
            self.status = "没有选中文件".to_string();
            return;
        };

        // 线程只捕获 PathBuf 和格式名，不借用 &self
        let (tx, rx) = mpsc::channel::<Result<String>>();
        let fmt = format.to_string();
        let target = path.clone();
        std::thread::spawn(move || {
            let result = run_compress(&fmt, &target);
            let _ = tx.send(result);
        });

        self.job = Some(JobState {
            format: format.to_string(),
            target: path,
            started: Instant::now(),
            progress: 0,
        });
        self.job_rx = Some(rx);
        self.status = format!("开始压缩: {}", format);
    }

    /// 每次节拍推进进度动画（不确定进度，循环播放）
    fn tick_job(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.progress = (job.progress + PROGRESS_STEP) % 101;
        }
    }

    /// 收取后台线程的结果；任务结束后刷新列表并夹紧选中项
    fn poll_job(&mut self) {
        let Some(rx) = self.job_rx.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else {
            return;
        };

        self.job = None;
        self.job_rx = None;
        self.status = match result {
            Ok(msg) => msg,
            Err(e) => format!("错误: {}", e),
        };
        // 目录里新增了压缩产物，重新读取并防止越界
        self.entries = Self::read_dir(&self.current_dir);
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len() - 1;
        }
        let max_offset = self.entries.len().saturating_sub(VISIBLE_ROWS);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + VISIBLE_ROWS {
            self.scroll_offset = self.selected + 1 - VISIBLE_ROWS;
        }
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }
    }
}

/// 按格式分派到具体的压缩实现
fn run_compress(format: &str, path: &Path) -> Result<String> {
    match format {
        "tar" => compress_tar(path),
        "zip" => compress_zip(path),
        "wim" => compress_wim(path),
        "7z" => compress_7z(path),
        "zst" => compress_zst(path),
        "gz" => compress_gz(path),
        "xz" => compress_xz(path),
        _ => unreachable!(),
    }
}

fn compress_tar(path: &Path) -> Result<String> {
    let out = path.with_extension("tar");
    let status = Command::new("tar")
        .arg("-cf")
        .arg(&out)
        .arg(path)
        .status()?;
    if status.success() {
        Ok(format!("已打包: {}", out.display()))
    } else {
        Ok("tar 打包失败".to_string())
    }
}

fn compress_zip(path: &Path) -> Result<String> {
    let out = path.with_extension("zip");
    let status = Command::new("powershell")
        .args([
            "-Command",
            &format!(
                "Compress-Archive -Path '{}' -DestinationPath '{}' -Force",
                path.display(),
                out.display()
            ),
        ])
        .status()?;
    if status.success() {
        Ok(format!("已压缩: {}", out.display()))
    } else {
        Ok("zip 压缩失败".to_string())
    }
}

fn compress_wim(path: &Path) -> Result<String> {
    let out = path.with_extension("wim");
    let status = Command::new("dism")
        .args([
            "/Capture-Image",
            &format!("/ImageFile:{}", out.display()),
            &format!("/CaptureDir:{}", path.display()),
            "/Name:archive",
            "/Compress:max",
        ])
        .status()?;
    if status.success() {
        Ok(format!("已压缩: {}", out.display()))
    } else {
        Ok("wim 压缩失败（需要管理员权限）".to_string())
    }
}

fn compress_7z(path: &Path) -> Result<String> {
    let out = path.with_extension("7z");
    let status = Command::new("7z")
        .arg("a")
        .arg(&out)
        .arg(path)
        .status()?;
    if status.success() {
        Ok(format!("已压缩: {}", out.display()))
    } else {
        Ok("7z 压缩失败".to_string())
    }
}

fn compress_zst(path: &Path) -> Result<String> {
    let out = path.with_extension("zst");
    let status = Command::new("zstd")
        .arg("-f")
        .arg("-T0")
        .arg("-o")
        .arg(&out)
        .arg(path)
        .status()?;
    if status.success() {
        Ok(format!("已压缩: {}", out.display()))
    } else {
        Ok("zst 压缩失败".to_string())
    }
}

fn compress_gz(path: &Path) -> Result<String> {
    let out = path.with_extension("gz");
    let status = Command::new("gzip")
        .arg("-k")
        .arg("-f")
        .arg("-c")
        .arg(path)
        .stdout(std::process::Stdio::from(fs::File::create(&out)?))
        .status()?;
    if status.success() {
        Ok(format!("已压缩: {}", out.display()))
    } else {
        Ok("gz 压缩失败".to_string())
    }
}

fn compress_xz(path: &Path) -> Result<String> {
    let out = path.with_extension("xz");
    let status = Command::new("xz")
        .arg("-k")
        .arg("-f")
        .arg("-c")
        .arg(path)
        .stdout(std::process::Stdio::from(fs::File::create(&out)?))
        .status()?;
    if status.success() {
        Ok(format!("已压缩: {}", out.display()))
    } else {
        Ok("xz 压缩失败".to_string())
    }
}

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

        // 到点：推进进度动画并收取后台任务结果
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            app.tick_job();
            app.poll_job();
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    // 顶部：当前路径
    let path_text = Paragraph::new(Line::from(vec![
        Span::styled("📁 ", Style::default().fg(Color::Cyan)),
        Span::styled(
            app.current_dir.display().to_string(),
            Style::default().fg(Color::White),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("路径"))
    .style(Style::default().fg(Color::White));

    f.render_widget(path_text, chunks[0]);

    // 中部：左右分栏
    let mid_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    // 左：文件树
    let items: Vec<ListItem> = app
        .entries
        .iter()
        .enumerate()
        .skip(app.scroll_offset)
        .take(VISIBLE_ROWS)
        .map(|(i, path)| {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let (icon, color) = if path.is_dir() {
                ("📁", Color::Cyan)
            } else {
                ("📄", Color::White)
            };
            let style = if i == app.selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", icon), Style::default().fg(color)),
                Span::styled(name, style),
            ]))
        })
        .collect();

    let file_list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("文件树"))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(file_list, mid_chunks[0]);

    // 右：文件信息
    let info_text = if let Some(path) = app.get_selected_path() {
        let meta = fs::metadata(&path).unwrap_or_else(|_| {
            // 构造一个假的 metadata
            fs::metadata(".").unwrap()
        });
        let size = format_size(meta.len());
        let modified = meta
            .modified()
            .map(|t| {
                let dt: chrono::DateTime<chrono::Local> = t.into();
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            })
            .unwrap_or_else(|_| "未知".to_string());

        let lines = vec![
            Line::from(vec![
                Span::styled("名称: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("类型: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    if path.is_dir() { "目录" } else { "文件" },
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("大小: ", Style::default().fg(Color::Cyan)),
                Span::styled(size, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("修改时间: ", Style::default().fg(Color::Cyan)),
                Span::styled(modified, Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("完整路径: ", Style::default().fg(Color::Cyan)),
                Span::styled(
                    path.display().to_string(),
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];
        Paragraph::new(lines)
    } else {
        Paragraph::new("没有选中项目")
    }
    .block(Block::default().borders(Borders::ALL).title("文件信息"))
    .wrap(Wrap { trim: true });

    f.render_widget(info_text, mid_chunks[1]);

    // 底部：键位提示
    let keys = vec![
        Span::styled(" z:tar ", Style::default().fg(Color::Cyan)),
        Span::styled(" x:zip ", Style::default().fg(Color::Green)),
        Span::styled(" c:wim ", Style::default().fg(Color::Yellow)),
        Span::styled(" v:7z ", Style::default().fg(Color::Magenta)),
        Span::styled(" b:zst ", Style::default().fg(Color::Red)),
        Span::styled(" n:gz ", Style::default().fg(Color::Blue)),
        Span::styled(" m:xz ", Style::default().fg(Color::LightRed)),
        Span::styled(" | Enter:进入 ", Style::default().fg(Color::White)),
        Span::styled(" Backspace:返回 ", Style::default().fg(Color::White)),
        Span::styled(" q:退出 ", Style::default().fg(Color::White)),
    ];

    let key_hint = Paragraph::new(Line::from(keys))
        .block(Block::default().borders(Borders::ALL).title("键位"));

    f.render_widget(key_hint, chunks[2]);

    // 底部：进度 / 状态，独占一行，不会和上面任何 widget 重叠
    let gauge = match app.job.as_ref() {
        Some(job) => {
            let name = job
                .target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            Gauge::default()
                .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
                .style(Style::default().bg(Color::DarkGray))
                .label(Span::styled(
                    format!(
                        "压缩中: {} {} (已用 {}s)",
                        job.format,
                        name,
                        job.started.elapsed().as_secs()
                    ),
                    Style::default().fg(Color::White),
                ))
                .ratio(f64::from(job.progress) / 100.0)
        }
        None => Gauge::default()
            .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
            .style(Style::default().bg(Color::DarkGray))
            .label(Span::styled(
                format!("状态: {}", app.status),
                Style::default().fg(Color::White),
            ))
            .ratio(0.0),
    };

    f.render_widget(gauge, chunks[3]);
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}