//! The glyph that leads every row of the file tree.
//!
//! A terminal running a Nerd Font gets a glyph that names the file type; every
//! other terminal gets the plain emoji the listing has always used. The
//! fallback is not decoration: a private-use codepoint in a font with no glyph
//! for it is an empty box, which tells the user less than a folder emoji does.
//!
//! Every glyph is written as a `\u{...}` escape rather than as the character
//! itself. They all live in the Unicode private use area, where the literal
//! character is invisible in review, and a stray copy-paste through a tool that
//! does not pass private-use code points through turns it into something else
//! entirely. The `nf-` name in each comment is the glyph's name in the Nerd
//! Fonts cheat sheet, which is how one checks a code point without a font.

use std::path::Path;

// ---- emoji fallback ----

/// Shown for a directory when the Nerd Font glyphs are switched off
pub(crate) const FOLDER_EMOJI: &str = "\u{1f4c1}";
/// Shown for a file when the Nerd Font glyphs are switched off
pub(crate) const FILE_EMOJI: &str = "\u{1f4c4}";

// ---- Nerd Font glyphs ----

const FOLDER: &str = "\u{e5ff}"; // nf-custom-folder
const CPP: &str = "\u{f0672}"; // nf-md-language_cpp
const CSHARP: &str = "\u{f031b}"; // nf-md-language_csharp
const DOCX: &str = "\u{f022c}"; // nf-md-file_word
const PPTX: &str = "\u{f0227}"; // nf-md-file_powerpoint
const XLSX: &str = "\u{f021b}"; // nf-md-file_excel
const TXT: &str = "\u{f0219}"; // nf-md-file_document
const RUST: &str = "\u{e7a8}"; // nf-dev-rust
const PYTHON: &str = "\u{e606}"; // nf-seti-python
const PDF: &str = "\u{f0226}"; // nf-md-file_pdf_box
const MARKDOWN: &str = "\u{e73e}"; // nf-dev-markdown
const JS: &str = "\u{e60c}"; // nf-seti-javascript
const TS: &str = "\u{e628}"; // nf-seti-typescript
const LUA: &str = "\u{e620}"; // nf-seti-lua
const JAVA: &str = "\u{e738}"; // nf-dev-java
const FILE: &str = "\u{f15b}"; // nf-fa-file

/// Every Nerd Font glyph the listing can draw, the folder included.
///
/// `ui` measures the width of a row against this list rather than against the
/// Unicode ranges, because a glyph that is not listed here would be budgeted at
/// one column and would quietly pull every file name one column to the left.
/// Adding a glyph to the mapping above means adding it here
pub(crate) const ALL: [&str; 16] = [
    FOLDER, CPP, CSHARP, DOCX, PPTX, XLSX, TXT, RUST, PYTHON, PDF, MARKDOWN, JS, TS, LUA, JAVA, FILE,
];

/// Whether `c` is one of the Nerd Font glyphs this listing draws
pub(crate) fn is_nerd_icon(c: char) -> bool {
    ALL.iter().any(|icon| icon.starts_with(c))
}

/// The glyph that leads the row for `path`.
///
/// A directory is asked first: its name is not what the row is about, and a
/// directory called `release.rs` is still a directory. A file is then keyed off
/// its extension, matched in lower case, so `.RS` and `.Rs` land on the same
/// glyph as `.rs`. Anything without an extension the table knows is a plain
/// file, which is the honest answer rather than a guess at its contents.
///
/// `use_nerd` switches the whole table off at once and hands back the emoji the
/// listing used before Nerd Fonts were an option
pub(crate) fn icon_for(path: &Path, use_nerd: bool) -> &'static str {
    if !use_nerd {
        return if path.is_dir() { FOLDER_EMOJI } else { FILE_EMOJI };
    }

    if path.is_dir() {
        return FOLDER;
    }

    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return FILE;
    };
    match ext.to_lowercase().as_str() {
        "cxx" | "cpp" => CPP,
        "cs" => CSHARP,
        "docx" => DOCX,
        "pptx" => PPTX,
        "xlsx" => XLSX,
        "txt" => TXT,
        "rs" => RUST,
        "py" => PYTHON,
        "pdf" => PDF,
        "md" => MARKDOWN,
        "js" => JS,
        "ts" => TS,
        "lua" => LUA,
        "java" => JAVA,
        _ => FILE,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A temporary directory that removes itself on drop, whether the test
    /// passes or panics. Named off the process id and a counter so parallel
    /// runs cannot collide
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(tag: &str) -> Scratch {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lazyzst-icons-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    #[test]
    fn every_extension_maps_to_its_own_glyph_in_any_case() {
        // The table is spelled in lower case and the lookup lower-cases first,
        // so a file named on Windows with `.RS` or `.TXT` gets the same glyph as
        // the all-lower-case name rather than the plain-file one
        let cases: [(&str, &str); 15] = [
            ("main.cxx", CPP),
            ("main.cpp", CPP),
            ("Main.CPP", CPP),
            ("Program.cs", CSHARP),
            ("program.CS", CSHARP),
            ("report.docx", DOCX),
            ("report.DocX", DOCX),
            ("deck.pptx", PPTX),
            ("deck.PPTX", PPTX),
            ("sheet.xlsx", XLSX),
            ("sheet.Xlsx", XLSX),
            ("notes.txt", TXT),
            ("notes.TXT", TXT),
            ("main.rs", RUST),
            ("main.RS", RUST),
        ];
        for (name, expected) in cases {
            assert_eq!(
                icon_for(Path::new(name), true),
                expected,
                "{name} got the wrong glyph"
            );
        }

        // The rest of the table, one representative name each
        let rest: [(&str, &str); 8] = [
            ("build.py", PYTHON),
            ("build.PY", PYTHON),
            ("paper.pdf", PDF),
            ("paper.Pdf", PDF),
            ("README.md", MARKDOWN),
            ("readme.MD", MARKDOWN),
            ("app.js", JS),
            ("app.Js", JS),
        ];
        for (name, expected) in rest {
            assert_eq!(icon_for(Path::new(name), true), expected, "{name}");
        }
        for (name, expected) in [
            ("types.ts", TS),
            ("types.TS", TS),
            ("init.lua", LUA),
            ("init.Lua", LUA),
            ("Main.java", JAVA),
            ("main.JAVA", JAVA),
        ] {
            assert_eq!(icon_for(Path::new(name), true), expected, "{name}");
        }
    }

    #[test]
    fn an_unknown_or_missing_extension_falls_back_to_the_plain_file_glyph() {
        // No extension at all, a dot-file (which has no stem to take one from),
        // and an extension the table has never heard of all land on the plain
        // file glyph: it is the honest answer rather than a guess
        for name in ["Makefile", "README", ".bashrc", "archive.7z", "blob.zst", "a."] {
            assert_eq!(
                icon_for(Path::new(name), true),
                FILE,
                "{name} should read as a plain file"
            );
        }
        // The extension has to be the last segment: `a.txt.tar` is a tarball
        assert_eq!(icon_for(Path::new("a.txt.tar"), true), FILE);
    }

    #[test]
    fn a_directory_gets_the_folder_glyph_whatever_it_is_called() {
        // The kind is decided before the name is looked at, so a directory whose
        // name carries a file extension is still a directory. Real directories,
        // because the answer comes from asking the filesystem
        let s = scratch("dir");
        for name in ["src", "release.rs", "docs.tar"] {
            std::fs::create_dir(s.0.join(name)).expect("create directory");
        }
        for name in ["src", "release.rs", "docs.tar"] {
            assert_eq!(icon_for(&s.0.join(name), true), FOLDER, "{name}");
        }
        // ... while a file carrying a file extension is not a directory
        std::fs::write(s.0.join("main.rs"), b"x").expect("write file");
        assert_eq!(icon_for(&s.0.join("main.rs"), true), RUST);
    }

    #[test]
    fn switching_the_icons_off_falls_back_to_emoji_for_every_entry() {
        // With the glyphs off, nothing about a file can change the answer except
        // whether it is a directory: that is the whole point of the fallback
        for name in ["main.rs", "main.RS", "notes.TXT", "paper.pdf", "Makefile"] {
            assert_eq!(icon_for(Path::new(name), false), FILE_EMOJI, "{name}");
        }

        // A directory is still a directory with the glyphs off, and still gets
        // the folder emoji rather than the file one
        let s = scratch("off");
        for name in ["src", "release.rs"] {
            std::fs::create_dir(s.0.join(name)).expect("create directory");
        }
        for name in ["src", "release.rs"] {
            assert_eq!(icon_for(&s.0.join(name), false), FOLDER_EMOJI, "{name}");
        }
    }

    #[test]
    fn the_width_list_covers_every_glyph_the_mapping_can_return() {
        // A glyph the width list does not know about is budgeted at one column
        // and drags every name one column to the left, so the two lists have to
        // be kept in step. `icon_for` only ever hands back one of these
        for name in [
            "main.rs", "main.cxx", "main.cpp", "Program.cs", "report.docx", "deck.pptx",
            "sheet.xlsx", "notes.txt", "build.py", "paper.pdf", "README.md", "app.js", "types.ts",
            "init.lua", "Main.java", "Makefile",
        ] {
            let glyph = icon_for(Path::new(name), true);
            let c = glyph.chars().next().expect("a glyph is one character");
            assert_eq!(glyph.chars().count(), 1, "{glyph} is not a single glyph");
            assert!(is_nerd_icon(c), "{name}: {glyph} is missing from the width list");
        }
        // ... and nothing that is not one of them is mistaken for a glyph
        assert!(is_nerd_icon(FOLDER.chars().next().unwrap()));
        for c in ['a', '1', '\u{4e2d}', '\u{1f4c4}', ' '] {
            assert!(!is_nerd_icon(c), "U+{:04X} is not a Nerd Font glyph", c as u32);
        }
    }
}
