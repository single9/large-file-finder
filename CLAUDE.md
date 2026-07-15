# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project state

A Rust CLI (edition 2024) that walks a directory tree and reports the largest files/folders. Two modes:

- **TUI mode** (default, `src/tui.rs`): an interactive file-explorer built on `ratatui`/`crossterm`
  (`ratatui` is pulled in with `default-features = false, features = ["crossterm"]` — the default
  feature set drags in unrelated backends like termwiz/wezterm, so don't re-enable defaults).
- **Flat mode** (`--list` flag, `src/main.rs`): walks the tree with `walkdir::WalkDir` and prints the
  largest files sorted descending by size. Per-entry errors (e.g. permission denied) are logged to
  stderr and skipped rather than aborting the walk.

Shared CLI args (`Args` in `main.rs`, `clap` derive): `path` (default `.`), `-s/--min-size` (accepts
suffixes `k`/`m`/`g`/`t`, parsed by `parse_size`, `pub(crate)` so `tui.rs` reuses it for the in-app
min-size filter), `-n/--limit` (`--list` mode only), `--follow-links`, `--list`.

### TUI architecture (`src/tui.rs`)

- `App` holds `entries` (immediate children of `current_dir`) plus a `filtered` index list recomputed by
  `apply_filter()` whenever the name filter, min-size filter, or directory changes.
- **Directory sizes are computed off the UI thread.** `SizeScanner` runs a fixed pool of 4 worker threads
  reading jobs from an `mpsc` channel and pushing `(PathBuf, DirSize)` results to another channel; `App`
  polls it non-blockingly once per event loop tick (`poll_sizes`). This avoids spawning unbounded threads
  or blocking the render loop when a directory has many/huge subdirectories. Results are cached in
  `size_cache` so revisiting a directory doesn't rescan (invalidated per-subtree on `r` refresh).
- **Permission handling**: an entry that can't be `read_dir`'d at all is `Size::Denied` ("no access", shown
  in red) and cannot be entered/deleted-into; a directory that's readable but has unreadable descendants
  partway through the walk is `Size::Partial` (size is a lower bound, shown with a `+` suffix) rather than
  being dropped from the listing — nothing is silently excluded from view.
- Deletion (`d`) targets the selection set if non-empty, otherwise falls back to the entry under the
  cursor (`entries_to_delete`), and always goes through a `ConfirmDelete` mode before calling
  `fs::remove_file`/`fs::remove_dir_all`.

## Commands

- Build: `cargo build`
- Run: `cargo run`
- Test: `cargo test`
- Run a single test: `cargo test <test_name>`
- Lint: `cargo clippy`
- Format: `cargo fmt`
- Check without building: `cargo check`
