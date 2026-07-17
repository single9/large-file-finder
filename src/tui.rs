use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use humansize::{DECIMAL, format_size};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use ratatui::{DefaultTerminal, Frame};

/// Result of scanning a directory's total size, computed off the UI thread.
#[derive(Clone, Copy)]
enum DirSize {
    /// Fully scanned, no errors.
    Bytes(u64),
    /// Scanned, but some descendants could not be read (size is a lower bound).
    Partial(u64),
    /// The directory itself could not be listed at all.
    Denied,
}

enum Size {
    Pending,
    Known(u64),
    Partial(u64),
    Denied,
}

/// Progress updates from a background deletion, so the UI can show what's
/// happening instead of freezing on a large `remove_dir_all`.
enum DeleteMsg {
    Progress {
        done: usize,
        total: usize,
        name: String,
    },
    Finished {
        deleted: usize,
        failed: usize,
    },
}

struct DeleteProgress {
    done: usize,
    total: usize,
    name: String,
}

struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: Size,
    selected: bool,
}

/// A selection persists across navigation (unlike `Entry`, which is rebuilt
/// from scratch every time a directory or cache view loads), so items picked
/// in different folders can be reviewed and deleted together in one batch.
#[derive(Clone)]
struct SelectedItem {
    path: PathBuf,
    name: String,
    is_dir: bool,
    size: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum SortMode {
    SizeDesc,
    SizeAsc,
    NameAsc,
}

enum Mode {
    Normal,
    Filter,
    MinSize,
    GotoPath,
    ConfirmDelete,
    CleanMenu,
    Help,
}

#[derive(Clone, Copy, PartialEq)]
enum CacheCategory {
    Ai,
    System,
}

impl CacheCategory {
    const ALL: [CacheCategory; 2] = [CacheCategory::Ai, CacheCategory::System];

    fn label(self) -> &'static str {
        match self {
            CacheCategory::Ai => "AI Caches",
            CacheCategory::System => "System Caches",
        }
    }

    fn candidates(self) -> Vec<crate::cache_paths::CacheEntry> {
        match self {
            CacheCategory::Ai => crate::cache_paths::ai_cache_candidates(),
            CacheCategory::System => crate::cache_paths::system_cache_candidates(),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ViewKind {
    Explorer,
    Clean(CacheCategory),
    /// Windows only: list of mounted drive letters, shown when backing out
    /// of a drive root (which has no filesystem parent) or via the `D` key.
    Drives,
}

/// Mounted drive roots (e.g. `C:\`, `D:\`) on Windows; empty everywhere else,
/// since other platforms have a single filesystem root with no drive concept.
#[cfg(windows)]
fn windows_drives() -> Vec<PathBuf> {
    (b'A'..=b'Z')
        .filter_map(|letter| {
            let path = PathBuf::from(format!("{}:\\", letter as char));
            path.is_dir().then_some(path)
        })
        .collect()
}

#[cfg(not(windows))]
fn windows_drives() -> Vec<PathBuf> {
    Vec::new()
}

/// Sends directory paths to a fixed pool of worker threads and receives their
/// computed sizes, so opening a directory with many large subfolders doesn't
/// spawn unbounded threads or block the render loop.
struct SizeScanner {
    job_tx: Sender<PathBuf>,
    result_rx: Receiver<(PathBuf, DirSize)>,
}

impl SizeScanner {
    fn new(follow_links: bool) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<PathBuf>();
        let (result_tx, result_rx) = mpsc::channel();
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));

        for _ in 0..4 {
            let job_rx = job_rx.clone();
            let result_tx = result_tx.clone();
            thread::spawn(move || {
                loop {
                    let job = { job_rx.lock().unwrap().recv() };
                    let Ok(path) = job else { break };
                    let size = scan_dir_size(&path, follow_links);
                    if result_tx.send((path, size)).is_err() {
                        break;
                    }
                }
            });
        }

        Self { job_tx, result_rx }
    }

    fn request(&self, path: PathBuf) {
        let _ = self.job_tx.send(path);
    }

    fn poll(&self) -> Vec<(PathBuf, DirSize)> {
        self.result_rx.try_iter().collect()
    }
}

fn scan_dir_size(root: &Path, follow_links: bool) -> DirSize {
    if fs::read_dir(root).is_err() {
        return DirSize::Denied;
    }

    let mut total = 0u64;
    let mut had_error = false;

    for entry in walkdir::WalkDir::new(root).follow_links(follow_links) {
        match entry {
            Ok(entry) => {
                if entry.file_type().is_file() {
                    if let Ok(meta) = entry.metadata() {
                        total += meta.len();
                    } else {
                        had_error = true;
                    }
                }
            }
            Err(_) => had_error = true,
        }
    }

    if had_error {
        DirSize::Partial(total)
    } else {
        DirSize::Bytes(total)
    }
}

pub struct TuiOptions {
    pub root: PathBuf,
    pub min_size: u64,
    pub follow_links: bool,
}

pub fn run(opts: TuiOptions) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = App::new(opts).run(&mut terminal);
    ratatui::restore();
    result
}

struct App {
    current_dir: PathBuf,
    entries: Vec<Entry>,
    filtered: Vec<usize>,
    cursor: usize,
    filter: String,
    min_size: u64,
    sort_mode: SortMode,
    mode: Mode,
    input: String,
    status: String,
    scanner: SizeScanner,
    size_cache: HashMap<PathBuf, DirSize>,
    quit: bool,
    spinner_tick: usize,
    view: ViewKind,
    clean_menu_cursor: usize,
    delete_rx: Option<Receiver<DeleteMsg>>,
    delete_progress: Option<DeleteProgress>,
    /// Cross-folder selection, built up by pressing space in any directory
    /// or cache view; survives navigation until deleted or toggled off.
    selection: Vec<SelectedItem>,
    /// Snapshot of what's about to be deleted, captured when entering
    /// `Mode::ConfirmDelete` so the review screen and the actual delete
    /// operate on the same fixed list even if the user keeps browsing after
    /// cancelling.
    pending_delete: Vec<SelectedItem>,
}

impl App {
    fn new(opts: TuiOptions) -> Self {
        let scanner = SizeScanner::new(opts.follow_links);
        let root = opts.root.canonicalize().unwrap_or(opts.root);
        let mut app = Self {
            current_dir: root,
            entries: Vec::new(),
            filtered: Vec::new(),
            cursor: 0,
            filter: String::new(),
            min_size: opts.min_size,
            sort_mode: SortMode::SizeDesc,
            mode: Mode::Normal,
            input: String::new(),
            status: String::new(),
            scanner,
            size_cache: HashMap::new(),
            quit: false,
            spinner_tick: 0,
            view: ViewKind::Explorer,
            clean_menu_cursor: 0,
            delete_rx: None,
            delete_progress: None,
            selection: Vec::new(),
            pending_delete: Vec::new(),
        };
        app.load_dir();
        app
    }

    fn load_dir(&mut self) {
        self.entries.clear();
        self.cursor = 0;

        let read = match fs::read_dir(&self.current_dir) {
            Ok(read) => read,
            Err(err) => {
                self.status = format!("cannot read {}: {err}", self.current_dir.display());
                self.apply_filter();
                return;
            }
        };

        for item in read.flatten() {
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            let meta = item.metadata();

            match meta {
                Ok(meta) if meta.is_dir() => {
                    let size = if let Some(cached) = self.size_cache.get(&path) {
                        dir_size_to_size(*cached)
                    } else {
                        self.scanner.request(path.clone());
                        Size::Pending
                    };
                    self.entries.push(Entry {
                        name,
                        path,
                        is_dir: true,
                        size,
                        selected: false,
                    });
                }
                Ok(meta) => {
                    self.entries.push(Entry {
                        name,
                        path,
                        is_dir: false,
                        size: Size::Known(meta.len()),
                        selected: false,
                    });
                }
                Err(_) => {
                    self.entries.push(Entry {
                        name,
                        path,
                        is_dir: item.file_type().map(|t| t.is_dir()).unwrap_or(false),
                        size: Size::Denied,
                        selected: false,
                    });
                }
            }
        }

        self.hydrate_selection();
        self.sort_entries();
        self.apply_filter();
    }

    /// Marks freshly-loaded entries as selected if they're already part of
    /// the persistent cross-folder selection, so checkboxes stay accurate
    /// when re-visiting a directory.
    fn hydrate_selection(&mut self) {
        for entry in &mut self.entries {
            entry.selected = self.selection.iter().any(|s| s.path == entry.path);
        }
    }

    fn load_cache_candidates(&mut self, category: CacheCategory) {
        self.entries.clear();
        self.cursor = 0;

        let mut seen = std::collections::HashSet::new();
        for candidate in category.candidates() {
            if !candidate.path.exists() {
                continue;
            }
            let canon = candidate
                .path
                .canonicalize()
                .unwrap_or_else(|_| candidate.path.clone());
            if !seen.insert(canon.clone()) {
                continue;
            }

            let is_dir = canon.is_dir();
            let size = if is_dir {
                if let Some(cached) = self.size_cache.get(&canon) {
                    dir_size_to_size(*cached)
                } else {
                    self.scanner.request(canon.clone());
                    Size::Pending
                }
            } else {
                match fs::metadata(&canon) {
                    Ok(meta) => Size::Known(meta.len()),
                    Err(_) => Size::Denied,
                }
            };

            self.entries.push(Entry {
                name: format!("{}  ({})", candidate.label, canon.display()),
                path: canon,
                is_dir,
                size,
                selected: false,
            });
        }

        self.hydrate_selection();
        self.sort_entries();
        self.apply_filter();
    }

    fn open_clean_view(&mut self, category: CacheCategory) {
        self.view = ViewKind::Clean(category);
        self.mode = Mode::Normal;
        self.filter.clear();
        self.load_cache_candidates(category);
    }

    fn leave_clean_view(&mut self) {
        self.view = ViewKind::Explorer;
        self.filter.clear();
        self.load_dir();
    }

    fn leave_drives_view(&mut self) {
        self.view = ViewKind::Explorer;
        self.filter.clear();
        self.load_dir();
    }

    /// Populates `entries` with mounted drive letters instead of a
    /// directory's children, reusing the same list/select/enter pipeline as
    /// `load_dir` so picking a drive is just "open directory" on a drive
    /// root entry.
    fn load_drives(&mut self) {
        self.entries.clear();
        self.cursor = 0;

        for path in windows_drives() {
            let name = path.to_string_lossy().into_owned();
            let size = if let Some(cached) = self.size_cache.get(&path) {
                dir_size_to_size(*cached)
            } else {
                self.scanner.request(path.clone());
                Size::Pending
            };
            self.entries.push(Entry {
                name,
                path,
                is_dir: true,
                size,
                selected: false,
            });
        }

        self.hydrate_selection();
        self.sort_entries();
        self.apply_filter();
    }

    fn refresh_view(&mut self) {
        match self.view {
            ViewKind::Explorer => self.load_dir(),
            ViewKind::Clean(category) => self.load_cache_candidates(category),
            ViewKind::Drives => self.load_drives(),
        }
    }

    fn sort_entries(&mut self) {
        match self.sort_mode {
            SortMode::SizeDesc => self.entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then(size_value(&b.size).cmp(&size_value(&a.size)))
            }),
            SortMode::SizeAsc => self.entries.sort_by(|a, b| {
                b.is_dir
                    .cmp(&a.is_dir)
                    .then(size_value(&a.size).cmp(&size_value(&b.size)))
            }),
            SortMode::NameAsc => self.entries.sort_by_key(|a| a.name.to_lowercase()),
        }
    }

    fn apply_filter(&mut self) {
        let filter = self.filter.to_lowercase();
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| filter.is_empty() || e.name.to_lowercase().contains(&filter))
            .filter(|(_, e)| size_value(&e.size) >= self.min_size)
            .map(|(i, _)| i)
            .collect();
        if self.cursor >= self.filtered.len() {
            self.cursor = self.filtered.len().saturating_sub(1);
        }
    }

    fn poll_sizes(&mut self) {
        let results = self.scanner.poll();
        if results.is_empty() {
            return;
        }
        for (path, dir_size) in results {
            self.size_cache.insert(path.clone(), dir_size);
            if let Some(entry) = self.entries.iter_mut().find(|e| e.path == path) {
                entry.size = dir_size_to_size(dir_size);
            }
            if let Some(selected) = self.selection.iter_mut().find(|s| s.path == path) {
                selected.size = size_value(&dir_size_to_size(dir_size));
            }
        }
        let target = self.cursor_target();
        self.sort_entries();
        self.apply_filter();
        self.restore_cursor(target);
    }

    fn pending_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| matches!(e.size, Size::Pending))
            .count()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.filtered.get(self.cursor).map(|&i| &self.entries[i])
    }

    /// Path of the entry currently under the cursor, captured before a
    /// re-sort/re-filter so it can be re-located afterward — without this,
    /// `self.cursor` (a bare index) can end up silently pointing at a
    /// different entry once the list reorders (e.g. a background directory
    /// size arriving), which is dangerous right before a delete.
    fn cursor_target(&self) -> Option<PathBuf> {
        self.filtered
            .get(self.cursor)
            .map(|&i| self.entries[i].path.clone())
    }

    /// Re-points the cursor at `target` if it's still present in the current
    /// `filtered` list; otherwise leaves the clamped value `apply_filter`
    /// already set.
    fn restore_cursor(&mut self, target: Option<PathBuf>) {
        let Some(target) = target else {
            return;
        };
        if let Some(pos) = self
            .filtered
            .iter()
            .position(|&i| self.entries[i].path == target)
        {
            self.cursor = pos;
        }
    }

    /// The items a delete would act on: the cross-folder selection if
    /// anything's been picked with space, otherwise just the entry under the
    /// cursor (single-item quick delete).
    fn delete_candidates(&self) -> Vec<SelectedItem> {
        if !self.selection.is_empty() {
            return self.selection.clone();
        }
        self.filtered
            .get(self.cursor)
            .map(|&i| {
                let e = &self.entries[i];
                SelectedItem {
                    path: e.path.clone(),
                    name: e.name.clone(),
                    is_dir: e.is_dir,
                    size: size_value(&e.size),
                }
            })
            .into_iter()
            .collect()
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.quit {
            self.poll_sizes();
            self.poll_delete();
            self.spinner_tick = self.spinner_tick.wrapping_add(1);
            terminal.draw(|f| self.draw(f))?;

            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key.code, key.modifiers);
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match self.mode {
            Mode::Normal => self.handle_normal_key(code, modifiers),
            Mode::Filter => self.handle_text_input_key(code, TextTarget::Filter),
            Mode::MinSize => self.handle_text_input_key(code, TextTarget::MinSize),
            Mode::GotoPath => self.handle_text_input_key(code, TextTarget::GotoPath),
            Mode::ConfirmDelete => self.handle_confirm_key(code),
            Mode::CleanMenu => self.handle_clean_menu_key(code),
            Mode::Help => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                    self.mode = Mode::Normal;
                }
            }
        }
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        match code {
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if matches!(self.view, ViewKind::Clean(_)) {
                    self.leave_clean_view();
                } else if matches!(self.view, ViewKind::Drives) {
                    self.leave_drives_view();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.cursor + 1 < self.filtered.len() {
                    self.cursor += 1;
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.enter_selected(),
            KeyCode::Left | KeyCode::Backspace | KeyCode::Char('h') => self.go_parent(),
            KeyCode::Char(' ') => self.toggle_select(),
            KeyCode::Char('d') | KeyCode::Delete => {
                if self.delete_progress.is_some() {
                    return;
                }
                let candidates = self.delete_candidates();
                if candidates.is_empty() {
                    return;
                }
                if let Some(item) = candidates
                    .iter()
                    .find(|c| crate::cache_paths::is_protected_path(&c.path))
                {
                    self.status = format!(
                        "refused: \"{}\" is a protected system location and cannot be removed",
                        item.name
                    );
                } else {
                    self.pending_delete = candidates;
                    self.mode = Mode::ConfirmDelete;
                }
            }
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                self.input = self.filter.clone();
            }
            KeyCode::Char('m') => {
                self.mode = Mode::MinSize;
                self.input.clear();
            }
            KeyCode::Char('g') => {
                self.mode = Mode::GotoPath;
                self.input = self.current_dir.to_string_lossy().into_owned();
            }
            KeyCode::Char('D') if cfg!(windows) => {
                self.view = ViewKind::Drives;
                self.filter.clear();
                self.load_drives();
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('c') => {
                self.mode = Mode::CleanMenu;
                self.clean_menu_cursor = 0;
            }
            KeyCode::Char('s') => {
                let target = self.cursor_target();
                self.sort_mode = match self.sort_mode {
                    SortMode::SizeDesc => SortMode::SizeAsc,
                    SortMode::SizeAsc => SortMode::NameAsc,
                    SortMode::NameAsc => SortMode::SizeDesc,
                };
                self.sort_entries();
                self.apply_filter();
                self.restore_cursor(target);
            }
            KeyCode::Char('r') => {
                let target = self.cursor_target();
                match self.view {
                    ViewKind::Explorer => {
                        self.size_cache
                            .retain(|p, _| !p.starts_with(&self.current_dir));
                    }
                    ViewKind::Clean(_) | ViewKind::Drives => {
                        for entry in &self.entries {
                            self.size_cache.remove(&entry.path);
                        }
                    }
                }
                self.refresh_view();
                self.restore_cursor(target);
            }
            _ => {}
        }
    }

    fn handle_clean_menu_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Up | KeyCode::Char('k') => {
                if self.clean_menu_cursor > 0 {
                    self.clean_menu_cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.clean_menu_cursor + 1 < CacheCategory::ALL.len() {
                    self.clean_menu_cursor += 1;
                }
            }
            KeyCode::Char('1') => self.open_clean_view(CacheCategory::Ai),
            KeyCode::Char('2') => self.open_clean_view(CacheCategory::System),
            KeyCode::Enter => {
                self.open_clean_view(CacheCategory::ALL[self.clean_menu_cursor]);
            }
            _ => {}
        }
    }

    fn enter_selected(&mut self) {
        let Some(entry) = self.selected_entry() else {
            return;
        };
        if entry.is_dir && !matches!(entry.size, Size::Denied) {
            self.current_dir = entry.path.clone();
            self.view = ViewKind::Explorer;
            self.filter.clear();
            self.load_dir();
        }
    }

    fn go_parent(&mut self) {
        if matches!(self.view, ViewKind::Clean(_)) {
            self.leave_clean_view();
            return;
        }
        if matches!(self.view, ViewKind::Drives) {
            return;
        }
        if let Some(parent) = self.current_dir.parent() {
            let child = self.current_dir.clone();
            self.current_dir = parent.to_path_buf();
            self.filter.clear();
            self.load_dir();
            if let Some(pos) = self
                .filtered
                .iter()
                .position(|&i| self.entries[i].path == child)
            {
                self.cursor = pos;
            }
        } else if cfg!(windows) {
            // Drive roots (e.g. `C:\`) have no filesystem parent — offer a
            // drive picker instead of doing nothing.
            self.view = ViewKind::Drives;
            self.filter.clear();
            self.load_drives();
        }
    }

    fn toggle_select(&mut self) {
        let Some(&i) = self.filtered.get(self.cursor) else {
            return;
        };
        let path = self.entries[i].path.clone();
        if let Some(pos) = self.selection.iter().position(|s| s.path == path) {
            self.selection.remove(pos);
            self.entries[i].selected = false;
        } else {
            let e = &self.entries[i];
            self.selection.push(SelectedItem {
                path: e.path.clone(),
                name: e.name.clone(),
                is_dir: e.is_dir,
                size: size_value(&e.size),
            });
            self.entries[i].selected = true;
        }
    }

    fn handle_text_input_key(&mut self, code: KeyCode, target: TextTarget) {
        match code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.input.clear();
            }
            KeyCode::Enter => {
                self.commit_text_input(target);
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.input.pop();
                if let TextTarget::Filter = target {
                    self.filter = self.input.clone();
                    self.apply_filter();
                }
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                if let TextTarget::Filter = target {
                    self.filter = self.input.clone();
                    self.apply_filter();
                }
            }
            _ => {}
        }
    }

    fn commit_text_input(&mut self, target: TextTarget) {
        match target {
            TextTarget::Filter => {
                self.filter = self.input.clone();
                self.apply_filter();
            }
            TextTarget::MinSize => match crate::parse_size(&self.input) {
                Ok(size) => {
                    self.min_size = size;
                    self.apply_filter();
                }
                Err(err) => self.status = err,
            },
            TextTarget::GotoPath => {
                let path = PathBuf::from(&self.input);
                if path.is_dir() {
                    self.current_dir = path.canonicalize().unwrap_or(path);
                    self.filter.clear();
                    self.load_dir();
                } else {
                    self.status = format!("not a directory: {}", self.input);
                }
            }
        }
        self.input.clear();
    }

    fn handle_confirm_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.start_delete();
                self.mode = Mode::Normal;
            }
            _ => {
                self.pending_delete.clear();
                self.mode = Mode::Normal;
            }
        }
    }

    fn start_delete(&mut self) {
        let items = std::mem::take(&mut self.pending_delete);
        // The whole cross-folder selection is being resolved by this delete
        // (whether it succeeds, partially fails, or was reached via the
        // cursor-fallback path) — clear it now so re-visited directories
        // don't keep showing stale checkboxes.
        self.selection.clear();

        let mut targets: Vec<(PathBuf, bool, String)> = Vec::new();
        let mut blocked = 0;
        for item in items {
            // Belt-and-suspenders: the 'd' key handler already refuses to
            // enter ConfirmDelete for a protected path, but never delete one
            // here either, regardless of how a target path was constructed.
            if crate::cache_paths::is_protected_path(&item.path) {
                blocked += 1;
                continue;
            }
            targets.push((item.path, item.is_dir, item.name));
        }
        let total = targets.len();
        if total == 0 {
            if blocked > 0 {
                self.status = "refused to delete a protected system location".to_string();
            }
            return;
        }

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut deleted = 0;
            let mut failed = blocked;
            for (done, (path, is_dir, name)) in targets.into_iter().enumerate() {
                let _ = tx.send(DeleteMsg::Progress { done, total, name });
                let result = if is_dir {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_file(&path)
                };
                match result {
                    Ok(()) => deleted += 1,
                    Err(_) => failed += 1,
                }
            }
            let _ = tx.send(DeleteMsg::Finished { deleted, failed });
        });

        self.delete_rx = Some(rx);
        self.delete_progress = Some(DeleteProgress {
            done: 0,
            total,
            name: String::new(),
        });
    }

    fn poll_delete(&mut self) {
        let Some(rx) = &self.delete_rx else {
            return;
        };

        let mut finished = None;
        for msg in rx.try_iter() {
            match msg {
                DeleteMsg::Progress { done, total, name } => {
                    self.delete_progress = Some(DeleteProgress { done, total, name });
                }
                DeleteMsg::Finished { deleted, failed } => finished = Some((deleted, failed)),
            }
        }

        if let Some((deleted, failed)) = finished {
            self.delete_rx = None;
            self.delete_progress = None;
            self.status = if failed == 0 {
                format!("deleted {deleted} item(s)")
            } else {
                format!("deleted {deleted} item(s), {failed} failed")
            };
            self.refresh_view();
        }
    }

    fn draw(&self, f: &mut Frame) {
        let area = f.area();
        let layout = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        self.draw_header(f, layout[0]);
        match self.mode {
            Mode::CleanMenu => self.draw_clean_menu(f, layout[1]),
            Mode::Help => self.draw_help(f, layout[1]),
            Mode::ConfirmDelete => self.draw_delete_review(f, layout[1]),
            _ => self.draw_list(f, layout[1]),
        }
        self.draw_status(f, layout[2]);
        self.draw_footer(f, layout[3]);
    }

    fn draw_header(&self, f: &mut Frame, area: Rect) {
        let path_text = match (&self.mode, self.view) {
            (Mode::GotoPath, _) => format!("go to: {}", self.input),
            (_, ViewKind::Clean(category)) => {
                format!("{} — review items, then select & delete", category.label())
            }
            (_, ViewKind::Drives) => "select a drive".to_string(),
            (_, ViewKind::Explorer) => format!("path: {}", self.current_dir.display()),
        };

        let pending = self.pending_count();
        let mut spans = vec![Span::styled(
            path_text,
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if !self.selection.is_empty() && !matches!(self.mode, Mode::ConfirmDelete) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{} selected across folders", self.selection.len()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let line = if pending > 0 {
            const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
            let frame = SPINNER[self.spinner_tick % SPINNER.len()];
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("{frame} scanning… ({pending} pending)"),
                Style::default().fg(Color::Green),
            ));
            Line::from(spans)
        } else {
            Line::from(spans)
        };
        f.render_widget(Paragraph::new(line), area);
    }

    fn draw_list(&self, f: &mut Frame, area: Rect) {
        let rows: Vec<Row> = self
            .filtered
            .iter()
            .map(|&i| {
                let e = &self.entries[i];
                let mark = if e.selected { "[x]" } else { "[ ]" };
                let icon = if e.is_dir { "📁" } else { "📄" };
                let (size_text, size_style) = match e.size {
                    Size::Pending => ("...".to_string(), Style::default().fg(Color::DarkGray)),
                    Size::Known(s) => (format_size(s, DECIMAL), Style::default()),
                    Size::Partial(s) => (
                        format!("{}+", format_size(s, DECIMAL)),
                        Style::default().fg(Color::Yellow),
                    ),
                    Size::Denied => ("no access".to_string(), Style::default().fg(Color::Red)),
                };
                let protected = crate::cache_paths::is_protected_path(&e.path);
                let name_style = if protected {
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD)
                } else if matches!(e.size, Size::Denied) {
                    Style::default().fg(Color::Red)
                } else if e.is_dir {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                };
                let name = if protected {
                    format!("🔒 {} (protected, cannot remove)", e.name)
                } else {
                    e.name.clone()
                };
                Row::new(vec![
                    Cell::from(format!("{mark} {icon}")),
                    Cell::from(name).style(name_style),
                    Cell::from(Line::from(size_text).alignment(Alignment::Right)).style(size_style),
                ])
            })
            .collect();

        let mut state = ratatui::widgets::TableState::default();
        if !self.filtered.is_empty() {
            state.select(Some(self.cursor));
        }

        let table = Table::new(
            rows,
            [
                Constraint::Length(6),
                Constraint::Min(10),
                Constraint::Length(14),
            ],
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("large-file-finder"),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        f.render_stateful_widget(table, area, &mut state);
    }

    /// Read-only final review of everything about to be deleted — the whole
    /// point of letting selections span multiple folders is that the user
    /// can no longer see them all in one screen while browsing, so this is
    /// the one place that lists every target together before it's too late.
    fn draw_delete_review(&self, f: &mut Frame, area: Rect) {
        let rows: Vec<Row> = self
            .pending_delete
            .iter()
            .map(|item| {
                let icon = if item.is_dir { "📁" } else { "📄" };
                Row::new(vec![
                    Cell::from(icon),
                    Cell::from(item.name.clone()),
                    Cell::from(
                        Line::from(format_size(item.size, DECIMAL)).alignment(Alignment::Right),
                    ),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(14),
            ],
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red))
                .title("Confirm delete — review before you press y"),
        );

        f.render_widget(table, area);
    }

    fn draw_clean_menu(&self, f: &mut Frame, area: Rect) {
        let lines: Vec<Line> = CacheCategory::ALL
            .iter()
            .enumerate()
            .map(|(i, category)| {
                let marker = if i == self.clean_menu_cursor {
                    "> "
                } else {
                    "  "
                };
                let style = if i == self.clean_menu_cursor {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                Line::from(Span::styled(format!("{marker}{}", category.label()), style))
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(
            "Clear caches & temp files — pick a category (↑/↓ + enter, or 1/2, Esc to cancel)",
        );
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_help(&self, f: &mut Frame, area: Rect) {
        const GROUPS: &[(&str, &[(&str, &str)])] = &[
            (
                "Navigate",
                &[
                    ("↑/↓, j/k", "move"),
                    ("→, enter, l", "open directory"),
                    ("←, backspace, h", "up / back"),
                    ("g", "go to path"),
                    ("D", "list drives (Windows)"),
                ],
            ),
            (
                "Find",
                &[("/", "filter by name"), ("m", "filter by min size")],
            ),
            (
                "Select & remove",
                &[
                    ("space", "select (persists across folders)"),
                    ("d, delete", "review & delete selection"),
                ],
            ),
            (
                "View",
                &[
                    ("s", "cycle sort"),
                    ("r", "refresh"),
                    ("c", "clean caches & temp files"),
                ],
            ),
            (
                "Other",
                &[("?", "toggle this help"), ("q, esc", "quit / back")],
            ),
        ];

        let mut lines = Vec::new();
        for (group, bindings) in GROUPS {
            lines.push(Line::from(Span::styled(
                *group,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            for (keys, desc) in *bindings {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {keys:<18}"), Style::default().fg(Color::Yellow)),
                    Span::raw(*desc),
                ]));
            }
            lines.push(Line::raw(""));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .title("Keyboard shortcuts (Esc/? to close)");
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        let text = match self.mode {
            Mode::Filter => format!("filter> {}", self.input),
            Mode::MinSize => format!("min size (e.g. 10M)> {}", self.input),
            Mode::GotoPath => String::new(),
            Mode::CleanMenu => String::new(),
            Mode::Help => String::new(),
            Mode::ConfirmDelete => {
                let total: u64 = self.pending_delete.iter().map(|i| i.size).sum();
                format!(
                    "delete these {} item(s), {} total — permanent, cannot be undone (y/n)",
                    self.pending_delete.len(),
                    format_size(total, DECIMAL)
                )
            }
            Mode::Normal => self.status.clone(),
        };

        if let Some(progress) = &self.delete_progress {
            const SPINNER: [char; 4] = ['|', '/', '-', '\\'];
            let frame = SPINNER[self.spinner_tick % SPINNER.len()];
            let text = format!(
                "{frame} deleting {}/{}: {}…",
                (progress.done + 1).min(progress.total),
                progress.total,
                progress.name
            );
            let style = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
            f.render_widget(Paragraph::new(text).style(style), area);
            return;
        }

        let style = if matches!(self.mode, Mode::ConfirmDelete) {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Yellow)
        };
        f.render_widget(Paragraph::new(text).style(style), area);
    }

    fn draw_footer(&self, f: &mut Frame, area: Rect) {
        let help = "↑/↓ move  enter open  space select  d delete  ? help  q quit";
        f.render_widget(
            Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }
}

enum TextTarget {
    Filter,
    MinSize,
    GotoPath,
}

fn dir_size_to_size(dir_size: DirSize) -> Size {
    match dir_size {
        DirSize::Bytes(s) => Size::Known(s),
        DirSize::Partial(s) => Size::Partial(s),
        DirSize::Denied => Size::Denied,
    }
}

fn size_value(size: &Size) -> u64 {
    match size {
        Size::Known(s) | Size::Partial(s) => *s,
        Size::Pending | Size::Denied => 0,
    }
}
