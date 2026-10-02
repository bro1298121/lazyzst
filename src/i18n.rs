//! UI copy in three languages, loaded from a user-editable JSON file.
//!
//! The table below is the single source of truth: each row is
//! `(key, zh-cn, zh-tw, en-us)`, so a new key physically cannot land in one
//! language and be forgotten in another. `Lang::builtin()` folds the rows into
//! the nested shape that gets written to `language.json`.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Language used as the second step of the fallback chain
pub(crate) const DEFAULT_LANG: &str = "en-us";

/// Built-in copy: `(key, zh-cn, zh-tw, en-us)`.
/// Placeholders are `{0}`, `{1}`, ... and are filled in by [`Lang::tf`].
const BUILTIN: &[(&str, &str, &str, &str)] = &[
    // ---- panel titles ----
    ("panel.path", "路径", "路徑", "Path"),
    ("panel.tree", "文件树", "檔案樹", "File Tree"),
    ("panel.info", "文件信息", "檔案資訊", "File Info"),
    ("panel.keys", "键位", "按鍵", "Keys"),
    // ---- status bar ----
    ("status.label", "状态:", "狀態:", "Status:"),
    ("status.ready", "就绪", "就緒", "Ready"),
    ("status.entered", "进入", "進入", "Entered"),
    ("status.job_running", "已有任务进行中", "已有任務進行中", "A task is already running"),
    ("status.nothing_selected", "没有选中文件", "沒有選中檔案", "Nothing selected"),
    (
        "status.confirm_prompt",
        "确认删除? Enter 删除 / Esc 取消",
        "確認刪除? Enter 刪除 / Esc 取消",
        "Delete? Enter to confirm / Esc to cancel",
    ),
    ("status.delete_cancelled", "已取消删除", "已取消刪除", "Delete cancelled"),
    ("status.compress_start", "开始压缩", "開始壓縮", "Compressing"),
    ("status.error", "错误", "錯誤", "Error"),
    (
        "status.compress_cancelled",
        "已取消正在进行的压缩",
        "已取消正在進行的壓縮",
        "Compression cancelled",
    ),
    ("status.output", "{0}: {1}  大小: {2}", "{0}: {1}  大小: {2}", "{0}: {1}  Size: {2}"),
    (
        "status.compressing",
        "压缩中: {0} {1} (已用 {2}s)",
        "壓縮中: {0} {1} (已用 {2}s)",
        "Compressing: {0} {1} ({2}s)",
    ),
    ("status.size_field", "大小:", "大小:", "Size:"),
    ("status.deleted", "已删除{0}: {1}", "已刪除{0}: {1}", "Deleted {0}: {1}"),
    ("status.delete_failed", "删除失败", "刪除失敗", "Delete failed"),
    (
        "status.config_error",
        "语言配置读取失败，已回退到内置 {0}: {1}",
        "語言設定讀取失敗，已回退到內建 {0}: {1}",
        "Could not read the language config, falling back to built-in {0}: {1}",
    ),
    // ---- extraction ----
    ("status.extract_unknown", "无法识别的格式", "無法識別的格式", "Unrecognized format"),
    (
        "status.nothing_extracted",
        "文件名没有可去掉的压缩扩展名，已中止",
        "檔名沒有可去掉的壓縮副檔名，已中止",
        "The name has no compression extension to strip; aborted",
    ),
    ("status.extract_start", "开始解压: {0}（{1}）", "開始解壓: {0}（{1}）", "Extracting: {0} ({1})"),
    (
        "status.extracting",
        "解压中: {0} {1} (已用 {2}s)",
        "解壓中: {0} {1} (已用 {2}s)",
        "Extracting: {0} {1} ({2}s)",
    ),
    (
        "status.extracted",
        "解压完成: {0} → {1}（{2} 项）",
        "解壓完成: {0} → {1}（{2} 項）",
        "Extracted: {0} → {1} ({2} entries)",
    ),
    (
        "status.extract_cancelled",
        "已取消正在进行的解压",
        "已取消正在進行的解壓",
        "Extraction cancelled",
    ),
    // ---- shared values ----
    ("value.dir", "目录", "目錄", "Directory"),
    ("value.file", "文件", "檔案", "File"),
    ("value.unknown", "未知", "未知", "Unknown"),
    // A directory sum that hit the walk budget is a floor, not a total
    ("value.size_partial", "{0}+", "{0}+", "{0}+"),
    // ---- file info panel ----
    ("info.name", "名称:", "名稱:", "Name:"),
    ("info.type", "类型:", "類型:", "Type:"),
    ("info.size", "大小:", "大小:", "Size:"),
    ("info.mtime", "修改时间:", "修改時間:", "Modified:"),
    ("info.full_path", "完整路径:", "完整路徑:", "Full path:"),
    ("info.empty", "没有选中项目", "沒有選中項目", "Nothing selected"),
    // ---- key hints ----
    // Only the label is translated; the key letters and the format names
    // (tar/zip/wim/7z/zst/gz/xz) are part of the keymap, not copy
    ("key.delete", "删除", "刪除", "Delete"),
    ("key.delete_tiny", "d", "d", "d"),
    ("key.enter", "进入", "進入", "Open"),
    ("key.quit", "退出", "離開", "Quit"),
    ("key.back", "返回", "返回", "Up"),
    ("key.extract", "解压", "解壓", "Extract"),
    // ---- delete confirmation dialog ----
    ("dialog.title", "删除确认", "刪除確認", "Confirm Delete"),
    (
        "dialog.danger_dir",
        "🚨 永久删除，目录里的所有内容都会消失",
        "🚨 永久刪除，目錄裡的所有內容都會消失",
        "🚨 Permanent: everything inside the directory is gone",
    ),
    (
        "dialog.danger_file",
        "🚨 永久删除，无法撤销",
        "🚨 永久刪除，無法復原",
        "🚨 Permanent: this cannot be undone",
    ),
    ("dialog.question_dir", "确定要删除这个目录吗?", "確定要刪除這個目錄嗎?", "Delete this directory?"),
    ("dialog.question_file", "确定要删除这个文件吗?", "確定要刪除這個檔案嗎?", "Delete this file?"),
    ("dialog.target", "目标", "目標", "Target"),
    ("dialog.key_enter", "Enter", "Enter", "Enter"),
    ("dialog.action_delete", "确认删除", "確認刪除", "Delete"),
    ("dialog.key_esc", "Esc", "Esc", "Esc"),
    ("dialog.action_cancel", "取消", "取消", "Cancel"),
    // ---- errors ----
    (
        "error.type_changed",
        "目标已从{0}变成{1}，为避免误删已中止",
        "目標已從{0}變成{1}，為避免誤刪已中止",
        "Target changed from {0} to {1}; aborted to avoid deleting the wrong thing",
    ),
    (
        "error.read_failed",
        "无法读取文件头: {0}（{1}）",
        "無法讀取檔頭: {0}（{1}）",
        "Could not read the header: {0} ({1})",
    ),
    (
        "error.read_too_short",
        "文件太短，不可能是压缩包: {0}",
        "檔案太短，不可能是壓縮檔: {0}",
        "Too short to be an archive: {0}",
    ),
    // ---- compression formats ----
    ("compress.tar.ok", "已打包", "已打包", "Packed"),
    ("compress.tar.fail", "tar 打包失败", "tar 打包失敗", "tar failed"),
    ("compress.zip.ok", "已压缩", "已壓縮", "Compressed"),
    ("compress.zip.fail", "zip 压缩失败", "zip 壓縮失敗", "zip failed"),
    ("compress.wim.ok", "已压缩", "已壓縮", "Compressed"),
    ("compress.wim.fail", "wim 压缩失败（需要管理员权限）", "wim 壓縮失敗（需要管理員權限）", "wim failed (needs administrator rights)"),
    ("compress.7z.ok", "已压缩", "已壓縮", "Compressed"),
    ("compress.7z.fail", "7z 压缩失败", "7z 壓縮失敗", "7z failed"),
    ("compress.zst.ok", "已压缩", "已壓縮", "Compressed"),
    ("compress.zst.fail", "zst 压缩失败", "zst 壓縮失敗", "zst failed"),
    ("compress.gz.ok", "已压缩", "已壓縮", "Compressed"),
    ("compress.gz.fail", "gz 压缩失败", "gz 壓縮失敗", "gz failed"),
    ("compress.xz.ok", "已压缩", "已壓縮", "Compressed"),
    ("compress.xz.fail", "xz 压缩失败", "xz 壓縮失敗", "xz failed"),
    // ---- extraction, one failure key per detected format ----
    ("extract.ok", "已解压", "已解壓", "Extracted"),
    ("extract.fail.gz", "gzip 解压失败", "gzip 解壓失敗", "gzip failed"),
    ("extract.fail.xz", "xz 解压失败", "xz 解壓失敗", "xz failed"),
    ("extract.fail.zst", "zstd 解压失败", "zstd 解壓失敗", "zstd failed"),
    ("extract.fail.bz2", "bzip2 解压失败", "bzip2 解壓失敗", "bzip2 failed"),
    ("extract.fail.tar.gz", "tar.gz 解压失败", "tar.gz 解壓失敗", "tar.gz failed"),
    ("extract.fail.tar.xz", "tar.xz 解压失败", "tar.xz 解壓失敗", "tar.xz failed"),
    ("extract.fail.tar.zst", "tar.zst 解压失败", "tar.zst 解壓失敗", "tar.zst failed"),
    ("extract.fail.tar.bz2", "tar.bz2 解压失败", "tar.bz2 解壓失敗", "tar.bz2 failed"),
    ("extract.fail.tar", "tar 解压失败", "tar 解壓失敗", "tar failed"),
    ("extract.fail.zip", "zip 解压失败", "zip 解壓失敗", "zip failed"),
    ("extract.fail.7z", "7z 解压失败", "7z 解壓失敗", "7z failed"),
    (
        "extract.fail.rar",
        "rar 解压失败（7z 无法处理 RAR5 归档）",
        "rar 解壓失敗（7z 無法處理 RAR5 封存檔）",
        "rar failed (7z cannot read RAR5 archives)",
    ),
    ("extract.fail.cab", "cab 解压失败", "cab 解壓失敗", "cab failed"),
    (
        "extract.fail.wim",
        "wim 解压失败（需要管理员权限）",
        "wim 解壓失敗（需要管理員權限）",
        "wim failed (needs administrator rights)",
    ),
];

/// Parsed `language.json`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Lang {
    /// Active language tag. A tag that is not in `languages` is tolerated:
    /// lookups simply fall through to `DEFAULT_LANG`
    #[serde(default = "default_current")]
    pub(crate) current: String,
    /// Language tag -> (key -> copy). A missing map deserializes to empty
    #[serde(default)]
    pub(crate) languages: HashMap<String, HashMap<String, String>>,
}

fn default_current() -> String {
    DEFAULT_LANG.to_string()
}

impl Default for Lang {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Lang {
    /// Built-in copy, entirely in memory: never reads or writes the filesystem,
    /// so tests can build one freely
    pub(crate) fn builtin() -> Self {
        let mut zh_cn = HashMap::new();
        let mut zh_tw = HashMap::new();
        let mut en_us = HashMap::new();
        for (key, cn, tw, en) in BUILTIN {
            zh_cn.insert((*key).to_string(), (*cn).to_string());
            zh_tw.insert((*key).to_string(), (*tw).to_string());
            en_us.insert((*key).to_string(), (*en).to_string());
        }

        let mut languages = HashMap::new();
        languages.insert("zh-cn".to_string(), zh_cn);
        languages.insert("zh-tw".to_string(), zh_tw);
        languages.insert(DEFAULT_LANG.to_string(), en_us);

        Self {
            current: DEFAULT_LANG.to_string(),
            languages,
        }
    }

    /// Load from `~/.config/lazyzst/language.json`, creating it with the
    /// built-in template when it does not exist yet.
    ///
    /// Never panics. The second element is a status-bar notice for the caller
    /// to show when something had to fall back
    pub(crate) fn load() -> (Self, Option<String>) {
        // No home directory at all: stay in memory rather than guessing a path
        let Some(path) = config_path() else {
            return (Self::builtin(), None);
        };

        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // First run: lay down the template so the file is editable
                let lang = Self::builtin();
                if let Err(err) = write_template(&path, &lang) {
                    let notice = lang.config_notice(&path, &err.to_string());
                    return (lang, Some(notice));
                }
                return (lang, None);
            }
            Err(e) => {
                let lang = Self::builtin();
                let notice = lang.config_notice(&path, &e.to_string());
                return (lang, Some(notice));
            }
        };

        Self::resolve(&text, &path)
    }

    /// Parse config text. Broken JSON yields `None` instead of panicking,
    /// which is the common case when someone hand-edits the file
    fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    /// Text -> (language, optional notice). Anything unparseable falls back to
    /// the built-in copy so the UI still has something to show
    fn resolve(text: &str, path: &Path) -> (Self, Option<String>) {
        // An empty file parses as "no current language" only if it is `{}`;
        // blank text is a syntax error, and treating it as one keeps the
        // "someone half-wrote the file" case on the same recovery path
        if let Some(lang) = Self::parse(text) {
            return (lang, None);
        }
        let lang = Self::builtin();
        let notice = lang.config_notice(path, "invalid JSON");
        (lang, Some(notice))
    }

    /// Human-readable notice about a config problem, already localized
    fn config_notice(&self, path: &Path, reason: &str) -> String {
        self.tf(
            "status.config_error",
            &[
                self.current(),
                &format!("{}: {}", path.display(), reason),
            ],
        )
    }

    /// Active language tag
    pub(crate) fn current(&self) -> &str {
        &self.current
    }

    /// Look up copy for `key`.
    ///
    /// Fallback chain: active language -> `en-us` -> the key itself. Empty
    /// translations count as missing, so no level can produce a blank or
    /// half-written label
    pub(crate) fn t(&self, key: &str) -> String {
        self.lookup(&self.current, key)
            .or_else(|| self.lookup(DEFAULT_LANG, key))
            .unwrap_or_else(|| key.to_string())
    }

    /// Look up copy and substitute `{0}`, `{1}`, ... placeholders.
    /// Placeholders without a matching argument are left untouched
    pub(crate) fn tf(&self, key: &str, args: &[&str]) -> String {
        fill_placeholders(&self.t(key), args)
    }

    fn lookup(&self, lang: &str, key: &str) -> Option<String> {
        self.languages
            .get(lang)?
            .get(key)
            .filter(|text| !text.is_empty())
            .cloned()
    }
}

/// `<home>/.config/lazyzst/language.json`. `USERPROFILE` first, then `HOME`
fn config_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .ok()
        .or_else(|| std::env::var("HOME").ok())
        .filter(|h| !h.is_empty())?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("lazyzst")
            .join("language.json"),
    )
}

/// Create the config directory and write the template out
fn write_template(path: &Path, lang: &Lang) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(lang)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, json)
}

/// Replace `{0}`, `{1}`, ... with `args`; unknown or out-of-range indices stay
/// as written so a mistyped template is visible instead of silently dropped
fn fill_placeholders(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        out.push('{');
        // `rest` shrinks by at least one byte per iteration, so this terminates
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            rest = after;
            continue;
        };
        let digits = &after[..close];
        match digits.parse::<usize>().ok().and_then(|i| args.get(i)) {
            Some(value) => out.push_str(value),
            None => {
                out.push_str(digits);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Removes the scratch directory on drop, whether the test passes or panics
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// One directory per test: temp dir + process id + counter, so parallel
    /// tests never collide. Distinct prefix from the app.rs guard
    fn scratch(tag: &str) -> Scratch {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "lazyzst-i18n-test-{}-{tag}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    #[test]
    fn missing_key_in_current_language_falls_back_to_english() {
        let mut lang = Lang::builtin();
        lang.current = "zh-cn".to_string();
        lang.languages
            .get_mut("zh-cn")
            .unwrap()
            .remove("panel.path");

        assert_eq!(lang.t("panel.path"), "Path");
    }

    #[test]
    fn key_missing_everywhere_returns_the_key_itself() {
        let lang = Lang::builtin();
        assert_eq!(lang.t("no.such.key"), "no.such.key");
    }

    #[test]
    fn unknown_current_language_does_not_panic() {
        let mut lang = Lang::builtin();
        lang.current = "fr-fr".to_string();

        assert_eq!(lang.t("panel.tree"), "File Tree");
        assert_eq!(lang.t("missing.key"), "missing.key");
    }

    #[test]
    fn empty_translation_counts_as_missing() {
        let mut lang = Lang::builtin();
        lang.current = "zh-cn".to_string();
        lang.languages
            .get_mut("zh-cn")
            .unwrap()
            .insert("panel.path".to_string(), String::new());

        assert_eq!(lang.t("panel.path"), "Path");
    }

    #[test]
    fn broken_json_falls_back_to_builtin_without_panicking() {
        let (lang, notice) = Lang::resolve("{ this is not json", Path::new("language.json"));

        assert_eq!(lang, Lang::builtin(), "must fall back to the built-in copy");
        let notice = notice.expect("a broken config should report a notice");
        assert!(!notice.is_empty());
        // The notice is itself localized, so it must not contain raw placeholders
        assert!(!notice.contains("{0}"), "notice={notice}");
    }

    #[test]
    fn empty_languages_map_still_yields_keys_without_panicking() {
        let lang = Lang::parse(r#"{"current":"en-us","languages":{}}"#).expect("valid JSON");
        assert_eq!(lang.t("panel.info"), "panel.info");
    }

    #[test]
    fn missing_fields_deserialize_to_defaults() {
        // No `current`, no `languages`: serde(default) keeps it from failing
        let lang = Lang::parse("{}").expect("an empty object is valid");
        assert_eq!(lang.current(), DEFAULT_LANG);
        assert!(lang.languages.is_empty());
        assert_eq!(lang.t("panel.keys"), "panel.keys");
    }

    #[test]
    fn round_trip_through_json_keeps_every_key() {
        let s = scratch("roundtrip");
        let path = s.0.join("nested").join("language.json");

        let builtin = Lang::builtin();
        write_template(&path, &builtin).expect("write template");

        let text = fs::read_to_string(&path).expect("read template back");
        let loaded = Lang::parse(&text).expect("template must parse");
        assert_eq!(loaded, builtin);

        for tag in ["zh-cn", "zh-tw", "en-us"] {
            let keys = builtin.languages.get(tag).expect("language present");
            assert_eq!(keys.len(), BUILTIN.len(), "{tag} has a different key count");
            for (key, _, _, _) in BUILTIN {
                assert!(keys.contains_key(*key), "{tag} is missing {key}");
                assert!(!keys[*key].is_empty(), "{tag} has an empty {key}");
            }
        }
    }

    #[test]
    fn every_language_has_exactly_the_same_keys() {
        let lang = Lang::builtin();
        let mut tags: Vec<&String> = lang.languages.keys().collect();
        tags.sort();
        assert_eq!(tags, vec!["en-us", "zh-cn", "zh-tw"]);

        let expected: std::collections::HashSet<&str> = BUILTIN.iter().map(|(k, ..)| *k).collect();
        for tag in tags {
            let keys: std::collections::HashSet<&str> = lang.languages[tag].keys().map(String::as_str).collect();
            assert_eq!(keys, expected, "{tag} key set drifted from the table");
        }
        assert_eq!(expected.len(), BUILTIN.len(), "duplicate keys in the table");
    }

    #[test]
    fn placeholders_are_filled_positionally() {
        let lang = Lang::builtin();
        let out = lang.tf("status.deleted", &["File", r"D:\a.txt"]);
        assert!(!out.contains("{0}"), "{out}");
        assert!(!out.contains("{1}"), "{out}");
        assert!(out.contains(r"D:\a.txt"), "{out}");

        // Out-of-range or non-numeric indices stay verbatim instead of vanishing
        let raw = lang.tf("status.deleted", &["File"]);
        assert!(raw.contains("{1}"), "{raw}");
    }

    #[test]
    fn builtin_does_not_need_the_filesystem() {
        // Same content every call, and identical to what gets written out
        assert_eq!(Lang::builtin(), Lang::default());
        assert_eq!(Lang::builtin().current(), "en-us");
    }

    #[test]
    fn config_path_points_at_the_documented_location() {
        // Nothing to assert in an environment without a home directory; the
        // rest of the loading path handles that case
        let Some(path) = config_path() else {
            return;
        };
        let dir = path.parent().expect("the config lives in a directory");
        assert_eq!(path.file_name().unwrap(), "language.json", "{path:?}");
        assert_eq!(dir.file_name().unwrap(), "lazyzst", "{dir:?}");
        assert_eq!(dir.parent().unwrap().file_name().unwrap(), ".config");
    }
}