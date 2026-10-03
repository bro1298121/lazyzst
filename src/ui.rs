use std::{fs, path::Path};

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Margin, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::{
    app::{App, Dialog, MarkedSummary, VISIBLE_ROWS},
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

    // Left: file tree. A leading `[x] ` / `[ ] ` column says at a glance which
    // entries a batch would take; it costs four columns, which the narrow left
    // pane can afford because the name simply gets clipped at the panel edge
    let marked = &app.marked;
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
            // The mark keeps its own colour even on the selected row, exactly
            // like the icon does: which entries are in the batch is the one thing
            // that must stay readable while the cursor is on it
            let (mark, mark_style) = if marked.contains(path) {
                ("[x] ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
            } else {
                ("[ ] ", Style::default().fg(Color::DarkGray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(mark, mark_style),
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
        // Walking a directory is expensive and the redraw runs on a timer, so
        // the measurement is kept until the selection moves elsewhere
        let size = match &app.size_cache {
            Some((cached, text)) if cached == &path => text.clone(),
            _ => {
                let text = size_of(&path, lang);
                app.size_cache = Some((path.clone(), text.clone()));
                text
            }
        };

        // An unreadable metadata shows as "unknown": never invent a size or time
        let modified = match fs::metadata(&path) {
            Ok(meta) => meta
                .modified()
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Local> = t.into();
                    dt.format("%Y-%m-%d %H:%M:%S").to_string()
                })
                .unwrap_or_else(|_| lang.t("value.unknown")),
            Err(_) => lang.t("value.unknown"),
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
    // widgets above. The mark counter is measured first and the row is split
    // around it, so the status text below is truncated against exactly the
    // columns the counter does not claim
    let counter = marked_counter(lang, app.marked_summary(), chunks[3].width as usize);
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            // Nothing marked means nothing claimed: the counter costs no columns
            Constraint::Length(counter.as_ref().map_or(0, |line| {
                line.spans.iter().map(|s| display_width(&s.content)).sum::<usize>() as u16
            })),
        ])
        .split(chunks[3]);

    let gauge = match app.job.as_ref() {
        Some(job) => {
            let name = &job.label;
            let secs = job.started.elapsed().as_secs().to_string();
            // The detected format leads the label, so the user can see what the
            // magic bytes turned out to be
            let text = if job.kind.is_extract() {
                lang.tf("status.extracting", &[&job.format, name, &secs])
            } else {
                lang.tf("status.compressing", &[&job.format, name, &secs])
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
                    let area = bottom[0];
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
                    bottom[0].width as usize,
                ),
            };
            Gauge::default()
                .gauge_style(Style::default().fg(Color::Green).bg(Color::DarkGray))
                .style(Style::default().bg(Color::DarkGray))
                .label(Span::styled(text, Style::default().fg(Color::White)))
                .ratio(0.0)
        }
    };

    f.render_widget(gauge, bottom[0]);

    if let Some(line) = counter {
        // Right-aligned against the screen edge, on the same background as the
        // Gauge so the row reads as one bar rather than two
        f.render_widget(
            Paragraph::new(line)
                .alignment(Alignment::Right)
                .style(Style::default().bg(Color::DarkGray)),
            bottom[1],
        );
    }

    // The dialog is drawn last: dim the whole screen, then stack the centered
    // confirmation on top
    if let Some(dialog) = app.dialog.as_ref() {
        render_dialog(f, lang, dialog);
    }
}

/// Columns the status text is guaranteed before the corner counter may compete
/// for the rest of the row.
///
/// The two share one line, so a counter that claims everything leaves a status
/// bar clipped down to its own label. What it says is the news; the count is
/// only an aid, so the status goes first
const MIN_STATUS_WIDTH: usize = 16;

/// The mark counter for the bottom-right corner, already cut down to what the row
/// can hold.
///
/// Three steps down and then silence: the count with its breakdown, the count
/// alone, and nothing. The row is a single line shared with the status bar, so a
/// counter that overflowed it would push the status out of the terminal; dropping
/// the breakdown first is what keeps the number itself on screen
fn marked_counter(lang: &Lang, summary: Option<MarkedSummary>, width: usize) -> Option<Line<'static>> {
    let summary = summary?;
    let count = Span::styled(
        summary.count(lang),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    );
    let detail = Span::styled(summary.detail(lang), Style::default().fg(Color::DarkGray));

    // Both halves are measured on display width, so a Chinese breakdown costs
    // twice as many columns as an English one
    let spare = width.saturating_sub(MIN_STATUS_WIDTH);
    let full = display_width(&count.content) + display_width(&detail.content);
    if full <= spare {
        Some(Line::from(vec![count, detail]))
    } else if display_width(&count.content) <= spare {
        Some(Line::from(vec![count]))
    } else {
        // Not even the count fits; say nothing rather than spill out of the row
        None
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
    let groups: [(u8, Vec<Span<'static>>); 6] = [
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
            // `Space` and `A` build up the batch a single key press then packs,
            // so they rank just below the format keys and `d` / `e`: they go
            // together or not at all
            vec![
                chip("Space", "key.mark", Style::default().fg(Color::Green)),
                chip("A", "key.mark_all", Style::default().fg(Color::Green)),
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

/// How many directory entries one size measurement will visit before giving up.
///
/// `ui` redraws on a timer, and a tree holding millions of files would turn a
/// size lookup into a visible stall. Stopping early leaves a lower bound, which
/// is labelled as one rather than passed off as a true total.
const SIZE_WALK_BUDGET: usize = 5_000;

/// Sum the lengths of every file under a directory.
///
/// A directory handle carries no usable size of its own: on Windows its `len()`
/// comes back as a fixed meaningless value (a folder holding 5 MB reads back as
/// 1), so a directory can only be sized by walking it. Symlinks are not
/// followed, which keeps a link pointing back up the tree from looping or from
/// counting the same bytes twice.
///
/// Returns the total and whether the walk finished inside the budget.
fn directory_size(path: &Path, budget: usize) -> (u64, bool) {
    let mut total: u64 = 0;
    let mut files = 0usize;

    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        // A broken symlink or a vanished file is not worth failing the whole
        // measurement over; skip it and keep what has been counted
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        if let Ok(meta) = entry.metadata() {
            // `saturating_add` because a total past u64 would otherwise wrap
            // around to a small number, which is the very bug this replaces
            total = total.saturating_add(meta.len());
        }
        files += 1;
        if files >= budget {
            return (total, false);
        }
    }
    (total, true)
}

/// Human-readable size of `path`: its own length for a file, the summed
/// contents for a directory.
pub(crate) fn size_of(path: &Path, lang: &Lang) -> String {
    size_of_budgeted(path, SIZE_WALK_BUDGET, lang)
}

/// `size_of` with an explicit walk budget. Splitting it out is what lets the
/// tests drive the truncated path without building a tree of 50 000 files.
fn size_of_budgeted(path: &Path, budget: usize, lang: &Lang) -> String {
    match fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let (total, complete) = directory_size(path, budget);
            let text = format_size(total);
            if complete {
                text
            } else {
                // Say that this is a floor rather than let a partial sum read
                // as the whole
                lang.tf("value.size_partial", &[&text])
            }
        }
        Ok(meta) => format_size(meta.len()),
        Err(_) => lang.t("value.unknown"),
    }
}

/// Scale a byte count into the largest unit that keeps it readable.
///
/// Steps up through PB as well, so a figure far past the terabytes still reads
/// as a number with a sane exponent instead of an unwieldy digit string.
pub(crate) fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;
    const PB: u64 = TB * 1024;

    if bytes >= PB {
        format!("{:.2} PB", bytes as f64 / PB as f64)
    } else if bytes >= TB {
        format!("{:.2} TB", bytes as f64 / TB as f64)
    } else if bytes >= GB {
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

    /// A temporary directory that removes itself on drop, whether the test
    /// passes or panics. Named off the process id so parallel runs cannot
    /// collide with each other
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lazyzst-uisize-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Two files, the first one marked. Nothing touches the disk: the marks are
    /// absolute paths, so they only have to match the listing
    fn app_with_marks(lang: &Lang) -> App {
        let mut app = App::new(lang.clone());
        app.entries = vec![PathBuf::from("alpha.txt"), PathBuf::from("beta.txt")];
        app.selected = 0;
        app.marked = vec![PathBuf::from("alpha.txt")];
        app
    }

    /// Columns inside the left panel's border, laid out exactly as `ui` does it.
    /// Reading the real layout instead of hand-counting keeps the assertion from
    /// drifting away from the widget it is checking
    fn inner_width(w: u16) -> usize {
        let area = Rect::new(0, 0, w, 24);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(area);
        let mid = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(rows[1]);
        (mid[0].width.saturating_sub(2)) as usize
    }

    /// The vertical rule the panels are drawn with, and the doubled one the
    /// dialog uses. Written as escapes so the test reads in plain ASCII
    const BORDER: char = '\u{2502}';
    const DOUBLE_BORDER: char = '\u{2551}';

    /// What sits inside the left panel's border on every row, leading border
    /// stepped over. Counting columns rather than splitting on the border glyph
    /// is what keeps this correct in the presence of two-column characters
    fn panel_rows(screen: &str, w: u16, h: u16) -> Vec<String> {
        let inner = inner_width(w);
        screen
            .lines()
            .take(h as usize)
            .filter_map(|line| {
                let inside = line.strip_prefix(BORDER)?;
                let mut cell = String::new();
                let mut used = 0;
                for c in inside.chars() {
                    if used >= inner || matches!(c, BORDER | DOUBLE_BORDER) {
                        break;
                    }
                    used += char_width(c);
                    cell.push(c);
                }
                Some(cell.trim_end().to_string())
            })
            .collect()
    }

    /// The file rows of a rendered screen: the ones carrying a mark column
    fn marked_rows(screen: &str, w: u16, h: u16) -> Vec<String> {
        panel_rows(screen, w, h)
            .into_iter()
            .filter(|cell| cell.starts_with("[x] ") || cell.starts_with("[ ] "))
            .collect()
    }

    #[test]
    fn key_hint_never_overflows_its_row() {
        // English copy is where the hint is widest, so check every language
        for tag in LANGS {
            let lang = lang(tag);
            // Every single width, not a sample: the mark chips widened tier 0 by
            // about twenty columns, so the exact drop point moved
            for width in 4usize..=200 {
                let line = key_hint_line(&lang, width);
                let w: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
                // The border takes two columns; the hints must fit inside the row
                assert!(w <= width.saturating_sub(2), "{tag} width={width} actual={w}");
            }
            // On a wide terminal the compression keys, the mark keys, `d` and `e`
            // are all present
            let full = key_hint_line(&lang, 200);
            let text: String = full.spans.iter().map(|s| s.content.to_string()).collect();
            assert!(text.contains("z:tar"), "{tag}: {text}");
            assert!(text.contains("x:zip"), "{tag}: {text}");
            assert!(
                text.contains(&format!("Space:{}", lang.t("key.mark"))),
                "{tag}: {text}"
            );
            assert!(
                text.contains(&format!("A:{}", lang.t("key.mark_all"))),
                "{tag}: {text}"
            );
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
    fn the_mark_keys_come_and_go_together() {
        // `Space` alone cannot build a batch without `A`, so they share one group
        // and are dropped whole, exactly as `d` and `e` are
        for tag in LANGS {
            let lang = lang(tag);
            for width in 4usize..=200 {
                let text: String = key_hint_line(&lang, width)
                    .spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect();
                let has_space = text.contains(&format!("Space:{}", lang.t("key.mark")));
                let has_all = text.contains(&format!("A:{}", lang.t("key.mark_all")));
                assert!(has_space == has_all, "{tag} width={width}: {text}");
            }
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
    fn the_mark_column_costs_exactly_four_columns() {
        // The left pane is 40% of the terminal, so every column the mark takes is
        // a column off the file name. Measure it against the very same listing
        // with the marks taken away: the difference has to be four, and what is
        // left has to be the row that was there before
        for tag in LANGS {
            let lang = lang(tag);
            for w in [60u16, 80, 120] {
                let mut marked_app = app_with_marks(&lang);
                let mut unmarked_app = app_with_marks(&lang);
                unmarked_app.marked.clear();

                let marked = marked_rows(&render(&mut marked_app, w, 24), w, 24);
                // The same rows with the mark column lifted off, which is
                // exactly what the listing looked like before marks existed
                let plain: Vec<String> = marked_rows(&render(&mut unmarked_app, w, 24), w, 24)
                    .into_iter()
                    .map(|row| {
                        row.strip_prefix("[x] ")
                            .or_else(|| row.strip_prefix("[ ] "))
                            .unwrap_or(row.as_str())
                            .to_string()
                    })
                    .collect();

                // Both entries are listed, the marked one first, and the file
                // name still has room at these widths
                assert_eq!(marked, vec!["[x] \u{1f4c4} alpha.txt", "[ ] \u{1f4c4} beta.txt"], "{tag} width={w}");
                assert_eq!(plain, vec!["\u{1f4c4} alpha.txt", "\u{1f4c4} beta.txt"], "{tag} width={w}");
                // Four columns, no more and no less, on every row
                for (row, before) in marked.iter().zip(&plain) {
                    assert_eq!(
                        display_width(row),
                        display_width(before) + 4,
                        "{tag} width={w}: {row:?} against {before:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_narrow_panel_clips_the_name_and_never_the_mark() {
        // Once the pane is too narrow for the name, the name is what goes: the
        // mark column and the icon stay, because they are the only way to tell
        // which entries a batch would take
        for tag in LANGS {
            let lang = lang(tag);
            for w in [24u16, 40] {
                let mut app = app_with_marks(&lang);
                let screen = render(&mut app, w, 24);
                let rows = marked_rows(&screen, w, 24);
                assert_eq!(rows.len(), 2, "{tag} width={w}: {rows:?}");
                for (row, full) in rows.iter().zip(["[x] \u{1f4c4} alpha.txt", "[ ] \u{1f4c4} beta.txt"]) {
                    assert!(
                        row.starts_with("[x] ") || row.starts_with("[ ] "),
                        "{tag} width={w}: the mark must survive: {row:?}"
                    );
                    assert!(
                        full.starts_with(row.as_str()),
                        "{tag} width={w}: clipping must cut from the right: {row:?}"
                    );
                    assert!(
                        display_width(row) <= inner_width(w),
                        "{tag} width={w}: the row spilled past the panel: {row:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_long_name_is_clipped_at_the_panel_edge_and_nowhere_else() {
        // The name is allowed to run out of room: the panel clips it. What must
        // never happen is a row spilling past the border, which would paint over
        // the panel next to it
        let lang = lang("en-us");
        let mut app = App::new(lang.clone());
        app.entries = vec![PathBuf::from(format!("{}.txt", "n".repeat(200)))];
        app.marked = app.entries.clone();

        for w in [8u16, 20, 40, 80] {
            let screen = render(&mut app, w, 20);
            assert_eq!(screen.lines().count(), 20, "width={w}: wrong row count");
            // Nothing paints past the terminal's own last column, whatever the
            // width
            for (i, line) in screen.lines().enumerate() {
                assert!(display_width(line) <= w as usize, "width={w} row{i}: {line:?}");
            }
            // Below four columns of pane there is no mark column left to check,
            // but the row still has to fit inside the border
            if inner_width(w) < 4 {
                continue;
            }
            let rows = marked_rows(&screen, w, 20);
            let row = rows.first().unwrap_or_else(|| panic!("width={w}: no row"));
            // The mark leads the row, so it is never the part that gets clipped
            assert!(row.starts_with("[x] "), "width={w}: {row:?}");
            assert!(
                display_width(row) <= inner_width(w),
                "width={w}: the row spilled past the panel: {row:?}"
            );
        }
    }

    #[test]
    fn the_corner_counter_degrades_in_three_steps() {
        let lang = lang("en-us");
        let summary = MarkedSummary {
            total: 12,
            files: 10,
            dirs: 2,
        };
        let count = summary.count(&lang);
        let detail = summary.detail(&lang);

        // Wide enough for both halves: the full phrase, split into two spans so
        // the count and the breakdown can be styled differently
        let full = marked_counter(&lang, Some(summary), 200).expect("wide enough");
        let spans = &full.spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, count);
        assert_eq!(spans[1].content, detail);

        // Enough for the count alone: the breakdown goes and the number stays,
        // because the number is what the user is actually counting. The status
        // keeps its columns either way, which is what the extra width is for
        let just_the_count = display_width(&count) + MIN_STATUS_WIDTH;
        let short = marked_counter(&lang, Some(summary), just_the_count).expect("count fits");
        assert_eq!(short.spans.len(), 1);
        assert_eq!(short.spans[0].content, count);

        // One column short of that: say nothing at all rather than spill out
        assert!(marked_counter(&lang, Some(summary), just_the_count - 1).is_none());
        assert!(marked_counter(&lang, Some(summary), 0).is_none());

        // Nothing marked: no counter, so the row stays exactly as wide as it was
        assert!(marked_counter(&lang, None, 200).is_none());
    }

    #[test]
    fn the_corner_counter_never_starves_the_status_bar() {
        // The counter is an aid, the status is the news: a row too narrow for
        // both has to give up the breakdown, and then the count
        let lang = lang("zh-cn");
        let summary = MarkedSummary { total: 3, files: 3, dirs: 0 };
        let breakdown = display_width(&summary.detail(&lang));

        for row in 0usize..=120 {
            let line = marked_counter(&lang, Some(summary), row);
            let used: usize = line
                .as_ref()
                .map_or(0, |l| l.spans.iter().map(|s| display_width(&s.content)).sum());
            assert!(used <= row, "width={row}: used={used}");

            // Whatever is kept leaves the status its own columns
            let halves = line.map_or(0, |l| l.spans.len());
            assert!(
                halves == 0 || row - used >= MIN_STATUS_WIDTH,
                "width={row}: the counter left the status {} columns",
                row - used
            );
            // The breakdown is the first thing to go: a row that only holds the
            // count keeps the count and nothing else
            assert!(
                halves != 2 || display_width(&summary.count(&lang)) + breakdown <= row - MIN_STATUS_WIDTH,
                "width={row}: the breakdown was kept when the count alone would have done"
            );
        }
    }

    #[test]
    fn the_corner_counter_never_overflows_the_bottom_row() {
        for tag in LANGS {
            let lang = lang(tag);
            let summary = MarkedSummary {
                total: 1234,
                files: 1200,
                dirs: 34,
            };
            for w in 0usize..=120 {
                if let Some(line) = marked_counter(&lang, Some(summary), w) {
                    let used: usize = line.spans.iter().map(|s| display_width(&s.content)).sum();
                    assert!(used <= w, "{tag} width={w}: used={used}");
                }
            }
        }
    }

    #[test]
    fn the_corner_counter_shares_the_row_with_the_status_without_spilling() {
        for tag in LANGS {
            let lang = lang(tag);
            for w in [12u16, 24, 40, 60, 80, 120, 200] {
                let mut app = app_with_marks(&lang);
                // A long status, which is what the row has to survive sharing
                app.status = lang.tf("status.deleted", &["File", r"D:\a\b\c.txt"]);
                let screen = render(&mut app, w, 12);
                let row = screen.lines().last().unwrap_or_default();
                assert!(
                    display_width(row.trim_end()) <= w as usize,
                    "{tag} width={w}: {row:?}"
                );
            }

            // At a comfortable width the whole phrase is there, flush against the
            // right edge, with the breakdown last
            let mut app = app_with_marks(&lang);
            let detail = app.marked_summary().expect("one mark").detail(&lang);
            let screen = render(&mut app, 200, 12);
            let row = screen.lines().last().unwrap_or_default();
            assert!(row.trim_end().ends_with(&detail), "{tag}: {row:?}");
        }
    }

    #[test]
    fn an_unmarked_run_shows_no_counter_at_all() {
        // Zero marked entries must cost nothing: no digits, no label, and the
        // whole row belongs to the status bar
        for tag in LANGS {
            let lang = lang(tag);
            let mut app = App::new(lang.clone());
            app.status = lang.t("status.ready");
            let screen = render(&mut app, 80, 12);
            let row = screen.lines().last().unwrap_or_default();
            assert!(!row.contains("Marked"), "{tag}: {row:?}");
            assert!(!row.contains("已标记"), "{tag}: {row:?}");
            assert!(!row.contains("已標記"), "{tag}: {row:?}");
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
    fn sizes_scale_through_terabytes_and_petabytes() {
        const KB: u64 = 1024;
        const MB: u64 = KB * 1024;
        const GB: u64 = MB * 1024;
        const TB: u64 = GB * 1024;
        const PB: u64 = TB * 1024;

        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1), "1 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(KB), "1.00 KB");
        assert_eq!(format_size(MB), "1.00 MB");
        assert_eq!(format_size(GB), "1.00 GB");
        // The units the report asked for: a terabyte is a real figure again,
        // not a wall of digits
        assert_eq!(format_size(TB), "1.00 TB");
        assert_eq!(format_size(1500 * TB), "1.46 PB");
        // ... and past that it keeps stepping up instead of overflowing
        assert_eq!(format_size(PB), "1.00 PB");
        // The largest representable total still renders as a number with a
        // sane exponent instead of overflowing into nonsense
        assert!(format_size(u64::MAX).ends_with(" PB"), "{}", format_size(u64::MAX));
    }

    #[test]
    fn a_terabyte_of_real_bytes_is_not_reported_as_zero() {
        // A total past the terabytes must never wrap into a small number; the
        // old fixed-unit ladder had nowhere to go past the gigabytes
        let huge = 5_000u64 * 1024 * 1024 * 1024 * 1024;
        let shown = format_size(huge);
        assert!(shown.ends_with(" PB"), "{shown}");
        // 5000 TB is 4.88 PB, and crucially still a multi-digit total rather
        // than something that looks like an empty folder
        assert!(shown.starts_with("4.88 PB"), "{shown}");
    }

    #[test]
    fn a_directory_reports_the_size_of_what_it_contains() {
        // Windows hands back a meaningless length for a directory handle, so a
        // directory has to be walked; the file itself must not be trusted
        let dir = scratch("size");
        fs::create_dir_all(dir.0.join("nested")).unwrap();
        fs::write(dir.0.join("a.bin"), vec![0u8; 4096]).unwrap();
        fs::write(dir.0.join("nested/b.bin"), vec![0u8; 8192]).unwrap();

        let lang = Lang::builtin();
        let shown = size_of(&dir.0, &lang);
        // 4096 + 8192 = 12288 bytes, i.e. exactly 12.00 KB, counted across the
        // nested directory rather than taken from the folder's own handle
        assert_eq!(shown, "12.00 KB");

        // A plain file still reports its own length
        assert_eq!(size_of(&dir.0.join("a.bin"), &lang), "4.00 KB");
    }

    #[test]
    fn an_empty_directory_is_zero_and_a_missing_one_is_unknown() {
        let dir = scratch("size-empty");
        let lang = Lang::builtin();
        fs::create_dir_all(dir.0.join("void")).unwrap();

        assert_eq!(size_of(&dir.0.join("void"), &lang), "0 B");
        assert_eq!(size_of(&dir.0.join("nope"), &lang), lang.t("value.unknown"));
    }

    #[test]
    fn a_walk_that_hits_its_budget_is_marked_as_a_lower_bound() {
        let dir = scratch("size-budget");
        fs::create_dir_all(&dir.0).unwrap();
        for i in 0..10 {
            fs::write(dir.0.join(format!("f{i}.bin")), vec![0u8; 16]).unwrap();
        }
        let lang = Lang::builtin();

        // Enough budget to finish: a plain total
        assert_eq!(directory_size(&dir.0, 100), (160, true));
        // Budget of zero stops before the first file, and must say so rather
        // than present an empty total as the real size
        let (total, complete) = directory_size(&dir.0, 0);
        assert!(!complete, "a truncated walk must not claim to be complete");
        assert!(total <= 160);
        assert!(size_of_budgeted(&dir.0, 0, &lang).ends_with('+'));
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