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
- **Cross-folder selection**: `App.selection: Vec<SelectedItem>` is the persistent source of truth for
  what's picked with `space` — unlike `entries`, it survives navigation, so items chosen in different
  directories accumulate into one selection. `Entry.selected` is just a per-directory display mirror,
  re-derived from `selection` by `hydrate_selection()` every time `entries` is rebuilt (`load_dir`/
  `load_cache_candidates`); don't treat `Entry.selected` as authoritative. `delete_candidates()` returns
  `selection` if non-empty, else the cursor's current entry (single-item quick delete). Pressing `d`
  snapshots the result into `pending_delete`, which both the review screen (`draw_delete_review`, shown in
  place of the normal list while `Mode::ConfirmDelete` is active) and `start_delete()` read from — this
  keeps what's shown and what's deleted guaranteed identical even if `selection` changes while the
  confirm prompt is up. `start_delete()` clears `selection` immediately (not after the background thread
  finishes) since the batch is considered resolved once confirmed.
- Deletion (`d`) always goes through the `ConfirmDelete` review screen before calling
  `fs::remove_file`/`fs::remove_dir_all` on a background thread (`start_delete`/`poll_delete`).
- **Cache cleaner (`c`)**: `ViewKind::Clean(CacheCategory)` repurposes the same `entries`/selection/delete
  pipeline to list known cache/temp locations instead of a directory's children. Candidate paths (per-OS,
  `src/cache_paths.rs`) are just best-effort guesses filtered by `path.exists()`, so listing a path that's
  wrong for the current OS/toolset is harmless — it's simply absent. `Esc`/`h` from this view calls
  `leave_clean_view()` to return to `ViewKind::Explorer`; `refresh_view()` dispatches `r`/post-delete
  reloads to either `load_dir()` or `load_cache_candidates()` depending on the active view — don't call
  `load_dir()` directly from view-agnostic code paths or it'll silently switch back to Explorer content.

## Commands

- Build: `cargo build`
- Run: `cargo run`
- Test: `cargo test`
- Run a single test: `cargo test <test_name>`
- Lint: `cargo clippy`
- Format: `cargo fmt`
- Check without building: `cargo check`
