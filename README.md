# lazyzst

> A lazygit-style TUI archiver.

## Introduction

`lazyzst` is a terminal user interface (TUI) tool designed to provide a smooth, [lazygit](https://github.com/jesseduffield/lazygit)-like interactive experience for managing compression and decompression tasks. With intuitive keyboard navigation and a lazygit-inspired staging mechanism, archive operations become effortless and efficient.

## ✨ Features

### 1. One-Click Extraction

Supports one-click extraction for major archive formats:

| Format | Notes | Dependency Installation |
|--------|-------|------------------------|
| `.tar` | No extra installation required | - |
| `.zip` | No extra installation required | - |
| `.wim` | No extra installation required | - |
| `.7z` | Requires 7-Zip | `scoop install 7zip` |
| `.zst` | Requires zstd | `scoop install zstd` |
| `.gz` | Requires gzip | `scoop install gzip` |
| `.xz` | Requires xz | `scoop install xz` |

### 2. Vim/Neovim Keymap Support

- `j` / `k` — Move cursor up/down
- `q` — Quit the application

### 3. Lazygit-Inspired Staging Mechanism

- `<Space>` — Toggle mark on the current item
- `A` — Mark all items
- `u` — Unmark the last marked item
- `U` — Unmark all items
- Marked items can be batch-compressed into a single archive

## 🚧 Roadmap (Todo)

- [ ] **CLI argument support** — Enable automation and scripting via command-line arguments
- [ ] **More archive formats** — Expand format compatibility
- [ ] **Custom themes** — Edit color schemes via `colorscheme.json`
- [ ] **Keymap customization** — Modify key bindings via `keymap.json`
- [ ] **Default UI polish** — Improve the default visual presentation
- [ ] **Extraction mode switcher** — Press `p` to open a panel for switching extraction modes
- [ ] **Nerd Font support** — Support Nerd Font icon display

## Installation

```bash
# Install dependencies via Scoop (Windows)
scoop install 7zip zstd gzip xz
```

## Usage

```bash
# Launch the TUI
lazyzst

# (Planned) CLI arguments
lazyzst --extract archive.tar.gz
lazyzst --compress file1 file2 -o output.zip
```

## Keymap Reference

| Key | Action |
|-----|--------|
| `j` | Move down |
| `k` | Move up |
| `<Space>` | Toggle mark |
| `A` | Mark all |
| `u` | Unmark last |
| `U` | Unmark all |
| `p` | Open extraction mode panel (planned) |
| `q` | Quit |

## License

MIT License
