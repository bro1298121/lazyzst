use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Result;

use crate::i18n::Lang;

/// Output file extension per format
pub(crate) fn output_ext(format: &str) -> &'static str {
    match format {
        "tar" => "tar",
        "zip" => "zip",
        "wim" => "wim",
        "7z" => "7z",
        "zst" => "zst",
        "gz" => "gz",
        "xz" => "xz",
        _ => unreachable!(),
    }
}

/// Whether the format is a single-file stream compressor
///
/// These wrap exactly one file, so the original name has to survive into the
/// output for the round trip to give it back
fn is_stream(format: &str) -> bool {
    matches!(format, "gz" | "xz" | "zst")
}

/// Where a format's output lands next to its input.
///
/// Stream formats **append** the suffix (`notes.csv` -> `notes.csv.gz`) so that
/// decompressing restores `notes.csv` exactly. Replacing the extension instead
/// would swallow the real one and hand back a bare `notes`, silently renaming
/// the user's file on the way out.
///
/// Archive formats keep replacing (`notes.csv` -> `notes.tar`): their contents
/// travel inside the archive under their own names, so nothing is lost, and a
/// single `.tar` reads better than `.csv.tar`.
pub(crate) fn output_path(format: &str, path: &Path) -> PathBuf {
    if is_stream(format) {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        // A directory cannot be streamed, so it is tarred first and the stream
        // wraps that tar. The name has to say so, or unpacking could not tell a
        // bare `.gz` of one file from a `.tar.gz` of a tree
        if path.is_dir() {
            name.push(".tar");
        }
        name.push(".");
        name.push(output_ext(format));
        path.with_file_name(name)
    } else {
        path.with_extension(output_ext(format))
    }
}

/// Success prefix and failure message for a format, localized through `lang`.
///
/// Still a pure lookup: no filesystem, no process, no state. `lang.t` never
/// panics and falls back to `en-us` and then to the key, so an unknown key
/// shows up as `compress.tar.fail` rather than as a crash.
pub(crate) fn format_spec(lang: &Lang, format: &str) -> (String, String) {
    let (ok_key, fail_key) = match format {
        "tar" => ("compress.tar.ok", "compress.tar.fail"),
        "zip" => ("compress.zip.ok", "compress.zip.fail"),
        "wim" => ("compress.wim.ok", "compress.wim.fail"),
        "7z" => ("compress.7z.ok", "compress.7z.fail"),
        "zst" => ("compress.zst.ok", "compress.zst.fail"),
        "gz" => ("compress.gz.ok", "compress.gz.fail"),
        "xz" => ("compress.xz.ok", "compress.xz.fail"),
        _ => unreachable!(),
    };
    (lang.t(ok_key), lang.t(fail_key))
}

/// Assemble the not-yet-started command and output path for a format.
/// gz / xz create the output file up front, so a failure to create it is
/// reported right away instead of turning into a child process that runs and fails
pub(crate) fn build_command(format: &str, path: &Path) -> Result<(Command, PathBuf)> {
    let out = output_path(format, path);

    // None of gzip / xz / zstd accepts a directory: they answer
    // "is a directory -- ignored" and exit non-zero without writing anything,
    // which is what made packing a folder with n / m / b silently fail. A tree
    // has to be tarred first, and `tar -c<flag>f` does the tar and the
    // compression in one child process, so the job stays a single command and
    // leaves no intermediate `.tar` to clean up afterwards
    let dir_stream = is_stream(format) && path.is_dir();

    // tar takes its member names from the path it is handed. Given an absolute
    // one it drops the drive letter ("Removing leading drive letter from member
    // names") and records the members as `/Users/name/...`, so unpacking
    // rebuilds that entire chain from the drive root down. Running from the
    // parent and naming only the entry keeps the members relative, which is
    // what makes the archive portable. The output path stays absolute, so it is
    // unaffected by the working directory.
    let parent = path.parent().unwrap_or(Path::new("."));
    let member = path.file_name().unwrap_or_default();

    let mut cmd = if dir_stream {
        let mut c = Command::new("tar");
        match format {
            "xz" => {
                c.arg("-cJf");
            }
            "zst" => {
                // bsdtar spells this one out instead of folding it into a
                // single-letter flag like -z or -J
                c.arg("--zstd").arg("-cf");
            }
            _ => {
                c.arg("-czf");
            }
        }
        c.current_dir(parent).arg(&out).arg(member);
        c
    } else {
        match format {
            "tar" => {
                let mut c = Command::new("tar");
                c.current_dir(parent).arg("-cf").arg(&out).arg(member);
                c
            }
            "zip" => {
                let mut c = Command::new("powershell");
                c.args([
                    "-Command",
                    &format!(
                        "Compress-Archive -Path '{}' -DestinationPath '{}' -Force",
                        path.display(),
                        out.display()
                    ),
                ]);
                c
            }
            "wim" => {
                let mut c = Command::new("dism");
                c.args([
                    "/Capture-Image",
                    &format!("/ImageFile:{}", out.display()),
                    &format!("/CaptureDir:{}", path.display()),
                    "/Name:archive",
                    "/Compress:max",
                ]);
                c
            }
            "7z" => {
                let mut c = Command::new("7z");
                c.arg("a").arg(&out).arg(path);
                c
            }
            // A single file: the compressor writes the stream straight out
            "zst" => {
                let mut c = Command::new("zstd");
                c.arg("-f").arg("-T0").arg("-o").arg(&out).arg(path);
                c
            }
            "gz" | "xz" => {
                let mut c = Command::new(if format == "gz" { "gzip" } else { "xz" });
                c.arg("-k")
                    .arg("-f")
                    .arg("-c")
                    .arg(path)
                    .stdout(Stdio::from(fs::File::create(&out)?));
                c
            }
            _ => unreachable!(),
        }
    };

    // Only the single-file gz / xz branch already sent stdout into the output
    // file. Everything else discards its output, otherwise the progress chatter
    // from tar / 7z / zstd / dism lands in the TUI and shreds the screen and
    // the progress bar
    if !(matches!(format, "gz" | "xz") && !dir_stream) {
        cmd.stdout(Stdio::null());
    }
    cmd.stderr(Stdio::null());

    Ok((cmd, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMATS: [&str; 7] = ["tar", "zip", "wim", "7z", "zst", "gz", "xz"];

    /// A temporary directory that removes itself on drop, whether the test
    /// passes or panics. Named off the process id so parallel runs cannot
    /// collide
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
        let dir = std::env::temp_dir().join(format!(
            "lazyzst-compress-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    #[test]
    fn every_format_has_copy_in_every_language() {
        // A typo in a key would silently render as the key itself (the last
        // step of the fallback chain), so assert we never get a dotted key back
        for tag in ["zh-cn", "zh-tw", "en-us"] {
            let mut lang = Lang::builtin();
            lang.current = tag.to_string();
            for format in FORMATS {
                let (ok, fail) = format_spec(&lang, format);
                assert!(!ok.starts_with("compress."), "{tag}/{format} ok={ok}");
                assert!(!fail.starts_with("compress."), "{tag}/{format} fail={fail}");
                assert!(!ok.is_empty() && !fail.is_empty(), "{tag}/{format}");
                assert_eq!(output_ext(format), format);
            }
        }
    }

    #[test]
    fn tar_is_described_as_packing_while_the_rest_say_compressing() {
        let lang = Lang::builtin();
        assert_eq!(format_spec(&lang, "tar").0, "Packed");
        assert_eq!(format_spec(&lang, "zst").0, "Compressed");
    }

    #[test]
    fn a_directory_packed_as_a_stream_goes_through_tar() {
        // gzip / xz / zstd answer "is a directory -- ignored" and exit non-zero
        // without writing anything, so a folder has to be tarred first. This
        // runs the real tool over a real folder, because the point is the
        // behaviour of tar, not just the shape of the argument list
        let dir = scratch("stream-dir");
        fs::create_dir_all(dir.0.join("tree/inner")).unwrap();
        fs::write(dir.0.join("tree/inner/leaf.txt"), b"payload").unwrap();

        for (format, ext) in [("gz", "gz"), ("xz", "xz"), ("zst", "zst")] {
            let target = dir.0.join("tree");
            let (mut cmd, out) = build_command(format, &target).expect("build");

            assert_eq!(cmd.get_program(), "tar", "{format}");
            assert_eq!(out, dir.0.join(format!("tree.tar.{ext}")), "{format}");

            let status = cmd.status().expect("run tar");
            assert!(status.success(), "{format} failed: {status}");
            assert!(out.is_file(), "{format} produced nothing");

            // The archive has to hold the tree, not just exist
            let listing = Command::new("tar")
                .arg("-tf")
                .arg(&out)
                .output()
                .expect("list");
            assert!(listing.status.success(), "{format} is not a readable archive");
            let names = String::from_utf8_lossy(&listing.stdout);
            assert!(
                names.contains("leaf.txt"),
                "{format} lost the contents: {names}"
            );
        }
    }

    #[test]
    fn a_directory_stream_keeps_both_suffixes() {
        let dir = scratch("stream-name");
        let tree = dir.0.join("tree");
        fs::create_dir_all(&tree).unwrap();
        // The `.tar` matters: without it a `.gz` of a tree is indistinguishable
        // from a `.gz` of a single file
        assert_eq!(output_path("zst", &tree), dir.0.join("tree.tar.zst"));
        assert_eq!(output_path("gz", &tree), dir.0.join("tree.tar.gz"));
        assert_eq!(output_path("xz", &tree), dir.0.join("tree.tar.xz"));
        // A directory with a dot in its name still only appends
        fs::create_dir_all(dir.0.join("my.project")).unwrap();
        assert_eq!(
            output_path("zst", &dir.0.join("my.project")),
            dir.0.join("my.project.tar.zst")
        );
    }

    #[test]
    fn a_single_file_stream_is_still_handled_directly() {
        // The folder workaround must not leak into the ordinary file path
        let dir = scratch("stream-file");
        let file = dir.0.join("report.csv");
        fs::write(&file, b"a,b,c").unwrap();

        assert_eq!(output_path("zst", &file), dir.0.join("report.csv.zst"));
        let (cmd, _) = build_command("zst", &file).expect("build");
        assert_eq!(cmd.get_program(), "zstd", "a file needs no tar");
    }

    #[test]
    fn an_archive_format_on_a_directory_is_unchanged() {
        // tar / zip / 7z / wim all took a directory before, so nothing about
        // their handling may change
        let dir = scratch("archive-dir");
        let tree = dir.0.join("tree");
        fs::create_dir_all(&tree).unwrap();

        assert_eq!(output_path("tar", &tree), dir.0.join("tree.tar"));
        let (cmd, _) = build_command("tar", &tree).expect("build");
        assert_eq!(cmd.get_program(), "tar");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"-cf".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a == "--zstd"), "{args:?}");
    }

    #[test]
    fn wim_keeps_the_administrator_hint() {
        let lang = Lang::builtin();
        let (_, fail) = format_spec(&lang, "wim");
        assert!(fail.contains("administrator"), "{fail}");
    }

    #[test]
    fn stream_formats_append_their_suffix_while_archives_replace_it() {
        let src = Path::new(r"D:\work\report.csv");
        // Stream formats wrap exactly one file, so the suffix is appended.
        // Replacing here is what used to break the round trip: `report.csv`
        // became `report.gz`, which came back as a bare `report`
        assert_eq!(output_path("gz", src), Path::new(r"D:\work\report.csv.gz"));
        assert_eq!(output_path("xz", src), Path::new(r"D:\work\report.csv.xz"));
        assert_eq!(output_path("zst", src), Path::new(r"D:\work\report.csv.zst"));
        // Archives carry their contents under their own names, so nothing is
        // lost by replacing, and a single `.tar` reads better than `.csv.tar`
        assert_eq!(output_path("tar", src), Path::new(r"D:\work\report.tar"));
        assert_eq!(output_path("zip", src), Path::new(r"D:\work\report.zip"));
    }

    #[test]
    fn appending_works_for_names_without_an_extension() {
        // `with_extension` would have left these untouched and produced a name
        // identical to the input
        assert_eq!(output_path("gz", Path::new(r"D:\work\README")), Path::new(r"D:\work\README.gz"));
        assert_eq!(
            output_path("zst", Path::new(r"D:\work\.gitignore")),
            Path::new(r"D:\work\.gitignore.zst")
        );
    }
}