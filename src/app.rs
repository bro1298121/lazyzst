use std::{
    fs,
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, Instant},
};

// Do NOT `use anyhow::Result` here: `read_dir` relies on `Result::ok`, and
// shadowing it with anyhow's Result breaks the build.
use anyhow::Context;

use crate::{
    compress::{build_command, format_spec},
    i18n::Lang,
    ui::format_size,
};

/// Rows visible in the file list
pub(crate) const VISIBLE_ROWS: usize = 20;
/// Main-loop tick: drives the progress animation and caps idle CPU use
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// Animation step; wraps around once it passes 100
pub(crate) const PROGRESS_STEP: u16 = 3;

/// A running compression job. The child process is spawned without blocking
/// on the main thread and its handle stays here, so quitting can kill + wait
/// on it instead of leaving an orphan behind
pub(crate) struct JobState {
    pub(crate) format: String,
    pub(crate) target: PathBuf,
    pub(crate) started: Instant,
    /// Animated indeterminate progress (external tools report no real percentage)
    pub(crate) progress: u16,
    /// Child process handle
    pub(crate) child: Child,
    /// Output path, used to build the completion message
    pub(crate) out: PathBuf,
}

/// Info about the most recent successful output; lets the UI elide the path
/// when the terminal is too narrow
pub(crate) struct OutputInfo {
    pub(crate) prefix: String,
    pub(crate) path: PathBuf,
    pub(crate) size: String,
}

/// What a dialog does. Deletion is the only action so far; add a variant here
/// and the App-side flow stays as it is
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DialogAction {
    /// Permanent, irreversible deletion of a file or directory
    Delete,
}

/// Modal dialog state. While it is `None`, keys drive the main view; while it
/// is `Some`, focus belongs to the dialog and only Enter / Esc get through
pub(crate) struct Dialog {
    pub(crate) action: DialogAction,
    /// Target locked in at open time. Both the on-screen copy and the actual
    /// deletion use it, so what the user sees is always what gets deleted
    pub(crate) target: PathBuf,
    /// Kind of the target when the dialog opened; drives both the wording and
    /// how the removal is performed
    pub(crate) is_dir: bool,
}

pub(crate) struct App {
    /// Active language table. Owned by App rather than a global so tests can
    /// swap in `Lang::builtin()` without touching a shared singleton
    pub(crate) lang: Lang,
    pub(crate) current_dir: PathBuf,
    pub(crate) entries: Vec<PathBuf>,
    pub(crate) selected: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) status: String,
    pub(crate) job: Option<JobState>,
    /// When set, the status bar shows "output + size" and elides the path to
    /// the terminal width
    pub(crate) last_out: Option<OutputInfo>,
    /// Modal dialog; while open, keys are limited to Enter / Esc
    pub(crate) dialog: Option<Dialog>,
}

impl App {
    /// `lang` is passed in rather than loaded here, so tests can build an App
    /// without reading the user's config file
    pub(crate) fn new(lang: Lang) -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let entries = Self::read_dir(&current_dir);
        Self {
            status: lang.t("status.ready"),
            lang,
            current_dir,
            entries,
            selected: 0,
            scroll_offset: 0,
            job: None,
            last_out: None,
            dialog: None,
        }
    }

    fn read_dir(path: &Path) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by(|a, b| {
            let a_dir = a.is_dir();
            let b_dir = b.is_dir();
            b_dir.cmp(&a_dir).then(a.file_name().cmp(&b.file_name()))
        });
        entries
    }

    pub(crate) fn enter_dir(&mut self) {
        let entry = self.entries.get(self.selected).cloned();
        if let Some(entry) = entry
            && entry.is_dir()
        {
            let path = entry.clone();
            self.current_dir = path;
            self.entries = Self::read_dir(&self.current_dir);
            self.selected = 0;
            self.scroll_offset = 0;
            // Different directory: the previous output is no longer relevant
            self.last_out = None;
            self.status = format!("{} {}", self.lang.t("status.entered"), entry.display());
        }
    }

    pub(crate) fn go_up(&mut self) {
        let parent = self.current_dir.parent().map(|p| p.to_path_buf());
        if let Some(parent) = parent {
            self.current_dir = parent.clone();
            self.entries = Self::read_dir(&self.current_dir);
            self.selected = 0;
            self.scroll_offset = 0;
            // Different directory: the previous output is no longer relevant
            self.last_out = None;
            self.status = format!("{} {}", self.lang.t("status.entered"), parent.display());
        }
    }

    pub(crate) fn get_selected_path(&self) -> Option<PathBuf> {
        self.entries.get(self.selected).cloned()
    }

    /// `d`: open the delete confirmation. When the preconditions fail it only
    /// reports a status and never opens the dialog
    pub(crate) fn request_delete(&mut self) {
        // While a compression runs, the target is being read by the child
        // process; deleting it now would pull the source out from under it
        if self.job.is_some() {
            self.status = self.lang.t("status.job_running");
            return;
        }

        let Some(path) = self.get_selected_path() else {
            self.status = self.lang.t("status.nothing_selected");
            return;
        };

        // The kind is decided once, here: both the wording and the removal use
        // it, so nothing can drift halfway through the dialog
        self.dialog = Some(Dialog {
            action: DialogAction::Delete,
            is_dir: path.is_dir(),
            target: path,
        });
        self.status = self.lang.t("status.confirm_prompt");
    }

    /// `Enter` inside the dialog: run the action and close the dialog either way
    pub(crate) fn confirm_dialog(&mut self) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };

        match dialog.action {
            DialogAction::Delete => {
                let target = dialog.target;
                // Delete strictly by the kind locked in when the dialog opened;
                // do not re-derive it here
                match remove_path(&self.lang, &target, dialog.is_dir) {
                    Ok(()) => {
                        // One entry is gone: re-read and pull the selection back
                        // into range
                        self.entries = Self::read_dir(&self.current_dir);
                        self.clamp_selection();
                        // The previous output info is stale and would cover up
                        // the delete result
                        self.last_out = None;
                        let kind = if dialog.is_dir {
                            self.lang.t("value.dir")
                        } else {
                            self.lang.t("value.file")
                        };
                        self.status = self.lang.tf(
                            "status.deleted",
                            &[&kind, &target.display().to_string()],
                        );
                    }
                    Err(e) => {
                        // Read-only or in-use files fail on Windows: surface the
                        // error in the status bar, never panic
                        self.last_out = None;
                        self.status =
                            format!("{}: {}", self.lang.t("status.delete_failed"), e);
                    }
                }
            }
        }
    }

    /// `Esc` inside the dialog: close it and delete nothing
    pub(crate) fn dismiss_dialog(&mut self) {
        if self.dialog.take().is_none() {
            return;
        }
        // Cancelling is a deliberate action too: give it an acknowledgement so
        // it does not look like the key press was swallowed
        self.last_out = None;
        self.status = self.lang.t("status.delete_cancelled");
    }

    pub(crate) fn compress(&mut self, format: &str) {
        // Only one background job at a time
        if self.job.is_some() {
            self.status = self.lang.t("status.job_running");
            return;
        }

        let Some(path) = self.get_selected_path() else {
            self.status = self.lang.t("status.nothing_selected");
            return;
        };

        let (mut cmd, out) = match build_command(format, &path) {
            Ok(built) => built,
            Err(e) => {
                self.status = format!("{}: {}", self.lang.t("status.error"), e);
                return;
            }
        };

        // Spawn without blocking; the handle stays on this thread so quitting
        // can reap it
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.status = format!("{}: {}", self.lang.t("status.error"), e);
                return;
            }
        };

        self.job = Some(JobState {
            format: format.to_string(),
            target: path,
            started: Instant::now(),
            progress: 0,
            child,
            out,
        });
        // The previous output info is stale; the status bar shows the progress
        // bar while the new job runs
        self.last_out = None;
        self.status = format!("{}: {}", self.lang.t("status.compress_start"), format);
    }

    /// Advance the progress animation once per tick (indeterminate, looping)
    pub(crate) fn tick_job(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.progress = (job.progress + PROGRESS_STEP) % 101;
        }
    }

    /// Poll the child process; `try_wait` does not block, and the list is
    /// refreshed with the selection clamped once the job ends
    pub(crate) fn poll_job(&mut self) {
        // At most one try_wait call per frame
        let waited = match self.job.as_mut() {
            Some(job) => job.child.try_wait(),
            None => return,
        };

        let status = match waited {
            Ok(Some(status)) => status,
            Ok(None) => return,
            Err(e) => {
                // Reap the child so it cannot be orphaned
                self.cancel_job();
                // The job failed: drop the stale output info, otherwise it
                // would cover up the error message
                self.last_out = None;
                self.status = format!("{}: {}", self.lang.t("status.error"), e);
                return;
            }
        };

        let Some(job) = self.job.take() else {
            return;
        };
        let (ok_prefix, fail_msg) = format_spec(&self.lang, &job.format);
        if status.success() {
            // The output may have been moved or deleted meanwhile; fall back to
            // "unknown" when its size cannot be read
            let size = fs::metadata(&job.out)
                .map(|meta| format_size(meta.len()))
                .unwrap_or_else(|_| self.lang.t("value.unknown"));
            self.last_out = Some(OutputInfo {
                prefix: ok_prefix.clone(),
                path: job.out.clone(),
                size: size.clone(),
            });
            self.status = self.lang.tf(
                "status.output",
                &[&ok_prefix, &job.out.display().to_string(), &size],
            );
        } else {
            // On failure keep no output info, so the status bar shows the
            // failure message instead
            self.last_out = None;
            self.status = fail_msg;
        }
        // The directory gained a new output: re-read it and stay in range
        self.entries = Self::read_dir(&self.current_dir);
        self.clamp_selection();
    }

    /// Stop the running job: `kill` must be followed by `wait`, otherwise the
    /// process is left behind as a zombie
    pub(crate) fn cancel_job(&mut self) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        let _ = job.child.kill();
        let _ = job.child.wait();
        // After a cancel the previous output must not linger; the status bar
        // switches to the cancel message
        self.last_out = None;
        self.status = self.lang.t("status.compress_cancelled");
    }

    fn clamp_selection(&mut self) {
        if self.entries.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        if self.selected >= self.entries.len() {
            self.selected = self.entries.len() - 1;
        }
        let max_offset = self.entries.len().saturating_sub(VISIBLE_ROWS);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        }
        if self.selected >= self.scroll_offset + VISIBLE_ROWS {
            self.scroll_offset = self.selected + 1 - VISIBLE_ROWS;
        }
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }
    }
}

/// Delete by the kind that was locked in when the dialog opened.
///
/// Deletion is irreversible, so the kind is **not re-derived** here and `is_dir`
/// is obeyed literally. If the re-check finds that the target changed kind (a
/// file swapped for a directory while the dialog was open, say), return an error
/// and abort: better to fail than to delete a whole tree the user never agreed to.
///
/// The re-check deliberately reuses `Path::is_dir()` (which follows symlinks) so
/// it matches the judgement made by `request_delete`; otherwise a symlink to a
/// directory would read as "the kind changed" and could never be deleted.
/// `lang` is only used to word that error, which reaches the user through the
/// status bar.
pub(crate) fn remove_path(lang: &Lang, path: &Path, is_dir: bool) -> anyhow::Result<()> {
    // An already-missing path is not a kind change: let the removal below report
    // the real "no such file" error
    if path.exists() && path.is_dir() != is_dir {
        let expect = if is_dir {
            lang.t("value.dir")
        } else {
            lang.t("value.file")
        };
        let actual = if path.is_dir() {
            lang.t("value.dir")
        } else {
            lang.t("value.file")
        };
        anyhow::bail!(
            "{}",
            lang.tf("error.type_changed", &[&expect, &actual])
        );
    }

    let result = if is_dir {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    // Carry the path along: on Windows, "access denied" or "file in use" is
    // meaningless without knowing which file it was about
    result.with_context(|| path.display().to_string())
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

    /// One directory per test: named with the process id plus a counter so
    /// parallel runs cannot collide; a leftover of the same name is cleared first
    fn scratch(tag: &str) -> Scratch {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("lazyzst-test-{}-{tag}-{n}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    /// Point the selection at the scratch directory so the tests never touch
    /// the real listing. `Lang::builtin()` keeps them off the filesystem
    fn app_with_target(dir: &Path, target: PathBuf) -> App {
        let mut app = App::new(Lang::builtin());
        app.current_dir = dir.to_path_buf();
        app.entries = vec![target];
        app.selected = 0;
        app.scroll_offset = 0;
        app
    }

    #[test]
    fn removes_a_file_and_it_is_gone() {
        let s = scratch("file");
        let dir = &s.0;
        let file = dir.join("note.txt");
        fs::write(&file, b"hello").unwrap();
        assert!(file.is_file());

        remove_path(&Lang::builtin(), &file, false).expect("removing a file should work");

        assert!(!file.exists(), "the file should be gone");
        assert!(dir.exists(), "only the file goes; the parent stays");
    }

    #[test]
    fn removes_a_directory_with_all_its_contents() {
        let s = scratch("tree");
        let dir = &s.0;
        fs::create_dir_all(dir.join("sub/deep")).unwrap();
        fs::write(dir.join("sub/deep/leaf.bin"), b"x").unwrap();
        fs::write(dir.join("sub/top.txt"), b"y").unwrap();
        let leaf = dir.join("sub/deep/leaf.bin");
        assert!(leaf.is_file());

        remove_path(&Lang::builtin(), dir, true).expect("removing a directory should work");

        assert!(!dir.exists(), "the directory tree should be gone");
        assert!(!leaf.exists(), "nested files go with it");
    }

    #[test]
    fn missing_path_returns_err_instead_of_panicking() {
        let s = scratch("missing");
        let dir = &s.0;
        let ghost = dir.join("never-created.txt");

        let err = remove_path(&Lang::builtin(), &ghost, false).expect_err("a missing path must be an Err");

        // The error has to carry the path, otherwise the status bar has nothing
        // to report
        assert!(
            err.to_string().contains(&ghost.display().to_string()),
            "the error should name the path: {err}"
        );
        assert!(dir.exists(), "a failure must not remove the parent");
    }

    #[test]
    fn file_replaced_by_a_directory_aborts_instead_of_deleting_the_tree() {
        let s = scratch("swap-dir");
        let dir = &s.0;
        let p = dir.join("target");
        // A file at the moment the dialog opens
        fs::write(&p, b"x").unwrap();
        assert!(!p.is_dir());

        // Swapped for a directory while the dialog is open, with a file inside
        fs::remove_file(&p).unwrap();
        fs::create_dir(&p).unwrap();
        fs::write(p.join("leaf.txt"), b"y").unwrap();

        let lang = Lang::builtin();
        let err = remove_path(&lang, &p, false).expect_err("a changed kind must abort");
        assert!(
            err.to_string().contains(&lang.t("value.dir")),
            "the error should name the actual kind: {err}"
        );
        assert!(p.exists(), "the target itself must survive a kind mismatch");
        assert!(
            p.join("leaf.txt").exists(),
            "the worst case here is deleting a whole tree; it must stay intact"
        );
    }

    #[test]
    fn directory_replaced_by_a_file_aborts() {
        let s = scratch("swap-file");
        let dir = &s.0;
        let p = dir.join("target");
        // A directory at the moment the dialog opens
        fs::create_dir(&p).unwrap();
        fs::write(p.join("leaf.txt"), b"y").unwrap();
        assert!(p.is_dir());

        // The directory is removed and a file takes its place while the dialog
        // is open
        fs::remove_dir_all(&p).unwrap();
        fs::write(&p, b"z").unwrap();

        let lang = Lang::builtin();
        let err = remove_path(&lang, &p, true).expect_err("a changed kind must abort");
        assert!(
            err.to_string().contains(&lang.t("value.file")),
            "the error should name the actual kind: {err}"
        );
        assert!(p.exists(), "nothing may be deleted on a kind mismatch");
    }

    #[test]
    fn escape_dismisses_dialog_without_deleting() {
        let s = scratch("esc");
        let file = s.0.join("keep.txt");
        fs::write(&file, b"x").unwrap();
        let mut app = app_with_target(&s.0, file.clone());
        let lang = Lang::builtin();

        app.request_delete();
        assert!(app.dialog.is_some(), "the dialog should open");
        assert_eq!(app.status, lang.t("status.confirm_prompt"));

        app.dismiss_dialog();

        assert!(app.dialog.is_none(), "Esc should close the dialog");
        assert!(file.exists(), "Esc must never delete the file");
        assert_eq!(app.status, lang.t("status.delete_cancelled"));
    }

    #[test]
    fn confirm_deletes_the_target_and_refreshes_the_list() {
        let s = scratch("confirm");
        let file = s.0.join("gone.txt");
        fs::write(&file, b"x").unwrap();
        let mut app = app_with_target(&s.0, file.clone());
        let lang = Lang::builtin();
        app.last_out = Some(OutputInfo {
            prefix: lang.t("compress.zst.ok"),
            path: s.0.join("old.zst"),
            size: "1 KB".to_string(),
        });

        app.request_delete();
        let dialog = app.dialog.as_ref().expect("the dialog should open");
        assert_eq!(dialog.action, DialogAction::Delete);
        assert!(!dialog.is_dir, "a scratch file should read as a file");
        assert_eq!(dialog.target, file);

        app.confirm_dialog();

        assert!(app.dialog.is_none(), "confirming should close the dialog");
        assert!(!file.exists(), "the confirmed file should be deleted");
        assert!(app.last_out.is_none(), "the stale output info must be cleared");
        assert_eq!(
            app.status,
            lang.tf(
                "status.deleted",
                &[&lang.t("value.file"), &file.display().to_string()]
            ),
            "status={}",
            app.status
        );
    }

    #[test]
    fn dialog_marks_directory_targets_as_directories() {
        let s = scratch("isdir");
        let child = s.0.join("child");
        fs::create_dir_all(&child).unwrap();
        let mut app = app_with_target(&s.0, child.clone());

        app.request_delete();
        let dialog = app.dialog.as_ref().expect("the dialog should open");
        assert!(dialog.is_dir, "a directory should read as a directory");
        assert_eq!(dialog.target, child);

        app.dismiss_dialog();
        assert!(child.is_dir(), "cancelling must leave directories alone");
    }

    #[test]
    fn nothing_selected_reports_status_without_opening_dialog() {
        let s = scratch("nosel");
        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.entries.clear();
        app.selected = 0;

        app.request_delete();

        assert!(app.dialog.is_none(), "with nothing selected no dialog appears");
        assert_eq!(app.status, app.lang.t("status.nothing_selected"));
    }

    #[test]
    fn status_copy_follows_the_active_language() {
        let s = scratch("lang");
        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.entries.clear();

        // Built-in copy is English by default
        assert_eq!(app.status, "Ready");

        // Switching the table changes what the status line says; the literal
        // below is the zh-cn entry, spelled out on purpose to pin that table
        app.lang.current = "zh-cn".to_string();
        app.request_delete();
        assert_eq!(app.status, "没有选中文件");
    }
}