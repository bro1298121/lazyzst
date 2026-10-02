use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::Result;

/// 各格式的：产物扩展名 / 成功文案前缀 / 失败文案
pub(crate) fn format_spec(format: &str) -> (&'static str, &'static str, &'static str) {
    match format {
        "tar" => ("tar", "已打包", "tar 打包失败"),
        "zip" => ("zip", "已压缩", "zip 压缩失败"),
        "wim" => ("wim", "已压缩", "wim 压缩失败（需要管理员权限）"),
        "7z" => ("7z", "已压缩", "7z 压缩失败"),
        "zst" => ("zst", "已压缩", "zst 压缩失败"),
        "gz" => ("gz", "已压缩", "gz 压缩失败"),
        "xz" => ("xz", "已压缩", "xz 压缩失败"),
        _ => unreachable!(),
    }
}

/// 按格式拼出未启动的命令与产物路径；gz / xz 预先建好产物文件，
/// 这样创建失败能立刻报错，而不会变成一个跑失败的子进程
pub(crate) fn build_command(format: &str, path: &Path) -> Result<(Command, PathBuf)> {
    let (ext, _, _) = format_spec(format);
    let out = path.with_extension(ext);

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

    // gz / xz 的 stdout 已重定向到产物文件，其余格式一律丢弃输出：
    // 否则 7z / zstd / dism 的进度输出会直接打进 TUI 画面，撕裂界面和进度条
    if !matches!(format, "gz" | "xz") {
        cmd.stdout(Stdio::null());
    }
    cmd.stderr(Stdio::null());

    Ok((cmd, out))
}