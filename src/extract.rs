//! Extraction support: work out what a file really is from its magic bytes,
//! then build the command that unpacks it.
//!
//! Two rules shape everything in this module:
//!
//! * The extension is never trusted. A `.zip` holding gzip bytes is gzip, and a
//!   file with no extension at all is still recognizable.
//! * Nothing here may panic. The paths come from a directory listing, so an
//!   unreadable, empty, truncated or weirdly named file is normal input rather
//!   than a bug. That is why detection returns `Option` and there is no
//!   `unreachable!()`, unlike the fixed set of seven formats on the compress
//!   side.

use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Result;

use crate::i18n::Lang;

/// Bytes read from the head of a file. tar has no magic at offset 0 and is only
/// identifiable at offset 257, so every detection reads this far
const HEAD_LEN: usize = 262;
/// Offset of the `ustar` marker inside a tar header
const TAR_MAGIC_AT: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

/// What a file turned out to be.
///
/// The stream compressors appear twice: on their own they hold a single file,
/// and after the nesting probe they may turn out to hold a tar archive. Both
/// shapes are kept here because the UI names the format and the two need
/// different commands
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// Single file behind a stream compressor
    Gz,
    Xz,
    Zst,
    Bz2,
    /// A tar archive behind a stream compressor
    TarGz,
    TarXz,
    TarZst,
    TarBz2,
    /// A tar archive with no compression on top
    Tar,
    Zip,
    SevenZip,
    Rar,
    Cab,
    Wim,
}

impl Format {
    /// Name shown in the UI. Format names are proper nouns and stay as they are
    /// in every language
    pub(crate) fn label(self) -> &'static str {
        match self {
            Format::Gz => "gzip",
            Format::Xz => "xz",
            Format::Zst => "zstd",
            Format::Bz2 => "bzip2",
            Format::TarGz => "tar.gz",
            Format::TarXz => "tar.xz",
            Format::TarZst => "tar.zst",
            Format::TarBz2 => "tar.bz2",
            Format::Tar => "tar",
            Format::Zip => "zip",
            Format::SevenZip => "7z",
            Format::Rar => "rar",
            Format::Cab => "cab",
            Format::Wim => "wim",
        }
    }

    /// Whether extraction targets one produced file rather than a directory
    /// full of them
    pub(crate) fn is_single_file(self) -> bool {
        matches!(self, Format::Gz | Format::Xz | Format::Zst | Format::Bz2)
    }

    /// The suffix this format appends to the name of the file it wraps
    ///
    /// Only the single-file formats have one, and only theirs may be stripped
    /// again on the way out
    pub(crate) fn stream_ext(self) -> Option<&'static str> {
        match self {
            Format::Gz => Some("gz"),
            Format::Xz => Some("xz"),
            Format::Zst => Some("zst"),
            Format::Bz2 => Some("bz2"),
            _ => None,
        }
    }

    /// The failure copy key for this format, so the message can name what was
    /// actually detected
    pub(crate) fn fail_key(self) -> &'static str {
        match self {
            Format::Gz => "extract.fail.gz",
            Format::Xz => "extract.fail.xz",
            Format::Zst => "extract.fail.zst",
            Format::Bz2 => "extract.fail.bz2",
            Format::TarGz => "extract.fail.tar.gz",
            Format::TarXz => "extract.fail.tar.xz",
            Format::TarZst => "extract.fail.tar.zst",
            Format::TarBz2 => "extract.fail.tar.bz2",
            Format::Tar => "extract.fail.tar",
            Format::Zip => "extract.fail.zip",
            Format::SevenZip => "extract.fail.7z",
            Format::Rar => "extract.fail.rar",
            Format::Cab => "extract.fail.cab",
            Format::Wim => "extract.fail.wim",
        }
    }

    /// Tool that can write the decompressed stream to stdout without touching
    /// the archive. `-c` rather than `-d`: the probe must not modify the file
    /// it is inspecting, and `-d` also strips the extension in place
    fn peek_command(self) -> Option<(&'static str, &'static str)> {
        match self {
            Format::Gz => Some(("gzip", "-dc")),
            Format::Xz => Some(("xz", "-dc")),
            Format::Zst => Some(("zstd", "-dc")),
            Format::Bz2 => Some(("bzip2", "-dc")),
            _ => None,
        }
    }

    /// The tar-wrapped variant of a stream format, when one exists
    fn tar_variant(self) -> Option<Self> {
        match self {
            Format::Gz => Some(Format::TarGz),
            Format::Xz => Some(Format::TarXz),
            Format::Zst => Some(Format::TarZst),
            Format::Bz2 => Some(Format::TarBz2),
            _ => None,
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// `(offset, magic, format)`. The offset 0 entries come first so that tar,
/// which can only be identified at offset 257, is the last thing tried
const SIGNATURES: &[(usize, &[u8], Format)] = &[
    (0, &[0x1F, 0x8B], Format::Gz),
    (0, &[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00], Format::Xz),
    (0, &[0x28, 0xB5, 0x2F, 0xFD], Format::Zst),
    // Empty archive
    (0, &[0x50, 0x4B, 0x05, 0x06], Format::Zip),
    // Spanned archive
    (0, &[0x50, 0x4B, 0x07, 0x08], Format::Zip),
    // Local file header, the ordinary case
    (0, &[0x50, 0x4B, 0x03, 0x04], Format::Zip),
    (0, &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C], Format::SevenZip),
    (0, &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07], Format::Rar),
    (0, &[0x42, 0x5A, 0x68], Format::Bz2),
    (0, &[0x4D, 0x53, 0x57, 0x49, 0x4D, 0x00, 0x00, 0x00], Format::Wim),
    (0, &[0x4D, 0x53, 0x43, 0x46], Format::Cab),
    (TAR_MAGIC_AT, TAR_MAGIC, Format::Tar),
];

/// Identify a file from its leading bytes.
///
/// Returns `None` for anything unrecognized as well as for a file that cannot
/// be read at all: a missing file, a directory, an unreadable one, or one too
/// short to carry the marker being looked for. Never panics
pub(crate) fn detect_format(path: &Path) -> Option<Format> {
    // A pipe or a directory would either block in `File::open` or fail deep
    // inside it; `is_file` settles both before the open
    if !path.is_file() {
        return None;
    }
    let head = read_head(path, HEAD_LEN).ok()?;
    detect_in_head(&head)
}

/// Match a head buffer against the signature table
fn detect_in_head(head: &[u8]) -> Option<Format> {
    SIGNATURES.iter().find_map(|(at, magic, format)| {
        // `get` yields None for a short buffer, so an undersized file simply
        // matches nothing
        let slice = head.get(*at..at + magic.len())?;
        (slice == *magic).then_some(*format)
    })
}

/// Read up to `len` bytes from the start of a file.
///
/// A short read is normal, not an error: the caller decides whether it has
/// enough to work with
fn read_head(path: &Path, len: usize) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        match file.read(&mut buf[filled..])? {
            0 => break, // end of file
            n => filled += n,
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// Whether a decompressed stream starts with a tar header
fn tar_wrapped(head: &[u8]) -> bool {
    head.get(TAR_MAGIC_AT..TAR_MAGIC_AT + TAR_MAGIC.len()) == Some(TAR_MAGIC)
}

/// Refine a stream format by peeking inside it, so `.tar.gz` stops looking like
/// a lone gzip file.
///
/// The stream compressor is run with its output on a pipe and the first bytes
/// are inspected for a tar header, which is the only difference the magic bytes
/// cannot show. Always returns a usable format: a missing tool, a tool that
/// fails, or a stream that is simply not a tar all degrade to the single-file
/// variant rather than interrupting the user
pub(crate) fn probe_nested(path: &Path, format: Format) -> Format {
    let Some((program, flag)) = format.peek_command() else {
        return format;
    };
    if !path.is_file() {
        return format;
    }

    let mut cmd = Command::new(program);
    cmd.arg(flag).arg(path);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());

    // No decompressor installed: treat it as a single file
    let Ok(mut child) = cmd.spawn() else {
        return format;
    };
    let Some(mut pipe) = child.stdout.take() else {
        // Nothing to read from; still reap the process we just started
        let _ = child.kill();
        let _ = child.wait();
        return format;
    };

    let mut head: Vec<u8> = Vec::with_capacity(HEAD_LEN);
    let mut chunk = [0u8; 512];
    // A pipe hands over whatever happens to be buffered, so accumulate in a
    // loop until the marker range is covered or the stream ends. Never ask for
    // more than the marker range needs, so the child cannot fill the pipe and
    // block on a write we will not read
    while head.len() < HEAD_LEN {
        let want = (HEAD_LEN - head.len()).min(chunk.len());
        match pipe.read(&mut chunk[..want]) {
            Ok(0) => break, // the tool finished or the stream ended
            Ok(n) => head.extend_from_slice(&chunk[..n]),
            // A closed pipe means the tool died; there is nothing more to learn
            Err(_) => break,
        }
    }
    drop(pipe);

    // Stop after the peek: draining the whole stream would let the tool block
    // on a full pipe, and kill + wait leaves no orphan behind
    let _ = child.kill();
    let _ = child.wait();

    match format.tar_variant() {
        Some(tar) if tar_wrapped(&head) => tar,
        _ => format,
    }
}

/// Build the command that unpacks `path` into `dest`.
///
/// `lang` is needed because the one failure this function raises itself (a
/// single-file archive with no extension to strip) is user-facing copy
pub(crate) fn build_extract_command(
    lang: &Lang,
    format: Format,
    path: &Path,
    dest: &Path,
) -> Result<Command> {
    let arg = path.display().to_string();

    // Refuse rather than write the output over the archive itself: a name with
    // no extension to strip (or only a leading dot, as in `.gz`) has nowhere
    // else to go, and the tools would refuse too, only less clearly
    if format.is_single_file() && single_file_output(format, path).is_none() {
        anyhow::bail!("{}", lang.t("status.nothing_extracted"));
    }

    let (program, args): (&str, Vec<String>) = match format {
        Format::Tar => ("tar", vec!["-xf".to_string(), arg]),
        Format::TarGz => ("tar", vec!["-xzf".to_string(), arg]),
        Format::TarXz => ("tar", vec!["-xJf".to_string(), arg]),
        // `-x` has to be spelled out on these two: `--zstd` / `--bzip2` are
        // separate flags rather than single letters folded into `-xzf`, and
        // without an operation flag tar refuses with "Must specify one of -c,
        // -r, -t, -u, -x" and writes nothing at all
        Format::TarZst => (
            "tar",
            vec![
                "--zstd".to_string(),
                "-x".to_string(),
                "-f".to_string(),
                arg,
            ],
        ),
        Format::TarBz2 => (
            "tar",
            vec![
                "--bzip2".to_string(),
                "-x".to_string(),
                "-f".to_string(),
                arg,
            ],
        ),
        // bsdtar, the `tar` that ships with Windows, reads zip as well
        Format::Zip => ("tar", vec!["-xf".to_string(), arg]),
        // 7z handles both; `-y` answers the overwrite question for us, which
        // matters because the child's stdin is not a terminal
        Format::SevenZip | Format::Rar => ("7z", vec!["x".to_string(), "-y".to_string(), arg]),
        // `-F:` expands unconditionally; without it expand asks before
        // overwriting and would wait forever for an answer
        Format::Cab => ("expand", vec![format!("-F:{arg}")]),
        // Applying a WIM is the only way to get its contents out, and it needs
        // an elevated shell
        Format::Wim => (
            "dism",
            vec![
                "/Apply-Image".to_string(),
                format!("/ImageFile:{arg}"),
                format!("/Destination-Image:{}", dest.display()),
            ],
        ),
        // The decompressor rewrites the archive in place, leaving the payload
        // under its stripped name
        Format::Gz => ("gzip", vec!["-d".to_string(), "-f".to_string(), arg]),
        Format::Xz => ("xz", vec!["-d".to_string(), "-f".to_string(), arg]),
        Format::Zst => ("zstd", vec!["-d".to_string(), "-f".to_string(), arg]),
        Format::Bz2 => ("bzip2", vec!["-d".to_string(), "-f".to_string(), arg]),
    };

    let mut cmd = Command::new(program);
    cmd.args(args);
    // Unpack into the directory being browsed, the same place the compressor
    // writes its output
    cmd.current_dir(dest);
    // Nothing may reach the screen: the progress chatter from tar / 7z / dism
    // would land in the TUI and shred it
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    Ok(cmd)
}

/// Where an extraction ends up.
///
/// Archive formats fill a directory, so the directory itself is the result; a
/// single-file format produces exactly one file next to its archive
pub(crate) fn extract_target(format: Format, path: &Path, dest: &Path) -> PathBuf {
    if format.is_single_file() {
        // `build_extract_command` already rejected the case where this cannot
        // work, so the fallback here is never the one that runs
        return single_file_output(format, path).unwrap_or_else(|| dest.to_path_buf());
    }
    dest.to_path_buf()
}

/// The payload of a single-file archive: its name minus the suffix the format
/// appended.
///
/// `None` when that suffix is absent, which happens when the archive does not
/// carry it: a gzip stream named `notes.txt`, a `.gz` with no stem, or a
/// format whose own suffix is missing. Returning `None` makes the caller
/// refuse instead of guessing, because the alternatives are a self-overwrite
/// (stemless) or a name that silently drops the user's real extension.
fn single_file_output(format: Format, path: &Path) -> Option<PathBuf> {
    let suffix = format.stream_ext()?;
    // Compare the suffix the archive actually carries against the one this
    // format writes. A `.txt` on a gzip stream is the user's own extension, not
    // ours to strip, and removing it would rename their file behind their back
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(suffix))
    {
        return None;
    }
    let out = path.with_extension("");
    (out != path).then_some(out)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Removes the scratch directory on drop, whether the test passes or panics
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One directory per test: named with the process id plus a counter so
    /// parallel runs cannot collide; distinct prefix from the app.rs and i18n.rs
    /// guards
    fn scratch(tag: &str) -> Scratch {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lazyzst-extract-test-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// A zero-filled head with `magic` written at `at`
    fn head_with(at: usize, magic: &[u8]) -> Vec<u8> {
        let mut head = vec![0u8; HEAD_LEN];
        head[at..at + magic.len()].copy_from_slice(magic);
        head
    }

    /// Write `bytes` into the scratch directory under `name`
    fn file_in(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write fixture");
        path
    }

    #[test]
    fn every_signature_is_recognized() {
        for (at, magic, format) in SIGNATURES {
            let head = head_with(*at, magic);
            assert_eq!(detect_in_head(&head), Some(*format), "{format} magic not matched");
        }
    }

    #[test]
    fn magic_wins_over_a_lying_extension() {
        // A `.zip` that is really gzip data: the name must not decide
        let s = scratch("lying");
        let head = head_with(0, &[0x1F, 0x8B]);
        let path = file_in(&s.0, "archive.zip", &head);

        assert_eq!(detect_format(&path), Some(Format::Gz));
    }

    #[test]
    fn a_file_without_an_extension_is_still_recognized() {
        let s = scratch("noext");
        let head = head_with(0, &[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]);
        let path = file_in(&s.0, "no_extension_here", &head);

        assert_eq!(detect_format(&path), Some(Format::Xz));
    }

    #[test]
    fn tar_is_found_at_offset_257() {
        let s = scratch("tar");
        let head = head_with(TAR_MAGIC_AT, TAR_MAGIC);
        // The file name really is in the way at offset 0, exactly as a tar has it
        let path = file_in(&s.0, "plain.tar", &head);

        assert_eq!(detect_format(&path), Some(Format::Tar));
    }

    #[test]
    fn short_files_return_none_without_panicking() {
        let s = scratch("short");
        // Empty, two bytes, and one byte short of the tar marker range
        let empty = file_in(&s.0, "empty", &[]);
        let two = file_in(&s.0, "two", &[0x1F, 0x8B]);
        let almost = file_in(&s.0, "almost", &vec![0u8; HEAD_LEN - 1]);

        // The two-byte gzip magic is real but the tar range is not covered
        assert_eq!(detect_format(&empty), None);
        assert_eq!(detect_format(&almost), None);
        assert!(detect_format(&two).is_some(), "a 2-byte gzip header is still gzip");
    }

    #[test]
    fn a_missing_path_returns_none_without_panicking() {
        let s = scratch("missing");
        let ghost = s.0.join("never-created.zip");

        assert_eq!(detect_format(&ghost), None);
    }

    #[test]
    fn a_directory_returns_none() {
        let s = scratch("dir");

        assert_eq!(detect_format(&s.0), None);
    }

    #[test]
    fn unknown_bytes_return_none() {
        let s = scratch("unknown");
        let path = file_in(&s.0, "zeros.bin", &vec![0u8; 512]);

        assert_eq!(detect_format(&path), None);
    }

    #[test]
    fn all_three_zip_headers_count_as_zip() {
        for magic in [
            &[0x50u8, 0x4B, 0x03, 0x04][..],
            &[0x50, 0x4B, 0x05, 0x06],
            &[0x50, 0x4B, 0x07, 0x08],
        ] {
            let head = head_with(0, magic);
            assert_eq!(detect_in_head(&head), Some(Format::Zip));
        }
    }

    #[test]
    fn nested_probe_only_upgrades_a_real_tar_stream() {
        let tar_stream = head_with(TAR_MAGIC_AT, TAR_MAGIC);
        assert!(tar_wrapped(&tar_stream));
        assert!(!tar_wrapped(&head_with(0, &[0x1F, 0x8B])));
        // A stream shorter than the marker range is not a tar either
        assert!(!tar_wrapped(&[0u8; 100]));
    }

    #[test]
    fn probing_degrades_to_single_file_when_nothing_can_be_read() {
        // No such archive, and in any case no readable stream: the caller must
        // still get a usable format back
        let s = scratch("probe");
        let ghost = s.0.join("never-created.gz");

        assert_eq!(probe_nested(&ghost, Format::Gz), Format::Gz);
        assert_eq!(probe_nested(&ghost, Format::Tar), Format::Tar);
    }

    #[test]
    fn probing_an_unknown_magic_file_leaves_the_format_alone() {
        let s = scratch("probe-unknown");
        let path = file_in(&s.0, "thing.gz", &head_with(0, &[0x1F, 0x8B]));

        // Whatever the tool does or does not exist, the result must be one of
        // the two sane answers and never a panic
        let out = probe_nested(&path, Format::Gz);
        assert!(matches!(out, Format::Gz | Format::TarGz), "{out:?}");
    }

    #[test]
    fn every_format_maps_to_its_command() {
        let lang = Lang::builtin();
        let dir = Path::new("D:\\work");
        let path = Path::new(r"D:\work\archive.dat");

        let cases: [(Format, &str, &[&str]); 14] = [
            (Format::Tar, "tar", &["-xf"]),
            (Format::TarGz, "tar", &["-xzf"]),
            (Format::TarXz, "tar", &["-xJf"]),
            (Format::TarZst, "tar", &["--zstd", "-x", "-f"]),
            (Format::TarBz2, "tar", &["--bzip2", "-x", "-f"]),
            (Format::Zip, "tar", &["-xf"]),
            (Format::SevenZip, "7z", &["x", "-y"]),
            (Format::Rar, "7z", &["x", "-y"]),
            (Format::Cab, "expand", &["-F:"]),
            (Format::Wim, "dism", &["/Apply-Image", "/ImageFile:", "/Destination-Image:"]),
            (Format::Gz, "gzip", &["-d", "-f"]),
            (Format::Xz, "xz", &["-d", "-f"]),
            (Format::Zst, "zstd", &["-d", "-f"]),
            (Format::Bz2, "bzip2", &["-d", "-f"]),
        ];

        for (format, program, flags) in cases {
            // A single-file archive must carry the suffix its own format
            // appends, otherwise extraction refuses rather than guess
            let file = match format.stream_ext() {
                Some(suffix) => PathBuf::from(format!(r"D:\work\payload.{suffix}")),
                None => path.to_path_buf(),
            };
            let cmd = build_extract_command(&lang, format, &file, dir).expect("build");
            assert_eq!(cmd.get_program(), program, "{format}");

            let all: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
            assert!(!all.is_empty(), "{format} takes no arguments");
            // Every declared flag leads the argument list, in order
            let mut rest = all.as_slice();
            for flag in flags {
                let (head, tail) = rest.split_first().expect("flag present");
                assert!(head.starts_with(flag), "{format}: expected {flag} in {all:?}");
                rest = tail;
            }
            // The archive path is always passed, either as its own argument or
            // glued to a flag (`-F:<path>`, `/ImageFile:<path>`)
            let path_arg = file.display().to_string();
            assert!(
                all.iter().any(|a| a.contains(&path_arg)),
                "{format}: the archive path is missing from {all:?}"
            );
        }
    }

    #[test]
    fn a_single_file_archive_without_an_extension_is_refused() {
        let lang = Lang::builtin();
        let dir = Path::new("D:\\work");
        // No extension to strip means the output would land on the input
        for name in [r"payload", ".gz", "no_suffix_here"] {
            let path = Path::new(name);
            let err = build_extract_command(&lang, Format::Gz, path, dir)
                .expect_err("a name with nothing to strip must be refused");
            assert!(!err.to_string().is_empty());
        }
        // An archive format has no such problem: it unpacks into a directory
        assert!(build_extract_command(&lang, Format::Tar, Path::new("payload"), dir).is_ok());
    }

    #[test]
    fn extract_target_tracks_what_the_command_produces() {
        let dir = Path::new(r"D:\work");

        // Archives fill the destination directory
        assert_eq!(extract_target(Format::TarGz, Path::new(r"D:\work\a.tar.gz"), dir), dir);
        // A single-file archive produces the name without its extension
        assert_eq!(
            extract_target(Format::Gz, Path::new(r"D:\work\a.txt.gz"), dir),
            Path::new(r"D:\work\a.txt")
        );
        assert_eq!(
            extract_target(Format::Bz2, Path::new(r"D:\work\a.bz2"), dir),
            Path::new(r"D:\work\a")
        );
    }

    #[test]
    fn labels_and_fail_keys_cover_every_format() {
        // Every format needs a UI name and a failure key that exists in the
        // translation table, otherwise the fallback chain would show the key
        let lang = Lang::builtin();
        let all = [
            Format::Gz,
            Format::Xz,
            Format::Zst,
            Format::Bz2,
            Format::TarGz,
            Format::TarXz,
            Format::TarZst,
            Format::TarBz2,
            Format::Tar,
            Format::Zip,
            Format::SevenZip,
            Format::Rar,
            Format::Cab,
            Format::Wim,
        ];
        for format in all {
            assert!(!format.label().is_empty(), "{format} has no label");
            assert_eq!(format.to_string(), format.label());
            let msg = lang.t(format.fail_key());
            assert!(!msg.starts_with("extract."), "{} has no copy: {msg}", format.fail_key());
        }
        // Names are proper nouns and must not drift per language
        assert_eq!(format!("{}", Format::TarGz), "tar.gz");
        assert_eq!(format!("{}", Format::SevenZip), "7z");
    }

    #[test]
    fn compressing_then_extracting_returns_the_original_name() {
        // The round trip that regressed: packing used to replace the extension,
        // so `report.csv` became `report.gz` and unpacking handed back a bare
        // `report` with the `.csv` gone for good
        for (format, detected) in [("gz", Format::Gz), ("xz", Format::Xz), ("zst", Format::Zst)] {
            let src = Path::new(r"D:\work\report.csv");
            let packed = crate::compress::output_path(format, src);
            assert_eq!(
                single_file_output(detected, &packed).as_deref(),
                Some(src),
                "{format} lost the name on the way back"
            );
        }
    }

    #[test]
    fn a_stream_archive_only_gives_back_the_suffix_it_added() {
        assert_eq!(
            single_file_output(Format::Gz, Path::new(r"D:\work\report.csv.gz")).as_deref(),
            Some(Path::new(r"D:\work\report.csv"))
        );
        assert_eq!(
            single_file_output(Format::Xz, Path::new(r"D:\work\report.csv.xz")).as_deref(),
            Some(Path::new(r"D:\work\report.csv"))
        );
    }

    #[test]
    fn a_suffix_the_format_did_not_write_is_refused_rather_than_stripped() {
        // gzip bytes behind a `.txt` or a `.zip`: stripping that extension would
        // silently rename the user's file, so the caller gives up instead
        assert_eq!(single_file_output(Format::Gz, Path::new(r"D:\work\notes.txt")), None);
        assert_eq!(single_file_output(Format::Gz, Path::new(r"D:\work\archive.zip")), None);
        // The right suffix but a different format is still a foreign name
        assert_eq!(single_file_output(Format::Gz, Path::new(r"D:\work\notes.xz")), None);
    }

    #[test]
    fn nothing_to_strip_is_refused() {
        // Each of these would land the output on top of the input
        assert_eq!(single_file_output(Format::Gz, Path::new(r"D:\work\payload")), None);
        assert_eq!(single_file_output(Format::Gz, Path::new(r"D:\work\.gz")), None);
        assert_eq!(single_file_output(Format::Zst, Path::new("")), None);
        // Archive formats never strip a suffix
        assert_eq!(single_file_output(Format::Zip, Path::new(r"D:\work\report.csv.gz")), None);
    }

    #[test]
    fn the_suffix_is_matched_case_insensitively() {
        assert_eq!(
            single_file_output(Format::Gz, Path::new(r"D:\work\report.csv.GZ")).as_deref(),
            Some(Path::new(r"D:\work\report.csv"))
        );
    }

    #[test]
    fn every_tar_invocation_names_an_operation() {
        // `--zstd` and `--bzip2` cannot fold into `-xzf`, so without an explicit
        // `-x` tar refuses with "Must specify one of -c, -r, -t, -u, -x" and
        // unpacks nothing at all
        let lang = Lang::builtin();
        let dir = Path::new("D:\\work");
        for format in [
            Format::Tar,
            Format::TarGz,
            Format::TarXz,
            Format::TarZst,
            Format::TarBz2,
        ] {
            let path = Path::new(r"D:\work\bundle.tar");
            let cmd = build_extract_command(&lang, format, path, dir).expect("build");
            assert_eq!(cmd.get_program(), "tar", "{format}");

            let all: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            // `-xzf` / `-xJf` carry the operation inside the same flag, while
            // the `--zstd` form has to spell `-x` out on its own
            let names_an_operation = all.iter().any(|a| a == "-x")
                || all.iter().any(|a| a.starts_with("-x") && a.len() > 2);
            assert!(
                names_an_operation,
                "{format} would run without an operation: {all:?}"
            );
        }
    }

    #[test]
    fn a_directory_survives_the_pack_and_unpack_round_trip() {
        // The pairing that matters: packing a folder with a stream format writes
        // a `.tar.zst`, and unpacking it has to name the same operation tar used
        // when writing
        let s = scratch("roundtrip");
        let tree = s.0.join("tree");
        fs::create_dir_all(tree.join("inner")).unwrap();
        fs::write(tree.join("inner/leaf.txt"), b"payload").unwrap();

        // `build_command` hands back a command that has not been started yet
        let (mut pack, packed) = crate::compress::build_command("zst", &tree).expect("build pack");
        assert_eq!(packed, s.0.join("tree.tar.zst"));
        let packed_status = pack.status().expect("run tar");
        assert!(packed_status.success(), "packing failed: {packed_status}");
        assert!(packed.is_file(), "packing produced nothing");

        // The name has to be recognized from its magic bytes and probed as
        // tar-wrapped, exactly as the extractor does at run time
        let detected = detect_format(&packed).expect("not recognized");
        assert_eq!(probe_nested(&packed, detected), Format::TarZst);

        // Members must be stored relative to the parent, not as the absolute
        // path: tar silently drops the drive letter, and unpacking an archive
        // holding `/Users/name/...` would rebuild that whole chain
        let listing = Command::new("tar")
            .arg("--zstd")
            .arg("-tf")
            .arg(&packed)
            .output()
            .expect("list members");
        assert!(listing.status.success(), "cannot list members");
        let members = String::from_utf8_lossy(&listing.stdout);
        assert!(
            members.contains("tree/inner/leaf.txt"),
            "members are not relative: {members}"
        );
        assert!(
            !members.contains(':') && !members.contains("Users"),
            "an absolute path leaked into the archive: {members}"
        );

        let lang = Lang::builtin();
        let mut cmd =
            build_extract_command(&lang, Format::TarZst, &packed, &s.0).expect("build unpack");

        // Unpack somewhere clean, so finding the tree there proves it worked
        let dest = s.0.join("out");
        fs::create_dir_all(&dest).unwrap();
        cmd.current_dir(&dest);
        let status = cmd.status().expect("run tar");
        assert!(status.success(), "unpacking failed: {status}");
        assert_eq!(
            fs::read_to_string(dest.join("tree/inner/leaf.txt")).unwrap(),
            "payload",
            "the tree did not survive the round trip"
        );
        // Nothing may appear above `dest`: that is where a stray absolute
        // member would land
        assert!(
            !s.0.join("Users").exists(),
            "extraction escaped the destination directory"
        );
    }
}
