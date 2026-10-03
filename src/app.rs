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
    compress::{build_command, format_spec, same_parent},
    extract::{build_extract_command, detect_format, extract_target, probe_nested, Format},
    i18n::Lang,
    ui::format_size,
};

/// Rows visible in the file list
pub(crate) const VISIBLE_ROWS: usize = 20;
/// Main-loop tick: drives the progress animation and caps idle CPU use
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// Animation step; wraps around once it passes 100
pub(crate) const PROGRESS_STEP: u16 = 3;

/// What a running job is doing, so the completion copy and the Gauge label can
/// branch without the compression path having to know about extraction.
///
/// The detected format travels with the extraction case: it decides both the
/// wording and whether the result is a single payload or a whole directory
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum JobKind {
    /// Compressing into a new archive, named by the keymap
    Compress,
    /// Unpacking a detected archive into the directory being browsed
    Extract(Format),
}

impl JobKind {
    /// Whether the job is an extraction at all
    pub(crate) fn is_extract(self) -> bool {
        matches!(self, JobKind::Extract(_))
    }
}

/// A running compression job. The child process is spawned without blocking
/// on the main thread and its handle stays here, so quitting can kill + wait
/// on it instead of leaving an orphan behind
pub(crate) struct JobState {
    /// Compression or extraction; decides the completion copy
    pub(crate) kind: JobKind,
    /// Format name, taken from the keymap when compressing and from magic-byte
    /// detection when extracting
    pub(crate) format: String,
    pub(crate) target: PathBuf,
    /// What the Gauge names while the job runs. A compression shows the input it
    /// was given, or the archive being written when there are several
    pub(crate) label: String,
    pub(crate) started: Instant,
    /// Animated indeterminate progress (external tools report no real percentage)
    pub(crate) progress: u16,
    /// Child process handle
    pub(crate) child: Child,
    /// Output path, used to build the completion message
    pub(crate) out: PathBuf,
    /// How many entries the directory held when the job started, so the
    /// extraction copy can report how many landed there
    pub(crate) entries_before: usize,
}

/// Info about the most recent successful output; lets the UI elide the path
/// when the terminal is too narrow
pub(crate) struct OutputInfo {
    pub(crate) prefix: String,
    pub(crate) path: PathBuf,
    pub(crate) size: String,
}

/// What the marked set adds up to.
///
/// The two phrases are kept apart rather than glued together because the UI drops
/// the breakdown when its row is too narrow for it, while the status line keeps
/// both. Counting once and rendering twice is what keeps the corner counter and
/// the status text from ever disagreeing
#[derive(Clone, Copy, Debug)]
pub(crate) struct MarkedSummary {
    /// Marked entries in total, files and directories together
    pub(crate) total: usize,
    /// How many of them are files
    pub(crate) files: usize,
    /// How many of them are directories
    pub(crate) dirs: usize,
}

impl MarkedSummary {
    /// Short phrase: `已标记 3 项`
    pub(crate) fn count(&self, lang: &Lang) -> String {
        lang.tf("status.marked_count", &[&self.total.to_string()])
    }

    /// Breakdown only: `（2 文件 · 1 文件夹）`. It carries its own leading space
    /// and brackets, because where punctuation goes around an interpolated count
    /// is a question each language answers differently
    pub(crate) fn detail(&self, lang: &Lang) -> String {
        lang.tf(
            "status.marked_files_dirs",
            &[&self.files.to_string(), &self.dirs.to_string()],
        )
    }

    /// Both halves joined, for the status line and for a row wide enough
    pub(crate) fn full(&self, lang: &Lang) -> String {
        format!("{}{}", self.count(lang), self.detail(lang))
    }
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
    /// Size shown for the selected entry, kept between frames. `ui` redraws on
    /// a timer and measuring a directory means walking it, so the result is held
    /// until the selection moves or the listing is refreshed
    pub(crate) size_cache: Option<(PathBuf, String)>,
    /// Paths the user has marked, in the order they were marked.
    ///
    /// A `Vec` and not a set: the order decides the argument order of the batch
    /// command, and that order has to come out the same on every run rather than
    /// depending on a hash. Stored as absolute paths, so a mark survives
    /// navigating away and back, and is unrelated to the listing it was made from
    pub(crate) marked: Vec<PathBuf>,
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
            size_cache: None,
            marked: Vec::new(),
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

    /// Re-read the listing of the current directory.
    ///
    /// Every path that re-reads the listing goes through here so that the
    /// cached size of the selected entry gets dropped with it; a file that was
    /// just compressed, extracted or deleted would otherwise keep reporting the
    /// size it had before the change
    fn refresh_entries(&mut self) {
        self.entries = Self::read_dir(&self.current_dir);
        self.size_cache = None;
    }

    pub(crate) fn enter_dir(&mut self) {
        let entry = self.entries.get(self.selected).cloned();
        if let Some(entry) = entry
            && entry.is_dir()
        {
            let path = entry.clone();
            self.current_dir = path;
            self.refresh_entries();
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
            self.refresh_entries();
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

    /// What a compression acts on: the marked entries when there are any,
    /// otherwise whatever is selected, which is what keeps the plain
    /// single-target keys working exactly as before.
    ///
    /// The marked paths come back in the order they were marked
    pub(crate) fn targets(&self) -> Vec<PathBuf> {
        if self.marked.is_empty() {
            self.get_selected_path().into_iter().collect()
        } else {
            self.marked.clone()
        }
    }

    /// What the marked set adds up to, or `None` when nothing is marked
    pub(crate) fn marked_summary(&self) -> Option<MarkedSummary> {
        if self.marked.is_empty() {
            return None;
        }
        let dirs = self.marked.iter().filter(|p| p.is_dir()).count();
        Some(MarkedSummary {
            total: self.marked.len(),
            files: self.marked.len() - dirs,
            dirs,
        })
    }

    /// `Space`: mark the selected entry, or take the mark off again
    pub(crate) fn toggle_mark(&mut self) {
        let Some(path) = self.get_selected_path() else {
            self.status = self.lang.t("status.nothing_selected");
            return;
        };

        match self.marked.iter().position(|p| p == &path) {
            Some(at) => {
                // Remove by position: the remaining marks keep the order they
                // were made in, which the batch command's arguments inherit
                self.marked.remove(at);
                self.status = self.lang.tf("status.unmarked", &[&self.marked.len().to_string()]);
            }
            None => {
                self.marked.push(path);
                self.status = self.marked_summary()
                    .map(|s| s.full(&self.lang))
                    .unwrap_or_else(|| self.lang.t("status.ready"));
            }
        }
    }

    /// `A`: mark everything the current directory holds, directories included.
    /// Already-marked entries keep their place in the order, so re-running this
    /// does not reshuffle the batch
    pub(crate) fn mark_all(&mut self) {
        if self.entries.is_empty() {
            self.status = self.lang.t("status.nothing_selected");
            return;
        }

        // Gather first, then append: entries that are already marked keep the
        // slot they were given, so running this twice cannot reshuffle the batch
        let fresh: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|p| !self.marked.contains(p))
            .cloned()
            .collect();
        if fresh.is_empty() {
            self.status = self.lang.t("status.already_marked");
            return;
        }
        self.marked.extend(fresh);
        self.status = self.marked_summary()
            .map(|s| s.full(&self.lang))
            .unwrap_or_else(|| self.lang.t("status.ready"));
    }

    /// `u`: take the mark off the selected entry. Not an undo stack: it only
    /// ever touches whatever is under the cursor
    pub(crate) fn unmark_selected(&mut self) {
        let Some(path) = self.get_selected_path() else {
            self.status = self.lang.t("status.nothing_selected");
            return;
        };
        let Some(at) = self.marked.iter().position(|p| p == &path) else {
            self.status = self.lang.t("status.not_marked");
            return;
        };
        self.marked.remove(at);
        self.status = self.lang.tf("status.unmarked", &[&self.marked.len().to_string()]);
    }

    /// `U`: drop every mark at once
    pub(crate) fn clear_marks(&mut self) {
        if self.marked.is_empty() {
            self.status = self.lang.t("status.nothing_marked");
            return;
        }
        let had = self.marked.len();
        self.marked.clear();
        self.status = self.lang.tf("status.marks_cleared", &[&had.to_string()]);
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
                        self.refresh_entries();
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

        // Marks win over the selection: they are how a batch is built up, and
        // with none of them this is the single-target path it always was
        let targets = self.targets();
        if targets.is_empty() {
            self.status = self.lang.t("status.nothing_selected");
            return;
        }

        // wim captures one directory. Its `dism` invocation names a single
        // capture dir, so a batch would write an image of whichever entry it
        // read first and report success while losing the rest
        if targets.len() > 1 && format == "wim" {
            self.status = self.lang.t("status.wim_batch_unsupported");
            return;
        }

        // One archive lives beside its members and every tool is handed bare
        // member names, so entries from two directories have no common spelling
        if targets.len() > 1 && !same_parent(&targets) {
            self.status = self.lang.t("status.marks_mixed_dirs");
            return;
        }

        let (mut cmd, out) = match build_command(format, &targets) {
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

        // A batch has no single input worth showing while it runs, so the Gauge
        // names the archive being written instead
        let named = if targets.len() == 1 { &targets[0] } else { &out };
        let label = named
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        self.job = Some(JobState {
            kind: JobKind::Compress,
            format: format.to_string(),
            target: targets[0].clone(),
            label,
            started: Instant::now(),
            progress: 0,
            child,
            out,
            entries_before: self.entries.len(),
        });
        // The previous output info is stale; the status bar shows the progress
        // bar while the new job runs
        self.last_out = None;
        self.status = format!("{}: {}", self.lang.t("status.compress_start"), format);
    }

    /// `e`: extract the selected file into the directory being browsed.
    ///
    /// Detection reads the leading bytes on this thread, which is a short read
    /// and keeps the interactive part of the flow immediate
    pub(crate) fn extract(&mut self) {
        // Only one background job at a time
        if self.job.is_some() {
            self.status = self.lang.t("status.job_running");
            return;
        }

        let Some(path) = self.get_selected_path() else {
            self.status = self.lang.t("status.nothing_selected");
            return;
        };

        // `detect` reports why it gave up, so there is nothing to add here
        let Some(format) = self.detect(&path) else {
            return;
        };

        // The detection above found a stream compressor; look inside it to see
        // whether it holds a tar rather than a lone file. `format` is plain
        // Copy data, so nothing here touches `self`
        let format = probe_nested(&path, format);

        let dest = self.current_dir.clone();
        let mut cmd = match build_extract_command(&self.lang, format, &path, &dest) {
            Ok(cmd) => cmd,
            Err(e) => {
                self.status = format!("{}: {}", self.lang.t("status.error"), e);
                return;
            }
        };

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.status = format!("{}: {}", self.lang.t("status.error"), e);
                return;
            }
        };

        let out = extract_target(format, &path, &dest);
        let name = path.display().to_string();
        let label = format.to_string();
        let gauge_label = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        self.job = Some(JobState {
            kind: JobKind::Extract(format),
            format: label.clone(),
            target: path,
            label: gauge_label,
            started: Instant::now(),
            progress: 0,
            child,
            out,
            entries_before: self.entries.len(),
        });
        self.last_out = None;
        // Name the detected format so the user can see what was recognized
        self.status = self.lang.tf("status.extract_start", &[&name, &label]);
    }

    /// Identify a file, distinguishing "the header could not be read" from
    /// "the header says this is not an archive" so the status line can say
    /// which happened
    fn detect(&mut self, path: &Path) -> Option<Format> {
        // Only a regular file can carry a magic number; `is_file` also keeps
        // `detect_format` from blocking on a pipe
        if !path.is_file() {
            self.status = self.lang.t("status.extract_unknown");
            return None;
        }
        match detect_format(path) {
            Some(format) => Some(format),
            None => {
                // `detect_format` returns None both for an unknown header and
                // for a file it could not read. Telling them apart needs the
                // length, which is the only part worth checking: an unreadable
                // path would have no metadata at all
                match fs::metadata(path) {
                    Ok(meta) if meta.len() < 262 => {
                        self.status = self.lang.tf(
                            "error.read_too_short",
                            &[&path.display().to_string()],
                        );
                    }
                    Ok(_) => self.status = self.lang.t("status.extract_unknown"),
                    Err(e) => {
                        self.status = self.lang.tf(
                            "error.read_failed",
                            &[&path.display().to_string(), &e.to_string()],
                        );
                    }
                }
                None
            }
        }
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
        // An extraction reports a directory or a single payload, so it gets its
        // own copy; everything compressed keeps the wording it has always had
        let JobKind::Extract(format) = job.kind else {
            let (ok_prefix, fail_msg) = format_spec(&self.lang, &job.format);
            if status.success() {
                // The output may have been moved or deleted meanwhile; fall back
                // to "unknown" when its size cannot be read
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
                // The batch is inside that archive now, so the marks have done
                // their job. Only on success: a failed run keeps them, so the
                // same selection can simply be tried again
                self.marked.clear();
            } else {
                // On failure keep no output info, so the status bar shows the
                // failure message instead
                self.last_out = None;
                self.status = fail_msg;
            }
            // The directory gained a new output: re-read it and stay in range
            self.refresh_entries();
            self.clamp_selection();
            return;
        };
        self.finish_extract(&job, format, status.success());
    }

    /// Completion copy for a finished extraction, then the same list refresh
    /// the compression path does
    fn finish_extract(&mut self, job: &JobState, format: Format, ok: bool) {
        if !ok {
            // No output to point at, so the status bar carries the message and
            // the stale compression info must not cover it
            self.last_out = None;
            self.status = self.lang.t(format.fail_key());
        } else if format.is_single_file() {
            // One payload, so the same elidable output info as compression
            // applies and the status line reuses the shared output wording
            let size = fs::metadata(&job.out)
                .map(|meta| format_size(meta.len()))
                .unwrap_or_else(|_| self.lang.t("value.unknown"));
            let prefix = self.lang.t("extract.ok");
            self.last_out = Some(OutputInfo {
                prefix: prefix.clone(),
                path: job.out.clone(),
                size: size.clone(),
            });
            self.status = self.lang.tf(
                "status.output",
                &[&prefix, &job.out.display().to_string(), &size],
            );
        } else {
            // An archive scatters many files, so name the archive and where it
            // landed instead of inventing a single output
            let added =
                Self::read_dir(&self.current_dir).len().saturating_sub(job.entries_before);
            let count = added.to_string();
            self.last_out = None;
            self.status = self.lang.tf(
                "status.extracted",
                &[
                    &job.target.display().to_string(),
                    &job.out.display().to_string(),
                    &count,
                ],
            );
        }
        self.refresh_entries();
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
        self.status = if job.kind.is_extract() {
            self.lang.t("status.extract_cancelled")
        } else {
            self.lang.t("status.compress_cancelled")
        };
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
    use crate::extract::Format;

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

    /// A regular file whose bytes make up a gzip header, without touching its
    /// name: this is the fixture the extraction tests identify from
    fn gzip_archive(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        let mut bytes = vec![0u8; 300];
        bytes[0] = 0x1F;
        bytes[1] = 0x8B;
        fs::write(&path, bytes).expect("write fixture");
        path
    }

    /// Point the App at the scratch directory with one selected entry
    fn app_selecting(dir: &Path, target: PathBuf) -> App {
        let mut app = App::new(Lang::builtin());
        app.current_dir = dir.to_path_buf();
        app.entries = vec![target];
        app.selected = 0;
        app.scroll_offset = 0;
        app
    }

    #[test]
    fn extract_reports_nothing_selected_without_starting_a_job() {
        let s = scratch("extract-nosel");
        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.entries.clear();

        app.extract();

        assert!(app.job.is_none(), "no job may start without a selection");
        assert_eq!(app.status, app.lang.t("status.nothing_selected"));
    }

    #[test]
    fn extract_reports_an_unrecognized_format_without_starting_a_job() {
        let s = scratch("extract-unknown");
        // Right length, no recognizable magic
        let plain = s.0.join("mystery.bin");
        fs::write(&plain, vec![0u8; 400]).unwrap();
        let mut app = app_selecting(&s.0, plain);

        app.extract();

        assert!(app.job.is_none(), "an unknown header must not start a job");
        assert_eq!(app.status, app.lang.t("status.extract_unknown"));
    }

    #[test]
    fn extract_reports_a_file_too_short_to_carry_a_magic_number() {
        let s = scratch("extract-short");
        let tiny = s.0.join("tiny.bin");
        fs::write(&tiny, b"hi").unwrap();
        let mut app = app_selecting(&s.0, tiny);

        app.extract();

        assert!(app.job.is_none());
        assert_eq!(
            app.status,
            app.lang
                .tf("error.read_too_short", &[&s.0.join("tiny.bin").display().to_string()])
        );
    }

    #[test]
    fn extract_refuses_a_directory() {
        let s = scratch("extract-dir");
        let mut app = app_selecting(&s.0, s.0.clone());

        app.extract();

        assert!(app.job.is_none(), "a directory holds no magic number");
        assert_eq!(app.status, app.lang.t("status.extract_unknown"));
    }

    #[test]
    fn extract_refuses_a_single_file_archive_with_nothing_to_strip() {
        let s = scratch("extract-nosuffix");
        // Real gzip bytes, but the name has no extension to remove
        let path = gzip_archive(&s.0, "payload");
        let mut app = app_selecting(&s.0, path);

        app.extract();

        assert!(app.job.is_none(), "must not overwrite the archive itself");
        // The refusal is surfaced as an error with the localized reason in it
        let reason = app.lang.t("status.nothing_extracted");
        assert!(
            app.status.starts_with(&format!("{}: ", app.lang.t("status.error"))),
            "unexpected status: {}",
            app.status
        );
        assert!(
            app.status.contains(&reason),
            "the status should explain why: {}",
            app.status
        );
    }

    #[test]
    fn extract_starts_a_job_for_a_recognized_archive() {
        let s = scratch("extract-start");
        // A gzip header under a name that agrees with it; the nested probe
        // finds no tar inside, so this is a single-file job
        let path = gzip_archive(&s.0, "archive.gz");
        let mut app = app_selecting(&s.0, path.clone());

        app.extract();

        let job = app.job.as_ref().expect("a job should be running");
        assert_eq!(job.kind, JobKind::Extract(Format::Gz));
        // The name the user sees is the detected format, not the extension
        assert_eq!(job.format, "gzip");
        assert_eq!(job.target, path);
        // Single-file extraction produces the payload beside the archive
        assert_eq!(job.out, s.0.join("archive"));
        assert_eq!(
            app.status,
            app.lang
                .tf("status.extract_start", &[&path.display().to_string(), "gzip"])
        );
    }

    #[test]
    fn extract_targets_the_browsed_directory_for_an_archive() {
        let s = scratch("extract-dest");
        // A tar header at offset 257, with the archive itself in the scratch dir
        let mut bytes = vec![0u8; 300];
        bytes[257..262].copy_from_slice(b"ustar");
        let path = s.0.join("bundle.dat");
        fs::write(&path, bytes).unwrap();
        let mut app = app_selecting(&s.0, path);

        app.extract();

        let job = app.job.as_ref().expect("a job should be running");
        assert_eq!(job.kind, JobKind::Extract(Format::Tar));
        assert_eq!(job.format, "tar");
        // Nothing is written beside the archive: an archive fills the directory
        // being browsed
        assert_eq!(job.out, s.0);
    }

    #[test]
    fn a_second_job_is_refused_while_one_is_running() {
        let s = scratch("extract-busy");
        let path = gzip_archive(&s.0, "archive.gz");
        let mut app = app_selecting(&s.0, path);

        app.extract();
        assert!(app.job.is_some(), "the first job should be running");

        app.extract();

        assert_eq!(app.status, app.lang.t("status.job_running"));
    }

    #[test]
    fn cancelling_an_extraction_reports_the_extraction_copy() {
        let s = scratch("extract-cancel");
        let path = gzip_archive(&s.0, "archive.gz");
        let mut app = app_selecting(&s.0, path);

        app.extract();
        assert!(app.job.is_some(), "the job should be running");

        app.cancel_job();

        assert!(app.job.is_none());
        assert_eq!(app.status, app.lang.t("status.extract_cancelled"));
    }

    // ---- marks ----
    /// A listing holding `files` and `dirs`, sorted the way `read_dir` sorts:
    /// directories first, then by name
    fn listing_with(files: &[&str], dirs: &[&str]) -> Vec<PathBuf> {
        let mut entries: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
        entries.extend(files.iter().map(PathBuf::from));
        entries.sort();
        entries
    }

    /// An App browsing its own listing, with the cursor on `selected`
    fn app_over(entries: Vec<PathBuf>, selected: usize) -> App {
        let mut app = App::new(Lang::builtin());
        app.entries = entries;
        app.selected = selected;
        app.scroll_offset = 0;
        app
    }

    #[test]
    fn space_toggles_the_mark_on_the_selected_entry() {
        let entries = listing_with(&["a.txt", "b.txt"], &[]);
        let mut app = app_over(entries, 0);

        app.toggle_mark();
        assert_eq!(app.marked, vec![PathBuf::from("a.txt")]);
        let marked_status = app.status.clone();

        // Pressing it again takes the mark back off
        app.toggle_mark();
        assert!(app.marked.is_empty(), "the second press has to unmark");
        assert_ne!(app.status, marked_status, "the change has to be visible");
        assert_eq!(
            app.status,
            app.lang.tf("status.unmarked", &["0"]),
            "status={}",
            app.status
        );
    }

    #[test]
    fn a_toggle_with_no_selection_says_so_instead_of_doing_nothing() {
        let mut app = app_over(Vec::new(), 0);

        app.toggle_mark();

        assert!(app.marked.is_empty());
        assert_eq!(app.status, app.lang.t("status.nothing_selected"));
    }

    #[test]
    fn capital_a_marks_every_entry_including_the_directories() {
        // Real files and a real directory: the summary splits the set by kind,
        // which means asking the filesystem what each entry actually is
        let s = scratch("mark-all");
        fs::create_dir_all(s.0.join("dir")).unwrap();
        fs::write(s.0.join("a.txt"), b"a").unwrap();
        fs::write(s.0.join("b.txt"), b"b").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.refresh_entries();
        app.mark_all();

        assert_eq!(app.marked.len(), 3, "directories count too");
        let summary = app.marked_summary().expect("something is marked");
        assert_eq!(summary.total, 3);
        assert_eq!(summary.dirs, 1);
        assert_eq!(summary.files, 2);
        assert_eq!(app.status, summary.full(&app.lang));

        // Running it again adds nothing and says so rather than looking like it
        // worked; the order also has to survive, since it is the batch order
        let before = app.marked.clone();
        app.mark_all();
        assert_eq!(app.marked, before, "the order must not be reshuffled");
        assert_eq!(app.status, app.lang.t("status.already_marked"));
    }

    #[test]
    fn capital_a_with_nothing_to_mark_says_so() {
        let mut app = app_over(Vec::new(), 0);

        app.mark_all();

        assert!(app.marked.is_empty());
        assert_eq!(app.status, app.lang.t("status.nothing_selected"));
    }

    #[test]
    fn lower_u_drops_only_the_entry_under_the_cursor() {
        let entries = listing_with(&["a.txt", "b.txt", "c.txt"], &[]);
        let mut app = app_over(entries, 1);
        app.marked = vec![PathBuf::from("b.txt"), PathBuf::from("a.txt")];

        app.unmark_selected();

        // Not an undo stack: it took `b.txt` and left the rest exactly as they were
        assert_eq!(app.marked, vec![PathBuf::from("a.txt")]);
        assert_eq!(app.status, app.lang.tf("status.unmarked", &["1"]));

        // Pressing it on an unmarked entry reports rather than pretending
        app.unmark_selected();
        assert_eq!(app.marked, vec![PathBuf::from("a.txt")]);
        assert_eq!(app.status, app.lang.t("status.not_marked"));
    }

    #[test]
    fn lower_u_with_no_selection_says_so() {
        let mut app = app_over(Vec::new(), 0);

        app.unmark_selected();

        assert!(app.marked.is_empty());
        assert_eq!(app.status, app.lang.t("status.nothing_selected"));
    }

    #[test]
    fn capital_u_clears_every_mark_and_reports_what_went_away() {
        let entries = listing_with(&["a.txt", "b.txt"], &[]);
        let mut app = app_over(entries, 0);
        app.marked = vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")];

        app.clear_marks();

        assert!(app.marked.is_empty());
        assert_eq!(app.status, app.lang.tf("status.marks_cleared", &["2"]));
        assert!(app.marked_summary().is_none(), "nothing is marked any more");

        // Clearing an empty set is a no-op, and says so
        app.clear_marks();
        assert_eq!(app.status, app.lang.t("status.nothing_marked"));
    }

    #[test]
    fn targets_fall_back_to_the_selection_only_while_nothing_is_marked() {
        let entries = listing_with(&["a.txt", "b.txt"], &[]);
        let mut app = app_over(entries, 1);
        assert_eq!(app.targets(), vec![PathBuf::from("b.txt")]);

        // Marked entries win, and come back in the order they were marked rather
        // than in listing order, so the batch arguments are reproducible
        app.marked = vec![PathBuf::from("b.txt"), PathBuf::from("a.txt")];
        assert_eq!(app.targets(), vec![PathBuf::from("b.txt"), PathBuf::from("a.txt")]);

        // Clearing them hands the plain keys back to the selection
        app.clear_marks();
        assert_eq!(app.targets(), vec![PathBuf::from("b.txt")]);
    }

    #[test]
    fn targets_are_empty_with_nothing_marked_and_nothing_selected() {
        let app = app_over(Vec::new(), 0);

        assert!(app.targets().is_empty());
        assert!(app.marked_summary().is_none());
    }

    #[test]
    fn marks_survive_navigating_into_a_directory_and_back_out() {
        let s = scratch("marks-nav");
        let inside = s.0.join("inner");
        fs::create_dir_all(&inside).unwrap();
        fs::write(s.0.join("outside.txt"), b"x").unwrap();
        fs::write(inside.join("deep.txt"), b"y").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.refresh_entries();

        // Mark the two entries of the directory being browsed
        app.mark_all();
        let marked = app.marked.clone();
        assert_eq!(marked.len(), 2);

        // Down one level: the marks are absolute, so they do not travel with the
        // listing and cannot be clobbered by the refresh
        app.enter_dir();
        assert_eq!(app.current_dir, inside);
        assert_eq!(app.marked, marked, "marks must survive a directory change");
        assert_eq!(app.entries.len(), 1, "the new listing is what changed");

        // And back out again
        app.go_up();
        assert_eq!(app.current_dir, s.0);
        assert_eq!(app.marked, marked, "marks must survive going back");
    }

    #[test]
    fn a_marked_entry_that_vanishes_from_the_listing_still_counts() {
        // Marks are absolute paths, not row indices, so a refresh that drops the
        // entry does not silently shrink the batch behind the user's back
        let s = scratch("marks-stale");
        let file = s.0.join("here.txt");
        let gone = s.0.join("gone.txt");
        fs::write(&file, b"x").unwrap();
        fs::write(&gone, b"y").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.refresh_entries();
        app.marked = vec![file, gone.clone()];
        assert_eq!(app.marked_summary().unwrap().total, 2);

        fs::remove_file(&gone).unwrap();
        app.refresh_entries();

        assert!(!app.entries.contains(&gone));
        assert_eq!(app.marked.len(), 2, "the mark outlived its listing row");
    }

    // ---- batch compression ----
    /// Poll until the running job has been collected. `try_wait` never blocks, so
    /// this is the same shape as the main loop's tick
    fn drain(app: &mut App) {
        for _ in 0..400 {
            if app.job.is_none() {
                return;
            }
            app.poll_job();
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the job never finished");
    }

    #[test]
    fn a_marked_batch_packs_every_entry_and_clears_the_marks() {
        let s = scratch("batch-run");
        fs::write(s.0.join("a.txt"), b"one").unwrap();
        fs::write(s.0.join("b.txt"), b"two").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.refresh_entries();
        app.mark_all();
        assert_eq!(app.marked.len(), 2);

        app.compress("tar");
        let job = app.job.as_ref().expect("the batch should be running");
        // A batch has no single input to show, so the Gauge names the archive
        assert_eq!(job.label, "2_files.tar");
        drain(&mut app);

        assert!(s.0.join("2_files.tar").is_file(), "the batch produced nothing");
        assert!(app.marked.is_empty(), "a successful batch clears the marks");
        assert!(app.last_out.is_some(), "the output info should be shown");
    }

    #[test]
    fn a_failed_batch_keeps_its_marks_so_it_can_be_tried_again() {
        let s = scratch("batch-fail");
        fs::write(s.0.join("a.txt"), b"one").unwrap();
        fs::write(s.0.join("b.txt"), b"two").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.refresh_entries();
        // A member that is not there: tar exits non-zero, which is exactly the
        // case where throwing the marks away would be worst
        app.marked = vec![s.0.join("a.txt"), s.0.join("ghost")];

        app.compress("tar");
        drain(&mut app);

        assert_eq!(app.marked.len(), 2, "a failure must not clear the marks");
        assert!(app.last_out.is_none(), "a failure shows no output info");
    }

    #[test]
    fn a_wim_batch_is_refused_with_copy_that_says_why() {
        let s = scratch("wim-batch");
        fs::write(s.0.join("a.txt"), b"one").unwrap();
        fs::write(s.0.join("b.txt"), b"two").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.refresh_entries();
        app.mark_all();

        app.compress("wim");

        assert!(app.job.is_none(), "no job may start");
        assert_eq!(app.status, app.lang.t("status.wim_batch_unsupported"));
        assert_eq!(app.marked.len(), 2, "a refusal keeps the marks");

        // The refusal is about the count, not about wim: down to one target the very
        // same key goes out as before. Checked against the command builder
        // rather than by pressing the key, because that would start a real
        // capture here and leave `dism` running behind the test
        app.clear_marks();
        app.selected = 0;
        assert_eq!(app.targets().len(), 1, "the selection takes over from the marks");
        assert!(
            build_command("wim", std::slice::from_ref(&app.targets()[0])).is_ok(),
            "a single wim target is exactly what wim is for"
        );
    }

    #[test]
    fn marks_from_two_directories_are_refused() {
        let s = scratch("mixed-app");
        fs::create_dir_all(s.0.join("one")).unwrap();
        fs::create_dir_all(s.0.join("two")).unwrap();
        fs::write(s.0.join("one/a.txt"), b"a").unwrap();
        fs::write(s.0.join("two/b.txt"), b"b").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.marked = vec![s.0.join("one/a.txt"), s.0.join("two/b.txt")];

        app.compress("tar");

        assert!(app.job.is_none(), "no job may start");
        assert_eq!(app.status, app.lang.t("status.marks_mixed_dirs"));

        // One entry on its own never needs the check, so the same key still works
        app.marked.truncate(1);
        app.compress("tar");
        assert!(app.job.is_some(), "a single marked entry must still pack");
        app.cancel_job();
    }

    #[test]
    fn compress_with_nothing_marked_and_nothing_selected_reports_it() {
        let s = scratch("batch-empty");
        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.entries.clear();

        app.compress("tar");

        assert!(app.job.is_none());
        assert_eq!(app.status, app.lang.t("status.nothing_selected"));
    }

    #[test]
    fn a_single_selected_entry_still_packs_under_its_own_name() {
        // The plain keys are the ones people press without reading the hint row,
        // so with nothing marked they must behave exactly as they always did
        let s = scratch("single-target");
        let file = s.0.join("report.csv");
        fs::write(&file, b"a,b,c").unwrap();

        let mut app = App::new(Lang::builtin());
        app.current_dir = s.0.clone();
        app.entries = vec![file];
        app.selected = 0;

        app.compress("zst");

        let job = app.job.as_ref().expect("the job should be running");
        assert_eq!(job.out, s.0.join("report.csv.zst"));
        assert_eq!(job.label, "report.csv", "the Gauge names the input");
        drain(&mut app);
        assert!(s.0.join("report.csv.zst").is_file());
    }
}