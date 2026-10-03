# lazyzst

> 一个类 lazygit 风格的 TUI 归档管理器。

## 简介

`lazyzst` 是一个终端用户界面（TUI）工具，旨在提供类似 [lazygit](https://github.com/jesseduffield/lazygit) 的流畅交互体验，用于管理压缩与解压缩任务。通过直观的键盘操作和类 lazygit 的暂存机制，让归档操作变得轻松高效。

## ✨ 功能特性

### 1. 一键解压缩

支持主流压缩格式的一键解压：

| 格式 | 说明 | 依赖安装 |
|------|------|----------|
| `.tar` | 无需额外安装 | - |
| `.zip` | 无需额外安装 | - |
| `.wim` | 无需额外安装 | - |
| `.7z` | 需要安装 7-Zip | `scoop install 7zip` |
| `.zst` | 需要安装 zstd | `scoop install zstd` |
| `.gz` | 需要安装 gzip | `scoop install gzip` |
| `.xz` | 需要安装 xz | `scoop install xz` |

### 2. Vim/Neovim 键位适配

- `j` / `k` — 上下移动光标
- `q` — 退出程序

### 3. 仿 lazygit 暂存机制

- `<Space>` — 标记/取消标记当前项
- `A` — 全部标记
- `u` — 撤销上一个标记
- `U` — 撤销全部标记
- 标记后可批量压缩为一个文件

## 🚧 开发计划 (Todo)

- [ ] **命令行传参版** — 实现自动化操作，支持脚本调用
- [ ] **支持更多格式** — 扩展压缩格式兼容性
- [ ] **自定义主题** — 通过 `colorscheme.json` 编辑配色方案
- [ ] **键位自定义** — 通过 `keymap.json` 修改键位绑定
- [ ] **默认 UI 美化** — 优化默认界面视觉效果
- [ ] **解压模式切换** — 按 `p` 弹出面板切换解压模式
- [ ] **Nerd Font 支持** — 支持 Nerd Font 图标显示

## 安装

```bash
# 通过 Scoop 安装依赖（Windows）
scoop install 7zip zstd gzip xz
```

## 使用方法

```bash
# 启动 TUI 界面
lazyzst

# （计划中）命令行传参
lazyzst --extract archive.tar.gz
lazyzst --compress file1 file2 -o output.zip
```

## 快捷键一览

| 按键 | 功能 |
|------|------|
| `j` | 向下移动 |
| `k` | 向上移动 |
| `<Space>` | 标记/取消标记 |
| `A` | 全部标记 |
| `u` | 撤销上一个标记 |
| `U` | 撤销全部标记 |
| `p` | 弹出解压模式面板（计划中） |
| `q` | 退出 |

## 许可证

MIT License
