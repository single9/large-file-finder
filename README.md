# large-file-finder (`lff`)

A fast, cross-platform disk usage tool with an interactive terminal UI. Browse your
filesystem like a file explorer, sorted by size, and clean up large files, folders,
and AI/system caches — with a review-and-confirm step before anything is deleted.

## Install

Requires the Rust toolchain (`cargo`).

```sh
cargo install --path .
```

This installs the `lff` binary to `~/.cargo/bin`.

## Usage

```sh
lff [PATH]              # launch the interactive explorer (default)
lff --list [PATH]       # print a flat, sorted list instead
```

Options:

| Flag | Description |
| --- | --- |
| `PATH` | Directory to start in (default: `.`) |
| `-s, --min-size <SIZE>` | Only show entries at least this big (accepts `10M`, `1G`, `500K`, `2T`) |
| `-n, --limit <N>` | Number of results to show (`--list` mode only, default 20) |
| `--follow-links` | Follow symlinks while scanning |
| `--list` | Print a flat sorted list instead of launching the TUI |

## Interactive explorer

Directory sizes are computed on background threads so the UI never blocks, even on
huge directory trees — a spinner in the header shows when scanning is still in
progress. Entries you can't read (permission denied) are shown, not hidden, marked
"no access"; directories with some unreadable descendants show a lower-bound size
with a `+` suffix.

### Keyboard shortcuts

| Keys | Action |
| --- | --- |
| `↑/↓`, `j/k` | Move |
| `→`, `enter`, `l` | Open directory |
| `←`, `backspace`, `h` | Go up / back |
| `g` | Go to path |
| `/` | Filter by name |
| `m` | Filter by minimum size |
| `space` | Select — persists as you navigate, so you can pick items across multiple folders |
| `d`, `delete` | Review everything selected (across all folders) in one screen, then delete on confirmation |
| `s` | Cycle sort (size ↓, size ↑, name) |
| `r` | Refresh |
| `c` | Clean caches & temp files |
| `?` | Toggle the full shortcut help |
| `q`, `esc` | Quit / back |

### Cleaning caches and temp files

Press `c` to open the cache cleaner, then pick a category:

- **AI Caches** — model/tool caches such as Hugging Face hub, PyTorch hub, Ollama,
  Whisper, LM Studio, and known AI-assisted editor/desktop app caches.
- **System Caches** — OS-level caches and temp directories (e.g. `~/Library/Caches`
  and `~/Library/Logs` on macOS, `~/.cache` and `/var/tmp` on Linux, `%TEMP%` and
  the Windows internet cache on Windows).

Only locations that actually exist on disk are listed. Selection, review, and
delete-with-confirmation work exactly like browsing normal files — nothing is
removed without an explicit `y` confirmation. Deletion runs in the background with
a live progress indicator, so removing a large folder doesn't freeze the UI.

## Platform support

Tested on macOS; the cache catalog and path handling also cover Linux and Windows
conventions. Unsupported or missing paths are simply skipped rather than causing
errors.

## Development

```sh
cargo build   # build
cargo run     # run the TUI
cargo test    # run tests
cargo clippy  # lint
cargo fmt     # format
```

See `CLAUDE.md` for architecture notes.
