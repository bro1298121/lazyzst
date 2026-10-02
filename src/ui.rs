use std::{fs, path::Path};

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
        None => {
            // 有最近产物时按实际宽度拼装，放不下就把路径省略成 盘符:\...文件名.格式
            let text = match app.last_out.as_ref() {
                Some(o) => {
                    let area = chunks[3];
                    let head = format!("状态: {}: ", o.prefix);
                    let tail = format!("  大小: {}", o.size);
                    let fixed = display_width(&head) + display_width(&tail);
                    let budget = (area.width as usize).saturating_sub(fixed);
                    let path = elide_path(&o.path, budget);
                    // 定长片段本身就超宽时（例如大小文案特别长）再兜底截一次
                    truncate_to_width(&format!("{}{}{}", head, path, tail), area.width as usize)
                }
                None => format!("状态: {}", app.status),
            };
            Gauge::default()
                .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
                .style(Style::default().bg(Color::DarkGray))
                .label(Span::styled(text, Style::default().fg(Color::White)))
                .ratio(0.0)
        }
    };

    f.render_widget(gauge, chunks[3]);
}

/// 单个字符占用的显示列数：控制字符 0 列，ASCII / 半角 1 列，
/// CJK 与 emoji 2 列。属于常规近似，不追求完整 UAX#11
fn char_width(c: char) -> usize {
    match c {
        '\u{00}'..='\u{1f}' | '\u{7f}'..='\u{9f}' => 0,
        '\u{200b}'..='\u{200f}' | '\u{feff}' => 0,
        '\u{0300}'..='\u{036f}' => 0,
        _ if (c as u32) < 0x1100 => 1,
        _ if is_wide(c as u32) => 2,
        _ => 1,
    }
}

/// East Asian Wide / Fullwidth 与常用 emoji 区段
fn is_wide(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115f      // Hangul Jamo
        | 0x2e80..=0x303e    // CJK 部首、标点
        | 0x3041..=0x33ff    // 假名、注音、CJK 兼容
        | 0x3400..=0x4dbf    // CJK 扩展 A
        | 0x4e00..=0x9fff    // CJK 统一表意
        | 0xa000..=0xa4cf    // 彝文
        | 0xac00..=0xd7a3    // Hangul 音节
        | 0xf900..=0xfaff    // CJK 兼容表意
        | 0xfe10..=0xfe19    // 竖排标点
        | 0xfe30..=0xfe6f    // CJK 兼容形式
        | 0xff00..=0xff60    // 全角 ASCII
        | 0xffe0..=0xffe6    // 全角符号
        | 0x1f300..=0x1f64f  // 杂项符号与图形、emoji 表情
        | 0x1f680..=0x1f6ff  // 交通与地图
        | 0x1f900..=0x1f9ff  // 补充符号与图形
        | 0x20000..=0x3fffd  // CJK 扩展 B 及以后
    )
}

/// 估算字符串占用的终端显示列数
pub(crate) fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// 按显示宽度硬截断，保证结果不超过 `max_width` 列
fn truncate_to_width(s: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for c in s.chars() {
        let w = char_width(c);
        if width + w > max_width {
            break;
        }
        out.push(c);
        width += w;
    }
    out
}

/// 路径的根部分：盘符加分隔符；没有盘符时取到第一个分隔符之前的部分
fn path_root(path: &Path) -> String {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();

    // 盘符 + 分隔符，如 D:\
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let sep = bytes[2];
        if sep == b'\\' || sep == b'/' {
            return s[..3].to_string();
        }
    }

    // 否则取到第一个分隔符（含分隔符）之前的部分
    if let Some(i) = s.find(['\\', '/']) {
        s[..=i].to_string()
    } else {
        String::new()
    }
}

/// 超宽时把路径省略成 `根 + "..." + 文件名`，核心不变式：
/// 返回值的显示宽度在任何输入下都不超过 `max_width` 列
pub(crate) fn elide_path(path: &Path, max_width: usize) -> String {
    let full = path.to_string_lossy().to_string();
    if display_width(&full) <= max_width {
        return full;
    }

    // 文件名缺失（路径以分隔符结尾等）时直接截断原串
    let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return truncate_to_width(&full, max_width);
    };

    let root = path_root(path);
    let (stem, ext) = match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i..]),
        _ => (file_name.as_str(), ""),
    };

    // 逐步截短文件名，扩展名优先保住
    let fixed_width = display_width(&root) + 3;
    let keep = max_width.saturating_sub(fixed_width);
    let out = if display_width(stem) <= keep {
        format!("{}...{}{}", root, stem, ext)
    } else {
        // 只剩扩展名的位置也不够就先砍文件名
        let stem = truncate_to_width(stem, keep.saturating_sub(display_width(ext)));
        format!("{}...{}{}", root, stem, ext)
    };
    // 根 + "..." + 扩展名本身就超宽时，按显示宽度硬截断兜底
    truncate_to_width(&out, max_width)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_counts_ascii_cjk_and_emoji_as_columns() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("状态"), 4);
        assert_eq!(display_width("📁"), 2);
        assert_eq!(display_width("📁 文件"), 2 + 1 + 4);
        // 控制字符不占列
        assert_eq!(display_width("\u{1b}"), 0);
        assert_eq!(display_width("ab\u{1b}c"), 3);
    }

    #[test]
    fn short_path_returned_as_is() {
        let p = Path::new(r"D:\a.zst");
        assert_eq!(elide_path(p, 100), r"D:\a.zst");
    }

    #[test]
    fn long_path_elides_to_root_dots_filename_with_extension() {
        let p = Path::new(r"D:\lazyarchive\lazyzip\BussinGriddyCode.zst");
        let out = elide_path(p, 28);
        assert_eq!(out, r"D:\...BussinGriddyCode.zst");
    }

    #[test]
    fn extension_is_preserved_when_stem_gets_cut() {
        let p = Path::new(r"D:\a\very\deep\directory\BussinGriddyCode.zst");
        let out = elide_path(p, 20);
        assert!(out.ends_with(".zst"), "扩展名必须保留: {}", out);
        assert!(out.starts_with(r"D:\..."), "应保留盘符根: {}", out);
    }

    #[test]
    fn width_never_exceeds_budget() {
        let paths = [
            r"D:\lazyarchive\lazyzip\BussinGriddyCode.zst",
            r"D:\a.zst",
            r"/usr/local/share/documents/report.7z",
            r"D:\工作目录\压缩测试\很长的中文文件名.tar.gz",
            r"D:\📁目录\😀表情\文件.wim",
            r"relative.zst",
            r"D:\",
            r"\",
            r"",
        ];
        for p in paths {
            for budget in [0usize, 1, 2, 3, 4, 5, 6, 8, 10, 15, 20, 40, 200] {
                let out = elide_path(Path::new(p), budget);
                assert!(
                    display_width(&out) <= budget,
                    "超宽: path={:?} budget={} out={:?} width={}",
                    p,
                    budget,
                    out,
                    display_width(&out)
                );
            }
        }
    }

    #[test]
    fn tiny_budget_still_returns_something_without_panicking() {
        let p = Path::new(r"D:\lazyarchive\lazyzip\BussinGriddyCode.zst");
        for budget in 0..6 {
            let out = elide_path(p, budget);
            assert!(display_width(&out) <= budget);
        }
    }

    #[test]
    fn cjk_and_emoji_paths_count_as_two_columns() {
        // 中文文件名每个字符 2 列，预算按列算而不是按字符数
        let p = Path::new(r"D:\目录\中文文件.tar.gz");
        let budget = 16;
        let out = elide_path(p, budget);
        assert!(display_width(&out) <= budget, "out={:?}", out);
        assert!(out.ends_with(".gz"), "扩展名必须保留: {}", out);

        let emoji = Path::new(r"D:\😀😀😀\a.zst");
        assert!(display_width(&emoji.to_string_lossy()) > emoji.to_string_lossy().chars().count());
    }

    #[test]
    fn degenerate_paths_do_not_panic() {
        // 以分隔符结尾 / 无文件名 / 纯根 / 空路径
        for p in [r"D:\", "/", "", r"\\", r"C:", r"D:\.hidden", r"D:\..", r"D:\a."] {
            for budget in [0usize, 1, 3, 7, 50] {
                let out = elide_path(Path::new(p), budget);
                assert!(display_width(&out) <= budget, "path={:?} out={:?}", p, out);
            }
        }
    }

    #[test]
    fn unix_path_uses_slash_root() {
        let p = Path::new("/usr/local/share/documents/report.7z");
        let out = elide_path(p, 24);
        assert_eq!(out, "/...report.7z");
    }
}