use std::{
    fs,
    path::{Path, PathBuf},
    process::Child,
    time::{Duration, Instant},
};

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

pub(crate) struct App {
    pub(crate) current_dir: PathBuf,
    pub(crate) entries: Vec<PathBuf>,
    pub(crate) selected: usize,
    pub(crate) scroll_offset: usize,
    pub(crate) status: String,
    pub(crate) job: Option<JobState>,
    /// 有值时状态栏显示"压缩产物 + 大小"，并按终端宽度自适应省略路径
    pub(crate) last_out: Option<OutputInfo>,
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