use std::fs;

use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, VISIBLE_ROWS};

pub(crate) fn ui(f: &mut Frame, app: &mut App) {
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
        // 读不到 metadata 就显示"未知"，不伪造大小和时间
        let (size, modified) = match fs::metadata(&path) {
            Ok(meta) => (
                format_size(meta.len()),
                meta.modified()
                    .map(|t| {
                        let dt: chrono::DateTime<chrono::Local> = t.into();
                        dt.format("%Y-%m-%d %H:%M:%S").to_string()
                    })
                    .unwrap_or_else(|_| "未知".to_string()),
            ),
            Err(_) => ("未知".to_string(), "未知".to_string()),
        };

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

pub(crate) fn format_size(bytes: u64) -> String {
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