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

    let mut cmd = match format {
        "tar" => {
            let mut c = Command::new("tar");
            c.arg("-cf").arg(&out).arg(path);
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
    };

    // gz / xz already redirect stdout into the output file; every other format
    // discards its output: otherwise the progress chatter from 7z / zstd / dism
    // lands straight in the TUI and shreds the screen and the progress bar
    if !matches!(format, "gz" | "xz") {
        cmd.stdout(Stdio::null());
    }
    cmd.stderr(Stdio::null());

    Ok((cmd, out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORMATS: [&str; 7] = ["tar", "zip", "wim", "7z", "zst", "gz", "xz"];

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