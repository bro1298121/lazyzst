use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Result;

use crate::i18n::Lang;

/// How many names `batch_output_name` is willing to try before giving up.
///
/// The probe walks `_2`, `_3`, ... and a directory could in theory already hold
/// a thousand of them. The cap turns what would be a hang into a definite (if
/// almost unreachable) answer.
const BATCH_NAME_LIMIT: usize = 1000;

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

/// Whether every path sits directly inside the very same directory.
///
/// A batch archive is written next to its members and every tool is handed bare
/// member names taken from that directory, so entries pulled from two different
/// places have no single spelling. `App::compress` turns `false` into a
/// localized refusal; `build_command` re-checks it as an invariant.
pub(crate) fn same_parent(targets: &[PathBuf]) -> bool {
    let Some(first) = targets.first().and_then(|p| p.parent()) else {
        return false;
    };
    targets.iter().all(|p| p.parent() == Some(first))
}

/// Suffix a batch output carries, leading dot excluded.
///
/// The stream formats spell out the tar they wrap, so the name says what the
/// file really is: `3_files.tar.zst`, not a `3_files.zst` that would unpack as
/// one lone stream.
fn batch_ext(format: &str) -> &'static str {
    match format {
        "tar" => "tar",
        "zip" => "zip",
        "7z" => "7z",
        "zst" => "tar.zst",
        "gz" => "tar.gz",
        "xz" => "tar.xz",
        _ => unreachable!(),
    }
}

/// Name for an archive holding `count` marked entries.
///
/// A batch has no single input to name itself after, so it is named after its
/// size. `_2`, `_3`, ... are appended until a free name turns up, which keeps an
/// existing archive from being overwritten: the first run gets `3_files.zip`,
/// the next `3_files_2.zip`, and so on.
pub(crate) fn batch_output_name(count: usize, ext: &str, dir: &Path) -> PathBuf {
    let stem = format!("{count}_files");
    let first = dir.join(format!("{stem}.{ext}"));
    if !first.exists() {
        return first;
    }
    for n in 2..=BATCH_NAME_LIMIT {
        let candidate = dir.join(format!("{stem}_{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    // Every index up to the cap is taken, which is absurd on purpose. Stepping
    // past the cap keeps the function bounded; returning one of the names above
    // would have the tool overwrite an archive the user still has
    let past_the_cap = BATCH_NAME_LIMIT + 1;
    dir.join(format!("{stem}_{past_the_cap}.{ext}"))
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
///
/// `targets` holds one entry or several. A single entry behaves exactly as it
/// always did: its own name decides the output and its own kind decides the
/// tool. Several entries need one archive to live somewhere, so they must all
/// sit in the same directory; `App::compress` refuses anything else with
/// localized copy, and the check is repeated here as an invariant so this
/// function can never quietly pack the first entry and drop the rest.
///
/// gz / xz create the output file up front, so a failure to create it is
/// reported right away instead of turning into a child process that runs and fails
pub(crate) fn build_command(format: &str, targets: &[PathBuf]) -> Result<(Command, PathBuf)> {
    let Some(first) = targets.first() else {
        anyhow::bail!("compress called without a target");
    };
    let batch = targets.len() > 1;

    // wim captures one directory. Handing it several would write an image of
    // whichever one dism happened to read first, which reads as success while
    // silently losing the rest, so refuse instead.
    if batch && format == "wim" {
        anyhow::bail!("wim captures a single directory, not {}", targets.len());
    }

    // tar takes its member names from the path it is handed. Given an absolute
    // one it drops the drive letter ("Removing leading drive letter from member
    // names") and records the members as `/Users/name/...`, so unpacking
    // rebuilds that entire chain from the drive root down. Running from the
    // parent and naming only the entry keeps the members relative, which is
    // what makes the archive portable. The output path stays absolute, so it is
    // unaffected by the working directory.
    let parent = first.parent().unwrap_or(Path::new("."));
    if batch && !same_parent(targets) {
        anyhow::bail!("a batch has to sit in one directory");
    }
    let members: Vec<&OsStr> = targets
        .iter()
        .map(|p| p.file_name().unwrap_or_default())
        .collect();

    let out = if batch {
        batch_output_name(targets.len(), batch_ext(format), parent)
    } else {
        output_path(format, first)
    };

    // Which tool writes this archive. tar carries every format except 7z, wim
    // and a lone stream-compressed file; the last one needs no tar because the
    // compressor can wrap a single file on its own.
    //
    // None of gzip / xz / zstd accepts a directory: they answer "is a directory
    // -- ignored" and exit non-zero without writing anything, which is what made
    // packing a folder with n / m / b silently fail. A tree has to be tarred
    // first, and `tar -c<flag>f` does the tar and the compression in one child
    // process, so the job stays a single command and leaves no intermediate
    // `.tar` to clean up afterwards. Several entries need the same treatment.
    let via_tar = match format {
        "tar" | "zip" => true,
        "zst" | "gz" | "xz" => batch || first.is_dir(),
        _ => false,
    };

    let mut cmd = if via_tar {
        let mut c = Command::new("tar");
        match format {
            "tar" => {
                c.arg("-cf");
            }
            // `-a` picks the compressor from the output extension, and both GNU
            // tar and bsdtar (the tar that ships with Windows) understand it.
            // Powershell's `Compress-Archive` produced the same zip in a small
            // fraction of the time, so it is gone
            "zip" => {
                c.arg("-a").arg("-cf");
            }
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
        c.current_dir(parent).arg(&out).args(&members);
        c
    } else {
        match format {
            "wim" => {
                let mut c = Command::new("dism");
                c.args([
                    "/Capture-Image",
                    &format!("/ImageFile:{}", out.display()),
                    &format!("/CaptureDir:{}", first.display()),
                    "/Name:archive",
                    "/Compress:max",
                ]);
                c
            }
            // `-y` answers the overwrite question for us, which matters because
            // the child's stdin is not a terminal
            "7z" => {
                let mut c = Command::new("7z");
                c.arg("a").arg("-y").current_dir(parent).arg(&out).args(&members);
                c
            }
            // A single file: the compressor writes the stream straight out
            "zst" => {
                let mut c = Command::new("zstd");
                c.arg("-f").arg("-T0").arg("-o").arg(&out).arg(first);
                c
            }
            "gz" | "xz" => {
                let mut c = Command::new(if format == "gz" { "gzip" } else { "xz" });
                // No `-k`: the stream goes to stdout, which already means the
                // input is never removed, so "keep" says nothing here. gzip
                // 1.3.12 (the Scoop build) does not accept the flag at all and
                // exits 1, which broke every single-file gz pack
                c.arg("-f")
                    .arg("-c")
                    .arg(first)
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
    if !(matches!(format, "gz" | "xz") && !via_tar) {
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

    /// Two files and one subdirectory, which is the smallest shape that exercises
    /// "several entries, one of them a tree". The three targets come back in
    /// marking order.
    fn batch_fixture(tag: &str) -> (Scratch, Vec<PathBuf>) {
        let s = scratch(tag);
        fs::create_dir_all(s.0.join("sub")).unwrap();
        fs::write(s.0.join("first.txt"), b"one").unwrap();
        fs::write(s.0.join("second.txt"), b"two").unwrap();
        fs::write(s.0.join("sub/inner.txt"), b"three").unwrap();
        let targets = vec![
            s.0.join("first.txt"),
            s.0.join("second.txt"),
            s.0.join("sub"),
        ];
        (s, targets)
    }

    /// Arguments of a command that has not been started yet, as plain strings
    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// Member names inside `out`, as the tool that reads it back reports them,
    /// with separators normalized and directory entries trimmed.
    ///
    /// tar reads tar, zstd and zip on its own; 7z has an archive only it can
    /// list. `-slt` is the machine-readable form, where every entry is a
    /// `Path = <name>` line, the archive itself being the first
    fn list_members(format: &str, out: &Path) -> Vec<String> {
        let raw = if format == "7z" {
            let listing = Command::new("7z")
                .arg("l")
                .arg("-slt")
                .arg(out)
                .output()
                .expect("run 7z l");
            assert!(listing.status.success(), "7z cannot list its own archive");
            String::from_utf8_lossy(&listing.stdout)
                .lines()
                .filter_map(|line| line.strip_prefix("Path = "))
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            let listing = Command::new("tar")
                .arg("-tf")
                .arg(out)
                .output()
                .expect("run tar -tf");
            assert!(listing.status.success(), "tar cannot list the {format} archive");
            String::from_utf8_lossy(&listing.stdout).into_owned()
        };

        raw.lines()
            .map(|name| name.trim().replace('\\', "/"))
            .map(|name| name.trim_end_matches('/').to_string())
            .filter(|name| !name.is_empty())
            .collect()
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
            let (mut cmd, out) = build_command(format, &[target]).expect("build");

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
        let (cmd, _) = build_command("zst", std::slice::from_ref(&file)).expect("build");
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
        let (cmd, _) = build_command("tar", std::slice::from_ref(&tree)).expect("build");
        assert_eq!(cmd.get_program(), "tar");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"-cf".to_string()), "{args:?}");
        assert!(!args.iter().any(|a| a == "--zstd"), "{args:?}");
        // A single directory is a one-member archive, not a batch: the output is
        // named after the tree rather than after a count
        assert!(!args.iter().any(|a| a.contains("_files")), "{args:?}");
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

    // ---- batch naming ----
    #[test]
    fn a_batch_takes_the_free_name_and_then_counts_up() {
        let s = scratch("batch-name");
        let dir = &s.0;

        // Nothing in the way: the plain name, with no index at all
        assert_eq!(batch_output_name(3, "zip", dir), dir.join("3_files.zip"));

        // One taken: the index starts at 2, not at 1, so the first run's name is
        // never a special case nobody guesses
        fs::write(dir.join("3_files.zip"), b"x").unwrap();
        assert_eq!(batch_output_name(3, "zip", dir), dir.join("3_files_2.zip"));

        // Two taken: the probe keeps going rather than overwriting either
        fs::write(dir.join("3_files_2.zip"), b"x").unwrap();
        assert_eq!(batch_output_name(3, "zip", dir), dir.join("3_files_3.zip"));

        // The count is part of the name, so batches of different sizes never
        // collide with one another
        assert_eq!(batch_output_name(5, "zip", dir), dir.join("5_files.zip"));
    }

    #[test]
    fn a_batch_name_says_what_it_really_is() {
        let dir = Path::new(r"D:\work");
        // The stream formats wrap a tar, and the name has to admit it: a
        // `3_files.zst` would unpack as one lone stream rather than a tree
        assert_eq!(batch_output_name(3, "tar.zst", dir), Path::new(r"D:\work\3_files.tar.zst"));
        assert_eq!(batch_output_name(3, "tar.gz", dir), Path::new(r"D:\work\3_files.tar.gz"));
        assert_eq!(batch_output_name(3, "tar.xz", dir), Path::new(r"D:\work\3_files.tar.xz"));
        assert_eq!(batch_output_name(3, "tar", dir), Path::new(r"D:\work\3_files.tar"));
        assert_eq!(batch_output_name(3, "7z", dir), Path::new(r"D:\work\3_files.7z"));
    }

    #[test]
    fn a_batch_name_probe_gives_up_instead_of_spinning() {
        // Every candidate from the plain name up to the cap is taken, which is
        // absurd on purpose: the function still has to come back with something
        let s = scratch("batch-full");
        let dir = &s.0;
        fs::write(dir.join("3_files.zip"), b"x").unwrap();
        for n in 2..=BATCH_NAME_LIMIT {
            fs::write(dir.join(format!("3_files_{n}.zip")), b"x").unwrap();
        }
        // Every index up to the cap is taken, so the probe has to stop and the
        // answer lands one past the cap rather than on a name that exists
        let past_the_cap = BATCH_NAME_LIMIT + 1;
        assert_eq!(
            batch_output_name(3, "zip", dir),
            dir.join(format!("3_files_{past_the_cap}.zip"))
        );
    }

    // ---- batch arguments ----
    #[test]
    fn a_batch_of_tar_formats_names_its_members_relative_to_the_parent() {
        // `-a` rather than `--format zip`: both GNU tar and bsdtar understand it,
        // and `Compress-Archive` took orders of magnitude longer for the same zip
        for (format, flags) in [
            ("tar", vec!["-cf"]),
            ("zst", vec!["--zstd", "-cf"]),
            ("gz", vec!["-czf"]),
            ("xz", vec!["-cJf"]),
            ("zip", vec!["-a", "-cf"]),
        ] {
            let (s, targets) = batch_fixture(format);
            let (cmd, out) = build_command(format, &targets).expect("build");

            assert_eq!(cmd.get_program(), "tar", "{format}");
            // Running from the parent is what keeps the members relative: given an
            // absolute name tar drops the drive letter and records the members as
            // `/Users/name/...`, which rebuilds that whole chain when unpacked
            assert_eq!(cmd.get_current_dir(), Some(s.0.as_path()), "{format}");
            assert_eq!(out, s.0.join(format!("3_files.{}", batch_ext(format))), "{format}");

            let args = args_of(&cmd);
            assert_eq!(&args[..flags.len()], flags, "{format}");
            assert_eq!(args[flags.len()], out.display().to_string(), "{format}");
            // Bare names, in exactly the order the user marked them
            assert_eq!(
                &args[flags.len() + 1..],
                ["first.txt", "second.txt", "sub"],
                "{format} must pass bare member names"
            );
        }
    }

    #[test]
    fn a_batch_of_7z_passes_bare_names_too() {
        let (s, targets) = batch_fixture("7z-batch");
        let (cmd, out) = build_command("7z", &targets).expect("build");

        assert_eq!(cmd.get_program(), "7z");
        assert_eq!(cmd.get_current_dir(), Some(s.0.as_path()));
        assert_eq!(out, s.0.join("3_files.7z"));
        // `-y` answers the overwrite question, which matters because the child's
        // stdin is not a terminal
        assert_eq!(
            args_of(&cmd),
            [
                "a".to_string(),
                "-y".to_string(),
                out.display().to_string(),
                "first.txt".to_string(),
                "second.txt".to_string(),
                "sub".to_string(),
            ]
        );
    }

    #[test]
    fn a_single_zip_target_goes_through_tar_as_well() {
        // The switch away from Powershell applies to a lone file too: `x` on one
        // target is the most common zip there is
        let (s, mut targets) = batch_fixture("zip-one");
        targets.truncate(1);
        let (cmd, out) = build_command("zip", &targets).expect("build");

        assert_eq!(cmd.get_program(), "tar");
        assert_eq!(
            args_of(&cmd),
            [
                "-a".to_string(),
                "-cf".to_string(),
                out.display().to_string(),
                "first.txt".to_string(),
            ]
        );
        // A single target keeps its own naming: no count, no index
        assert_eq!(out, s.0.join("first.zip"));
    }

    #[test]
    fn a_batch_refuses_the_formats_that_cannot_take_several_entries() {
        let (_s, targets) = batch_fixture("wim-batch");
        // dism names one capture dir; handing it several would image whichever
        // one it read first and report success while dropping the rest
        let err = build_command("wim", &targets).expect_err("wim must refuse a batch");
        assert!(!err.to_string().is_empty());

        // A single directory is still perfectly fine
        let (s, mut one) = batch_fixture("wim-one");
        one.truncate(1);
        let (cmd, out) = build_command("wim", &one).expect("build");
        assert_eq!(cmd.get_program(), "dism");
        assert_eq!(out, s.0.join("first.wim"));
        let args = args_of(&cmd);
        assert!(args.iter().any(|a| a.starts_with("/CaptureDir:")), "{args:?}");
    }

    #[test]
    fn entries_from_two_directories_cannot_share_one_archive() {
        let s = scratch("mixed-dirs");
        fs::create_dir_all(s.0.join("one")).unwrap();
        fs::create_dir_all(s.0.join("two")).unwrap();
        let a = s.0.join("one/a.txt");
        let b = s.0.join("two/b.txt");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();

        assert!(!same_parent(&[a.clone(), b.clone()]));
        assert!(
            build_command("tar", &[a.clone(), b.clone()]).is_err(),
            "two directories have no common spelling for their members"
        );

        // One entry never needs the check, and same-directory entries pass it
        assert!(same_parent(std::slice::from_ref(&a)));
        assert!(build_command("tar", std::slice::from_ref(&a)).is_ok());
        let c = s.0.join("one/c.txt");
        assert!(same_parent(&[a.clone(), c.clone()]));
    }

    #[test]
    fn a_batch_with_no_targets_is_refused_rather_than_panicking() {
        assert!(build_command("tar", &[]).is_err());
        assert!(build_command("zip", &[]).is_err());
    }

    // ---- real tools ----
    #[test]
    fn a_real_batch_archive_holds_every_member_under_a_relative_name() {
        // The bug this guards: handing tar the absolute paths made it print
        // "Removing leading drive letter from member names" and store the members
        // as `/Users/name/...`, so unpacking rebuilt the entire chain from the
        // drive root. Only running the real tools catches that, because the
        // argument list looks fine either way
        for (format, ext) in [
            ("tar", "tar"),
            ("zst", "tar.zst"),
            ("zip", "zip"),
            ("7z", "7z"),
        ] {
            let (s, targets) = batch_fixture(format);
            let (mut cmd, out) = build_command(format, &targets).expect("build");
            assert_eq!(out, s.0.join(format!("3_files.{ext}")), "{format}");

            let status = cmd.status().unwrap_or_else(|e| panic!("run {format}: {e}"));
            assert!(status.success(), "{format} failed: {status}");
            assert!(out.is_file(), "{format} produced nothing");

            let members = list_members(format, &out);
            assert!(members.contains(&"first.txt".to_string()), "{format}: {members:?}");
            assert!(members.contains(&"second.txt".to_string()), "{format}: {members:?}");
            // The directory came along whole, not as an empty placeholder
            assert!(
                members.iter().any(|m| m == "sub" || m.starts_with("sub/")),
                "{format} dropped the tree: {members:?}"
            );
            for name in &members {
                assert!(!name.contains(':'), "{format} stored a drive letter: {name}");
                assert!(!name.contains("Users"), "{format} stored an absolute path: {name}");
                assert!(!name.starts_with('/'), "{format} stored an absolute path: {name}");
                assert!(
                    !name.contains(&s.0.display().to_string().replace('\\', "/")),
                    "{format} stored the whole path: {name}"
                );
            }

            // `tar -a -cf` has to produce a zip other tools can open, not a tar
            // wearing a `.zip` name
            if format == "zip" {
                let head = fs::read(&out).expect("read the zip back");
                assert_eq!(&head[..4], &[0x50, 0x4B, 0x03, 0x04], "not a zip archive");
            }
        }
    }

    #[test]
    fn a_single_file_stream_really_writes_into_the_output_file() {
        // gz / xz stream to stdout, so the only thing keeping the archive is the
        // redirect into the output file. Point stdout at /dev/null instead and the
        // file comes out empty while the job still reports success.
        //
        // Both compressors are exercised with the real binary. gzip used to be
        // skipped here because it was invoked with `-k`, which gzip 1.3.12
        // rejects outright: that turned a genuine bug (every single-file gz
        // pack failing) into an excuse to test something else instead
        let s = scratch("stream-stdout");
        let payload = b"a,b,c,a,b,c,a,b,c";

        for (format, program, ext) in [("gz", "gzip", "gz"), ("xz", "xz", "xz")] {
            // The same input for both: appending the suffix has to give
            // `report.csv.gz` / `report.csv.xz`, never `report.gz`
            let file = s.0.join("report.csv");
            fs::write(&file, payload).unwrap();

            let (mut cmd, out) = build_command(format, std::slice::from_ref(&file)).expect("build");
            assert_eq!(cmd.get_program(), program, "wrong compressor for {format}");
            // `-k` would be rejected by older gzip builds and is meaningless
            // here: the stream goes to stdout, so the input is never at risk
            assert!(
                !cmd.get_args().any(|a| a == "-k"),
                "{format} must not pass -k: {}",
                cmd.get_args()
                    .map(|a| a.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(" ")
            );

            let status = cmd.status().expect("run compressor");
            assert!(status.success(), "{format} failed: {status}");
            assert_eq!(out, s.0.join(format!("report.csv.{ext}")));
            assert!(
                fs::metadata(&out).expect("stat output").len() > 0,
                "{format}: the stream never reached the output file"
            );
            // The input has to survive: the round trip hands it back unchanged
            assert_eq!(fs::read(&file).unwrap(), payload, "{format} touched the input");
        }
    }

    #[test]
    fn a_second_batch_never_overwrites_the_first() {
        // Run the same batch twice and both archives have to survive: the probe is
        // what keeps `3_files.tar` from being clobbered by `3_files_2.tar`
        let (s, targets) = batch_fixture("batch-twice");
        let (mut first_cmd, first) = build_command("tar", &targets).expect("build");
        let status = first_cmd.status().expect("run tar");
        assert!(status.success(), "the first batch failed: {status}");

        let (_second_cmd, second) = build_command("tar", &targets).expect("build");

        assert_ne!(first, second);
        assert_eq!(first.file_name().unwrap(), "3_files.tar");
        assert_eq!(second.file_name().unwrap(), "3_files_2.tar");
        assert_eq!(first.parent(), second.parent());
        // Both land beside the entries they were built from
        assert_eq!(first.parent(), Some(s.0.as_path()));
        // The first archive is still there, holding exactly what it held before
        assert!(first.is_file());
        let members = list_members("tar", &first);
        assert!(
            members.contains(&"first.txt".to_string())
                && members.contains(&"second.txt".to_string())
                && members.contains(&"sub/inner.txt".to_string()),
            "the first batch changed: {members:?}"
        );
    }
}