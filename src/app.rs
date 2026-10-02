use std::{
    fs,
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, Instant},
};

// 注意：不要 `use anyhow::Result`，read_dir 里在用 `Result::ok`，
// 一旦被 anyhow 的 Result 顶掉就编译不过
use anyhow::Context;

use crate::{
    compress::{build_command, format_spec},
    ui::format_size,
};

/// 文件列表可见行数
pub(crate) const VISIBLE_ROWS: usize = 20;
/// 主循环节拍：驱动进度动画并限制空转
pub(crate) const TICK: Duration = Duration::from_millis(100);
/// 动画步进，到 100 后回绕
pub(crate) const PROGRESS_STEP: u16 = 3;

/// 正在运行的压缩任务：子进程由主线程非阻塞启动，句柄一直留在这里，
/// 退出时可以直接 kill + wait 回收，不会留下孤儿进程
pub(crate) struct JobState {
    pub(crate) format: String,
    pub(crate) target: PathBuf,
    pub(crate) started: Instant,
    /// 动画式不确定进度（外部工具拿不到真实百分比）
    pub(crate) progress: u16,
    /// 子进程句柄
    pub(crate) child: Child,
    /// 产物路径，用于生成完成文案
    pub(crate) out: PathBuf,
}

/// 最近一次成功产物的信息；供 UI 在窄终端下省略路径显示
pub(crate) struct OutputInfo {
    pub(crate) prefix: &'static str,
    pub(crate) path: PathBuf,
    pub(crate) size: String,
}

/// 弹窗要执行的动作。目前只有不可逆的删除，
/// 以后要加别的弹窗时在这里补一个变体，App 侧的流程不用改
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DialogAction {
    /// 永久删除文件或目录，无法撤销
    Delete,
}

/// 模态弹窗状态。为 `None` 时按键走主界面逻辑；
/// 为 `Some` 时焦点在弹窗上，只有 Enter / Esc 会被放行，其余按键全部吞掉
pub(crate) struct Dialog {
    pub(crate) action: DialogAction,
    /// 打开弹窗那一刻锁定的目标。界面显示和实际删除都用它，
    /// 保证"看到的"和"删掉的"永远是同一条路径
    pub(crate) target: PathBuf,
    /// 打开弹窗时目标的类型，决定文案与删除方式
    pub(crate) is_dir: bool,
}

pub(crate) struct App {
    pub(crate) current_dir: PathBuf,
    pub(crate) entries: Vec<PathBuf>,
    pub(crate) selected: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) status: String,
    pub(crate) job: Option<JobState>,
    /// 有值时状态栏显示"压缩产物 + 大小"，并按终端宽度自适应省略路径
    pub(crate) last_out: Option<OutputInfo>,
    /// 模态弹窗；打开时按键被限制在 Enter / Esc
    pub(crate) dialog: Option<Dialog>,
}

impl App {
    pub(crate) fn new() -> Self {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let entries = Self::read_dir(&current_dir);
        Self {
            current_dir,
            entries,
            selected: 0,
            scroll_offset: 0,
            status: "就绪".to_string(),
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
            // 换了目录，之前的压缩产物不再相关
            self.last_out = None;
            self.status = format!("进入 {}", entry.display());
        }
    }

    pub(crate) fn go_up(&mut self) {
        let parent = self.current_dir.parent().map(|p| p.to_path_buf());
        if let Some(parent) = parent {
            self.current_dir = parent.clone();
            self.entries = Self::read_dir(&self.current_dir);
            self.selected = 0;
            self.scroll_offset = 0;
            // 换了目录，之前的压缩产物不再相关
            self.last_out = None;
            self.status = format!("进入 {}", parent.display());
        }
    }

    pub(crate) fn get_selected_path(&self) -> Option<PathBuf> {
        self.entries.get(self.selected).cloned()
    }

    /// 按 `d`：打开删除确认弹窗。条件不满足时只提示状态，绝不开弹窗
    pub(crate) fn request_delete(&mut self) {
        // 正在压缩时目标文件正被子进程读取，此时删掉它等于把压缩源从源头上抽走
        if self.job.is_some() {
            self.status = "已有任务进行中".to_string();
            return;
        }

        let Some(path) = self.get_selected_path() else {
            self.status = "没有选中文件".to_string();
            return;
        };

        // 类型在打开时就定死：文案和删除方式都按它来，避免中途判断漂移
        self.dialog = Some(Dialog {
            action: DialogAction::Delete,
            is_dir: path.is_dir(),
            target: path,
        });
        self.status = "确认删除? Enter 删除 / Esc 取消".to_string();
    }

    /// 弹窗里按 `Enter`：执行动作，无论成败都先关掉弹窗
    pub(crate) fn confirm_dialog(&mut self) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };

        match dialog.action {
            DialogAction::Delete => {
                let target = dialog.target;
                match remove_path(&target) {
                    Ok(()) => {
                        // 列表少了一项，重新读取并把选中项夹回合法范围
                        self.entries = Self::read_dir(&self.current_dir);
                        self.clamp_selection();
                        // 上一次压缩产物的信息已不相关，留着会盖掉删除结果
                        self.last_out = None;
                        let kind = if dialog.is_dir { "目录" } else { "文件" };
                        self.status = format!("已删除{}: {}", kind, target.display());
                    }
                    Err(e) => {
                        // Windows 上只读 / 被占用的文件会失败，错误交给状态栏，绝不 panic
                        self.last_out = None;
                        self.status = format!("删除失败: {}", e);
                    }
                }
            }
        }
    }

    /// 弹窗里按 `Esc`：只关弹窗，什么都不删
    pub(crate) fn dismiss_dialog(&mut self) {
        if self.dialog.take().is_none() {
            return;
        }
        // 取消也是一次明确操作，给个回执，免得看起来像按键没生效
        self.last_out = None;
        self.status = "已取消删除".to_string();
    }

    pub(crate) fn compress(&mut self, format: &str) {
        // 同一时间只允许一个后台任务
        if self.job.is_some() {
            self.status = "已有任务进行中".to_string();
            return;
        }

        let Some(path) = self.get_selected_path() else {
            self.status = "没有选中文件".to_string();
            return;
        };

        let (mut cmd, out) = match build_command(format, &path) {
            Ok(built) => built,
            Err(e) => {
                self.status = format!("错误: {}", e);
                return;
            }
        };

        // 非阻塞启动，句柄留在主线程，退出时能回收
        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                self.status = format!("错误: {}", e);
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
        // 上一次的产物信息作废，新任务期间状态栏显示进度条
        self.last_out = None;
        self.status = format!("开始压缩: {}", format);
    }

    /// 每次节拍推进进度动画（不确定进度，循环播放）
    pub(crate) fn tick_job(&mut self) {
        if let Some(job) = self.job.as_mut() {
            job.progress = (job.progress + PROGRESS_STEP) % 101;
        }
    }

    /// 轮询子进程；try_wait 不阻塞，结束后刷新列表并夹紧选中项
    pub(crate) fn poll_job(&mut self) {
        // 每帧最多调用一次 try_wait
        let waited = match self.job.as_mut() {
            Some(job) => job.child.try_wait(),
            None => return,
        };

        let status = match waited {
            Ok(Some(status)) => status,
            Ok(None) => return,
            Err(e) => {
                // 回收子进程，避免留下孤儿
                self.cancel_job();
                // 任务出错，旧产物信息作废，否则会盖掉错误文案
                self.last_out = None;
                self.status = format!("错误: {}", e);
                return;
            }
        };

        let Some(job) = self.job.take() else {
            return;
        };
        let (_, ok_prefix, fail_msg) = format_spec(&job.format);
        if status.success() {
            // 产物可能已被外部移动或删除，取不到大小时降级为“未知”
            let size = fs::metadata(&job.out)
                .map(|meta| format_size(meta.len()))
                .unwrap_or_else(|_| "未知".to_string());
            self.last_out = Some(OutputInfo {
                prefix: ok_prefix,
                path: job.out.clone(),
                size: size.clone(),
            });
            self.status = format!("{}: {}  大小: {}", ok_prefix, job.out.display(), size);
        } else {
            // 失败时不保留旧产物信息，状态栏改显示失败文案
            self.last_out = None;
            self.status = fail_msg.to_string();
        }
        // 目录里新增了压缩产物，重新读取并防止越界
        self.entries = Self::read_dir(&self.current_dir);
        self.clamp_selection();
    }

    /// 终止正在运行的任务：kill 之后必须 wait 回收，否则留下僵尸
    pub(crate) fn cancel_job(&mut self) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        let _ = job.child.kill();
        let _ = job.child.wait();
        // 取消之后不该继续显示上一个产物，状态栏改显示取消文案
        self.last_out = None;
        self.status = "已取消正在进行的压缩".to_string();
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

/// 按类型删除：目录递归删除，其余按文件删除。
/// 删除不可逆，失败原样返回交给调用方报错，不 panic
pub(crate) fn remove_path(path: &Path) -> anyhow::Result<()> {
    // remove_dir_all 不跟随符号链接，指向目录的软链接只会删掉链接本身
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    // 带上路径，Windows 上"拒绝访问 / 文件被占用"这类错误才知道说的是谁
    result.with_context(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// 临时目录守卫：无论用例通过还是 panic，析构时都会把目录清掉，
    /// 不在临时目录里留垃圾
    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 每个用例一个独立临时目录：进程 id + 自增序号命名，
    /// 并行跑不会互相干扰，重名时先清掉上一次残留
    fn scratch(tag: &str) -> Scratch {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let name = format!("lazyzst-test-{}-{tag}-{n}", std::process::id());
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("创建临时目录");
        Scratch(dir)
    }

    /// 只让 App 的选中项指向临时目录，避免测试去动真实的列表
    fn app_with_target(dir: &Path, target: PathBuf) -> App {
        let mut app = App::new();
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

        remove_path(&file).expect("删除文件应成功");

        assert!(!file.exists(), "文件应已被删除");
        assert!(dir.exists(), "只删文件，父目录应保留");
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

        remove_path(dir).expect("删除目录应成功");

        assert!(!dir.exists(), "目录应被递归删除");
        assert!(!leaf.exists(), "子文件应一并删除");
    }

    #[test]
    fn missing_path_returns_err_instead_of_panicking() {
        let s = scratch("missing");
        let dir = &s.0;
        let ghost = dir.join("never-created.txt");

        let err = remove_path(&ghost).expect_err("不存在的路径应返回 Err");

        // 错误信息里要带得上路径，状态栏才有得说
        assert!(
            err.to_string().contains(&ghost.display().to_string()),
            "错误信息应带上路径: {err}"
        );
        assert!(dir.exists(), "失败不应误删父目录");
    }

    #[test]
    fn escape_dismisses_dialog_without_deleting() {
        let s = scratch("esc");
        let file = s.0.join("keep.txt");
        fs::write(&file, b"x").unwrap();
        let mut app = app_with_target(&s.0, file.clone());

        app.request_delete();
        assert!(app.dialog.is_some(), "应弹出确认框");

        app.dismiss_dialog();

        assert!(app.dialog.is_none(), "Esc 应关闭弹窗");
        assert!(file.exists(), "Esc 绝不能删文件");
        assert_eq!(app.status, "已取消删除");
    }

    #[test]
    fn confirm_deletes_the_target_and_refreshes_the_list() {
        let s = scratch("confirm");
        let file = s.0.join("gone.txt");
        fs::write(&file, b"x").unwrap();
        let mut app = app_with_target(&s.0, file.clone());
        app.last_out = Some(OutputInfo {
            prefix: "已压缩",
            path: s.0.join("old.zst"),
            size: "1 KB".to_string(),
        });

        app.request_delete();
        let dialog = app.dialog.as_ref().expect("应弹出确认框");
        assert_eq!(dialog.action, DialogAction::Delete);
        assert!(!dialog.is_dir, "临时文件应识别为文件");
        assert_eq!(dialog.target, file);

        app.confirm_dialog();

        assert!(app.dialog.is_none(), "确认后弹窗应关闭");
        assert!(!file.exists(), "确认后文件应被删除");
        assert!(app.last_out.is_none(), "旧产物信息必须清空，否则盖掉结果");
        assert!(app.status.contains("已删除文件"), "status={}", app.status);
    }

    #[test]
    fn dialog_marks_directory_targets_as_directories() {
        let s = scratch("isdir");
        let child = s.0.join("child");
        fs::create_dir_all(&child).unwrap();
        let mut app = app_with_target(&s.0, child.clone());

        app.request_delete();
        let dialog = app.dialog.as_ref().expect("应弹出确认框");
        assert!(dialog.is_dir, "目录应识别为目录");
        assert_eq!(dialog.target, child);

        app.dismiss_dialog();
        assert!(child.is_dir(), "取消不应影响任何目录");
    }

    #[test]
    fn nothing_selected_reports_status_without_opening_dialog() {
        let s = scratch("nosel");
        let mut app = App::new();
        app.current_dir = s.0.clone();
        app.entries.clear();
        app.selected = 0;

        app.request_delete();

        assert!(app.dialog.is_none(), "没有选中项时不该弹窗");
        assert_eq!(app.status, "没有选中文件");
    }
}