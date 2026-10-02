use std::{fs, path::Path};

use ratatui::{
    layout::{Constraint, Direction, Layout, Margin, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{App, Dialog, VISIBLE_ROWS},
    i18n::Lang,
};

pub(crate) fn ui(f: &mut Frame, app: &mut App) {
    // Every label below comes from the table; `app` is only read here, so the
    // borrow can live for the whole frame
    let lang = &app.lang;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(f.area());

    // Top: current path
    let path_text = Paragraph::new(Line::from(vec![
        Span::styled("📁 ", Style::default().fg(Color::Cyan)),
        Span::styled(
            app.current_dir.display().to_string(),
            Style::default().fg(Color::White),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title(lang.t("panel.path")))
    .style(Style::default().fg(Color::White));

    f.render_widget(path_text, chunks[0]);

    // Middle: split into two columns
    let mid_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(chunks[1]);

    // Left: file tree
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
        .block(Block::default().borders(Borders::ALL).title(lang.t("panel.tree")))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

    f.render_widget(file_list, mid_chunks[0]);

    // Right: file info
    let info_text = if let Some(path) = app.get_selected_path() {
        // An unreadable metadata shows as "unknown": never invent a size or time
        let (size, modified) = match fs::metadata(&path) {
            Ok(meta) => (
                format_size(meta.len()),
                meta.modified()
                    .map(|t| {
                        let dt: chrono::DateTime<chrono::Local> = t.into();
                        dt.format("%Y-%m-%d %H:%M:%S").to_string()
                    })
                    .unwrap_or_else(|_| lang.t("value.unknown")),
            ),
            Err(_) => (
                lang.t("value.unknown"),
                lang.t("value.unknown"),
            ),
        };

        let label = |key: &str| Span::styled(format!("{} ", lang.t(key)), Style::default().fg(Color::Cyan));
        let kind = if path.is_dir() {
            lang.t("value.dir")
        } else {
            lang.t("value.file")
        };
        let lines = vec![
            Line::from(vec![
                label("info.name"),
                Span::styled(
                    path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                label("info.type"),
                Span::styled(kind, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                label("info.size"),
                Span::styled(size, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                label("info.mtime"),
                Span::styled(modified, Style::default().fg(Color::White)),
            ]),
            Line::from(""),
            Line::from(vec![
                label("info.full_path"),
                Span::styled(path.display().to_string(), Style::default().fg(Color::Yellow)),
            ]),
        ];
        Paragraph::new(lines)
    } else {
        Paragraph::new(lang.t("info.empty"))
    }
    .block(Block::default().borders(Borders::ALL).title(lang.t("panel.info")))
    .wrap(Wrap { trim: true });

    f.render_widget(info_text, mid_chunks[1]);

    // Bottom: key hints. When a single row runs out of space the lower-priority
    // navigation hints get dropped (see key_hint_line)
    let key_hint = Paragraph::new(key_hint_line(lang, chunks[2].width as usize))
        .block(Block::default().borders(Borders::ALL).title(lang.t("panel.keys")));

    f.render_widget(key_hint, chunks[2]);

    // Bottom: progress / status, owning its own row so it never overlaps the
    // widgets above
    let gauge = match app.job.as_ref() {
        Some(job) => {
            let name = job
                .target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let secs = job.started.elapsed().as_secs().to_string();
            // The detected format leads the label, so the user can see what the
            // magic bytes turned out to be
            let text = if job.kind.is_extract() {
                lang.tf("status.extracting", &[&job.format, &name, &secs])
            } else {
                lang.tf("status.compressing", &[&job.format, &name, &secs])
            };
            Gauge::default()
                .gauge_style(Style::default().fg(Color::Cyan).bg(Color::DarkGray))
                .style(Style::default().bg(Color::DarkGray))
                .label(Span::styled(text, Style::default().fg(Color::White)))
                .ratio(f64::from(job.progress) / 100.0)
        }
        None => {
            // With a recent output, assemble the label against the real width;
            // when it does not fit, elide the path to  drive:\...name.ext
            let text = match app.last_out.as_ref() {
                Some(o) => {
                    let area = chunks[3];
                    let head = format!("{} {}: ", lang.t("status.label"), o.prefix);
                    let tail = format!("  {} {}", lang.t("status.size_field"), o.size);
                    let fixed = display_width(&head) + display_width(&tail);
                    let budget = (area.width as usize).saturating_sub(fixed);
                    let path = elide_path(&o.path, budget);
                    // Truncate once more when the fixed parts alone are already
                    // too wide (an unusually long size string, say)
                    truncate_to_width(&format!("{}{}{}", head, path, tail), area.width as usize)
                }
                // Plain status text, which can be an extraction message naming a
                // long archive and a long directory: truncate on display width so
                // it can never spill out of the row
                None => truncate_to_width(
                    &format!("{} {}", lang.t("status.label"), app.status),
                    chunks[3].width as usize,
                ),
            };
            Gauge::default()
                .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
                .style(Style::default().bg(Color::DarkGray))
                .label(Span::styled(text, Style::default().fg(Color::White)))
                .ratio(0.0)
        }
    };

    f.render_widget(gauge, chunks[3]);

    // The dialog is drawn last: dim the whole screen, then stack the centered
    // confirmation on top
    if let Some(dialog) = app.dialog.as_ref() {
        render_dialog(f, lang, dialog);
    }
}

/// Key hint row. The compression keys, `d` and `e` always stay; the navigation
/// hints get dropped in order of importance, which keeps everything inside one
/// row. The render order always follows the order of the table below
fn key_hint_line(lang: &Lang, width: usize) -> Line<'static> {
    // " <key>:<label> " chip; the key letter and the padding are layout, not copy
    let chip = |key: &str, label: &str, style: Style| {
        Span::styled(format!(" {key}:{} ", lang.t(label)), style)
    };
    // (tier, group): a lower tier claims space first, and a group that does not
    // fit is skipped whole
    let groups: [(u8, Vec<Span<'static>>); 5] = [
        (
            0,
            vec![
                Span::styled(" z:tar ", Style::default().fg(Color::Cyan)),
                Span::styled(" x:zip ", Style::default().fg(Color::Green)),
                Span::styled(" c:wim ", Style::default().fg(Color::Yellow)),
                Span::styled(" v:7z ", Style::default().fg(Color::Magenta)),
                Span::styled(" b:zst ", Style::default().fg(Color::Red)),
                Span::styled(" n:gz ", Style::default().fg(Color::Blue)),
                Span::styled(" m:xz ", Style::default().fg(Color::LightRed)),
            ],
        ),
        (
            0,
            // `d` and `e` act on the selection and share one group: a group is
            // kept or dropped whole, so the extract hint can never appear
            // without the delete hint beside it. The pipe is a layout separator
            // between this group and the format chips, not copy
            vec![
                Span::styled(
                    format!(" | d:{} ", lang.t("key.delete")),
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                chip("e", "key.extract", Style::default().fg(Color::Green)),
            ],
        ),
        (
            1,
            vec![chip("Enter", "key.enter", Style::default().fg(Color::White))],
        ),
        (
            2,
            vec![chip("q", "key.quit", Style::default().fg(Color::White))],
        ),
        (
            3,
            vec![chip("Backspace", "key.back", Style::default().fg(Color::White))],
        ),
    ];

    // The border eats two columns
    let budget = width.saturating_sub(2);
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&i| groups[i].0);

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for i in order {
        let (_, group) = &groups[i];
        let w: usize = group.iter().map(|s| display_width(&s.content)).sum();
        if used + w <= budget {
            used += w;
            spans.extend(group.iter().cloned());
        }
    }
    // When the terminal is too narrow for even one group, keep the smallest
    // possible delete hint
    if spans.is_empty() {
        let letter = lang.t("key.delete_tiny");
        for text in [format!(" {} ", letter), letter] {
            if display_width(&text) <= budget {
                spans.push(Span::styled(
                    text,
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ));
                break;
            }
        }
    }
    Line::from(spans)
}

/// Center a box of the given size inside `area`; the size is clamped to the
/// area first, so it can never spill off screen
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Dialog mask: keeps the glyph shapes but flattens the foreground to dark grey
/// over a black background, so the screen behind reads as a shadow and focus
/// clearly belongs to the dialog
fn dim_underlay(f: &mut Frame) {
    let area = f.area();
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_fg(Color::DarkGray).set_bg(Color::Black);
            }
        }
    }
}

/// Delete confirmation dialog. The action is irreversible, so: the full target
/// path is shown (never elided), the wording distinguishes file from directory,
/// and both ways out (Enter / Esc) are spelled out inside the box
fn render_dialog(f: &mut Frame, lang: &Lang, dialog: &Dialog) {
    dim_underlay(f);

    let target = dialog.target.display().to_string();
    let (icon, danger, question) = if dialog.is_dir {
        (
            "📁",
            lang.t("dialog.danger_dir"),
            lang.t("dialog.question_dir"),
        )
    } else {
        (
            "📄",
            lang.t("dialog.danger_file"),
            lang.t("dialog.question_file"),
        )
    };

    // Clamp the width to 24..=66 columns, then derive how many rows the path
    // actually needs from its real width
    let area = f.area();
    let w = area.width.min(66).max(area.width.min(24));
    // Usable width for the path: minus 2 border columns and 4 columns of padding
    let inner_w = w.saturating_sub(6).max(1) as usize;
    // The path gets a full-width block; that is what decides the row count
    let path_rows = display_width(&target).div_ceil(inner_w).max(1);
    // 2 border rows + 2 rows of vertical padding + 6 rows for danger / question /
    // blank / target label / blank / keys
    let wanted_h = (10 + path_rows as u16).min(22);
    let popup = centered(area, w, wanted_h);

    // Clear wipes both the glyphs and the styles in this area, so no residue
    // from the screen below shows through the box
    f.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(Color::Indexed(235)))
        .title(Span::styled(
            format!(" {} ", lang.t("dialog.title")),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(popup).inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // danger notice
            Constraint::Length(1), // question
            Constraint::Length(1), // blank
            Constraint::Length(1), // target label
            Constraint::Min(1),    // target path, shown in full, wrapped
            Constraint::Length(1), // blank
            Constraint::Length(1), // key hints
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", icon), Style::default().fg(Color::Cyan)),
            Span::styled(
                danger,
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ])),
        rows[0],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            question,
            Style::default().fg(Color::White),
        )])),
        rows[1],
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            lang.t("dialog.target"),
            Style::default().fg(Color::Cyan),
        )])),
        rows[3],
    );

    // Full path: wrap rather than elide, because an elided path could point the
    // user at the wrong file
    f.render_widget(
        Paragraph::new(target)
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .wrap(Wrap { trim: true }),
        rows[4],
    );

    let key = |key: &str| format!(" {} ", lang.t(key));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                key("dialog.key_enter"),
                Style::default().fg(Color::White).bg(Color::Red),
            ),
            Span::styled(key("dialog.action_delete"), Style::default().fg(Color::Red)),
            Span::styled(
                key("dialog.key_esc"),
                Style::default().fg(Color::White).bg(Color::DarkGray),
            ),
            Span::styled(
                key("dialog.action_cancel"),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        rows[6],
    );
}

/// Columns a single character occupies: control characters take 0 columns,
/// ASCII / half-width takes 1, CJK and emoji take 2. A deliberate approximation,
/// not a full UAX#11 implementation
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

/// East Asian Wide / Fullwidth ranges plus the common emoji ranges
fn is_wide(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115f      // Hangul Jamo
        | 0x2e80..=0x303e    // CJK radicals, punctuation
        | 0x3041..=0x33ff    // Kana, phonetic extensions, CJK compatibility
        | 0x3400..=0x4dbf    // CJK extension A
        | 0x4e00..=0x9fff    // CJK unified ideographs
        | 0xa000..=0xa4cf    // Yi
        | 0xac00..=0xd7a3    // Hangul syllables
        | 0xf900..=0xfaff    // CJK compatibility ideographs
        | 0xfe10..=0xfe19    // vertical forms
        | 0xfe30..=0xfe6f    // CJK compatibility forms
        | 0xff00..=0xff60    // fullwidth ASCII
        | 0xffe0..=0xffe6    // fullwidth signs
        | 0x1f300..=0x1f64f  // misc symbols and pictographs, emoji
        | 0x1f680..=0x1f6ff  // transport and map symbols
        | 0x1f900..=0x1f9ff  // supplemental symbols and pictographs
        | 0x20000..=0x3fffd  // CJK extension B and beyond
    )
}

/// Estimate how many terminal columns a string takes
pub(crate) fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Hard-truncate to a display width, guaranteeing the result never exceeds
/// `max_width` columns
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

/// The root part of a path: a drive letter plus separator, or, without one,
/// everything up to the first separator
fn path_root(path: &Path) -> String {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();

    // Drive letter + separator, e.g. D:\
    if bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let sep = bytes[2];
        if sep == b'\\' || sep == b'/' {
            return s[..3].to_string();
        }
    }

    // Otherwise take everything through the first separator
    if let Some(i) = s.find(['\\', '/']) {
        s[..=i].to_string()
    } else {
        String::new()
    }
}

/// Elide an over-wide path to `root + "..." + file_name`.
/// Core invariant: the returned display width never exceeds `max_width`,
/// whatever the input
pub(crate) fn elide_path(path: &Path, max_width: usize) -> String {
    let full = path.to_string_lossy().to_string();
    if display_width(&full) <= max_width {
        return full;
    }

    // No file name (the path ends in a separator, say): truncate the original
    let Some(file_name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return truncate_to_width(&full, max_width);
    };

    let root = path_root(path);
    let (stem, ext) = match file_name.rfind('.') {
        Some(i) if i > 0 => (&file_name[..i], &file_name[i..]),
        _ => (file_name.as_str(), ""),
    };

    // Shorten the file name step by step; the extension is preserved first
    let fixed_width = display_width(&root) + 3;
    let keep = max_width.saturating_sub(fixed_width);
    let out = if display_width(stem) <= keep {
        format!("{}...{}{}", root, stem, ext)
    } else {
        // Drop file name characters when even the extension no longer fits
        let stem = truncate_to_width(stem, keep.saturating_sub(display_width(ext)));
        format!("{}...{}{}", root, stem, ext)
    };
    // Root + "..." + extension can itself be too wide (a long root, say), so
    // clamp one last time
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
    use std::path::PathBuf;

    use ratatui::{backend::TestBackend, Terminal};

    use super::*;
    use crate::{
        app::{Dialog, DialogAction},
        i18n::Lang,
    };

    /// Every language the shipped config offers
    const LANGS: [&str; 3] = ["zh-cn", "zh-tw", "en-us"];

    /// Render one frame and stitch the glyphs into plain text for assertions.
    /// A wide character occupies two cells and the second one is reset to a
    /// space, so skipping it is what restores the original text
    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui(f, app)).unwrap();
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..h {
            let mut filler = false;
            for x in 0..w {
                let symbol = buf.cell(Position::new(x, y)).unwrap().symbol();
                if filler {
                    filler = false;
                    continue;
                }
                out.push_str(symbol);
                filler = display_width(symbol) == 2;
            }
            out.push('\n');
        }
        out
    }

    /// Built-in copy switched to `tag`, which never touches the filesystem
    fn lang(tag: &str) -> Lang {
        let mut lang = Lang::builtin();
        lang.current = tag.to_string();
        lang
    }

    fn with_dialog_in(lang: &Lang, target: &str, is_dir: bool) -> App {
        let mut app = App::new(lang.clone());
        app.entries.clear();
        app.dialog = Some(Dialog {
            action: DialogAction::Delete,
            target: PathBuf::from(target),
            is_dir,
        });
        app
    }

    #[test]
    fn key_hint_never_overflows_its_row() {
        // English copy is where the hint is widest, so check every language
        for tag in LANGS {
            let lang = lang(tag);
            for width in [4usize, 10, 20, 40, 56, 70, 78, 80, 100, 120, 200] {
                let line = key_hint_line(&lang, width);
                let w: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
                // The border takes two columns; the hints must fit inside the row
                assert!(w <= width.saturating_sub(2), "{tag} width={width} actual={w}");
            }
            // On a wide terminal the compression keys, `d` and `e` are present
            let full = key_hint_line(&lang, 120);
            let text: String = full.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(text.contains("z:tar"), "{tag}: {text}");
            assert!(
                text.contains(&format!("d:{}", lang.t("key.delete"))),
                "{tag}: {text}"
            );
            assert!(
                text.contains(&format!("e:{}", lang.t("key.extract"))),
                "{tag}: {text}"
            );
        }
    }

    #[test]
    fn the_extract_key_is_never_shown_without_the_delete_key() {
        // Both act on the selection and sit in the same tier, so `d` claims the
        // space first and `e` is never left on its own
        for tag in LANGS {
            let lang = lang(tag);
            for width in 4usize..=140 {
                let text: String = key_hint_line(&lang, width)
                    .spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect();
                let has_d = text.contains(&format!("d:{}", lang.t("key.delete")));
                let has_e = text.contains(&format!("e:{}", lang.t("key.extract")));
                assert!(!has_e || has_d, "{tag} width={width}: {text}");
            }
        }
    }

    #[test]
    fn key_hint_falls_back_to_a_single_letter_when_tiny() {
        let lang = lang("en-us");
        // Budget of 3 columns still holds the padded chip, 1 column only the
        // bare letter, and nothing at all below that
        let text = |w: usize| {
            key_hint_line(&lang, w)
                .spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(text(5), " d ");
        assert_eq!(text(3), "d");
        assert_eq!(text(2), "");
        assert_eq!(text(0), "");
    }

    #[test]
    fn status_row_shows_the_localized_prefix_and_never_overflows() {
        for tag in LANGS {
            let lang = lang(tag);
            for (w, h) in [(20u16, 8u16), (40, 10), (60, 14), (80, 24), (200, 50)] {
                let mut app = App::new(lang.clone());
                app.last_out = Some(crate::app::OutputInfo {
                    prefix: lang.t("compress.zst.ok"),
                    path: PathBuf::from(r"D:\lazyarchive\lazyzip\BussinGriddyCode.zst"),
                    size: "1.25 MB".to_string(),
                });
                app.status = lang.t("status.ready");
                let screen = render(&mut app, w, h);
                let row = screen.lines().last().unwrap_or_default();
                assert!(
                    display_width(row.trim_end()) <= w as usize,
                    "{tag} {w}x{h}: {row:?}"
                );
                if w >= 60 {
                    assert!(
                        row.contains(&lang.t("status.label")),
                        "{tag} {w}x{h}: the status prefix should survive: {row:?}"
                    );
                    assert!(row.contains(&lang.t("status.size_field")), "{tag} {w}x{h}: {row:?}");
                }
            }
        }
    }

    /// Take one row from inside the dialog: strip the side borders and
    /// whitespace, counting the second cell of a wide character as whitespace
    fn box_line(row: &str) -> String {
        row.split('║')
            .nth(1)
            .unwrap_or("")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn dialog_names_the_kind_and_shows_the_full_path() {
        let long = r"D:\工作目录\很长的中文目录名\子目录\子目录\更深的子目录\报告.tar";
        for tag in LANGS {
            let lang = lang(tag);
            let screen = render(&mut with_dialog_in(&lang, long, true), 100, 24);

            assert!(
                screen.contains(&lang.t("dialog.question_dir")),
                "{tag}: a directory must be called a directory"
            );
            assert!(screen.contains("Enter") && screen.contains("Esc"), "{tag}: both exits must be spelled out");

            // A long path wraps: joining the rows must reproduce the path
            // exactly. One character short means truncation, one too many means
            // an ellipsis stood in for something
            let lines: Vec<String> = screen.lines().map(box_line).collect();
            let start = lines
                .iter()
                .position(|l| l.starts_with("D:\\"))
                .expect("the dialog should show the target path");
            let mut joined = String::new();
            for line in &lines[start..] {
                if line.is_empty() {
                    break; // end of the path block
                }
                joined.push_str(line);
                if joined.chars().count() >= long.chars().count() {
                    break;
                }
            }
            assert_eq!(joined, long, "{tag}: the path must wrap in full");
            // An elided form must never appear: it could point at the wrong file
            assert!(
                !screen.contains(&elide_path(Path::new(long), 40)),
                "{tag}: the dialog must not elide the path"
            );
        }
    }

    #[test]
    fn dialog_for_a_file_asks_about_a_file() {
        let short = r"D:\a\b.txt";
        for tag in LANGS {
            let lang = lang(tag);
            let screen = render(&mut with_dialog_in(&lang, short, false), 80, 20);
            assert!(
                screen.contains(&lang.t("dialog.question_file")),
                "{tag}: a file must be called a file"
            );
            assert!(screen.contains(short), "{tag}: {screen}");
        }
    }

    #[test]
    fn dialog_renders_on_tiny_terminals_without_panicking() {
        // The dialog size is clamped to the terminal; extreme sizes only have
        // to not crash, in any language
        for tag in LANGS {
            let lang = lang(tag);
            for (w, h) in [(12u16, 6u16), (20, 4), (30, 10), (200, 60)] {
                let mut app = with_dialog_in(&lang, r"D:\very\long\path\to\a\file.txt", false);
                let screen = render(&mut app, w, h);
                assert_eq!(screen.lines().count(), h as usize, "{tag} {w}x{h}: wrong row count");
            }
        }
    }

    #[test]
    fn width_counts_ascii_cjk_and_emoji_as_columns() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("状态"), 4);
        assert_eq!(display_width("📁"), 2);
        assert_eq!(display_width("📁 文件"), 2 + 1 + 4);
        // Control characters take no columns
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
        assert!(out.ends_with(".zst"), "the extension must survive: {out}");
        assert!(out.starts_with(r"D:\..."), "the drive root must survive: {out}");
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
                    "too wide: path={:?} budget={} out={:?} width={}",
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
        // A CJK file name costs two columns per character, so the budget is in
        // columns, not in characters
        let p = Path::new(r"D:\目录\中文文件.tar.gz");
        let budget = 16;
        let out = elide_path(p, budget);
        assert!(display_width(&out) <= budget, "out={out:?}");
        assert!(out.ends_with(".gz"), "the extension must survive: {out}");

        let emoji = Path::new(r"D:\😀😀😀\a.zst");
        assert!(display_width(&emoji.to_string_lossy()) > emoji.to_string_lossy().chars().count());
    }

    #[test]
    fn degenerate_paths_do_not_panic() {
        // Trailing separator / no file name / bare root / empty path
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