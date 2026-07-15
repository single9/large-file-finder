use std::path::{Path, PathBuf};

/// A named cache/temp location we know how to look for. `exists` filters candidates
/// down to what's actually present, so listing wrong guesses for other platforms or
/// uninstalled tools is harmless.
pub struct CacheEntry {
    pub label: &'static str,
    pub path: PathBuf,
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(windows)]
fn local_app_data() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
}

/// Rejects candidate paths that would be catastrophic to offer for deletion:
/// relative paths (a sign the source env var was empty), filesystem roots
/// (`path.parent()` is `None` on both Unix `/` and Windows `C:\`), and the
/// home directory itself. `std::env::temp_dir()` in particular is used
/// unvalidated and returns exactly these values when `$TMPDIR` is empty or
/// set to `/`.
fn is_dangerous_candidate(path: &Path) -> bool {
    if !path.is_absolute() || path.parent().is_none() {
        return true;
    }
    home_dir().is_some_and(|home| path == home)
}

/// Caches produced by AI/ML tools: model hubs, local inference runtimes, and
/// AI-assisted editors/desktop apps. Best-effort locations gathered from each
/// tool's documented defaults; anything not present on disk is simply skipped.
pub fn ai_cache_candidates() -> Vec<CacheEntry> {
    let mut v = Vec::new();

    if let Some(home) = home_dir() {
        v.push(CacheEntry {
            label: "Hugging Face hub cache",
            path: home.join(".cache/huggingface"),
        });
        v.push(CacheEntry {
            label: "PyTorch hub cache",
            path: home.join(".cache/torch"),
        });
        v.push(CacheEntry {
            label: "Whisper model cache",
            path: home.join(".cache/whisper"),
        });
        v.push(CacheEntry {
            label: "Ollama models",
            path: home.join(".ollama/models"),
        });
        v.push(CacheEntry {
            label: "LM Studio model cache",
            path: home.join(".cache/lm-studio"),
        });
    }

    #[cfg(target_os = "macos")]
    if let Some(home) = home_dir() {
        v.push(CacheEntry {
            label: "Claude desktop cache",
            path: home.join("Library/Application Support/Claude/Cache"),
        });
        v.push(CacheEntry {
            label: "ChatGPT desktop cache",
            path: home.join("Library/Application Support/ChatGPT/Cache"),
        });
        v.push(CacheEntry {
            label: "Cursor editor cache",
            path: home.join("Library/Application Support/Cursor/Cache"),
        });
        v.push(CacheEntry {
            label: "GitHub Copilot cache",
            path: home.join("Library/Caches/GitHub Copilot"),
        });
    }

    #[cfg(target_os = "windows")]
    if let Some(local) = local_app_data() {
        v.push(CacheEntry {
            label: "Claude desktop cache",
            path: local.join("Programs\\Claude\\Cache"),
        });
        v.push(CacheEntry {
            label: "Cursor editor cache",
            path: local.join("Programs\\cursor\\Cache"),
        });
        v.push(CacheEntry {
            label: "Ollama cache",
            path: local.join("Ollama"),
        });
    }

    #[cfg(target_os = "linux")]
    if let Some(home) = home_dir() {
        v.push(CacheEntry {
            label: "Claude desktop cache",
            path: home.join(".config/Claude/Cache"),
        });
        v.push(CacheEntry {
            label: "Cursor editor cache",
            path: home.join(".config/Cursor/Cache"),
        });
    }

    v.retain(|c| !is_dangerous_candidate(&c.path));
    v
}

/// General OS/application cache and temp locations.
pub fn system_cache_candidates() -> Vec<CacheEntry> {
    let mut v = vec![CacheEntry {
        label: "System temp directory",
        path: std::env::temp_dir(),
    }];

    #[cfg(target_os = "macos")]
    if let Some(home) = home_dir() {
        v.push(CacheEntry {
            label: "User cache library",
            path: home.join("Library/Caches"),
        });
        v.push(CacheEntry {
            label: "User logs",
            path: home.join("Library/Logs"),
        });
    }

    #[cfg(target_os = "linux")]
    if let Some(home) = home_dir() {
        v.push(CacheEntry {
            label: "User cache directory",
            path: home.join(".cache"),
        });
    }

    #[cfg(target_os = "linux")]
    {
        v.push(CacheEntry {
            label: "System var/tmp",
            path: PathBuf::from("/var/tmp"),
        });
    }

    #[cfg(windows)]
    if let Some(local) = local_app_data() {
        v.push(CacheEntry {
            label: "Local app data temp",
            path: local.join("Temp"),
        });
        v.push(CacheEntry {
            label: "Windows internet cache",
            path: local.join("Microsoft\\Windows\\INetCache"),
        });
    }

    v.retain(|c| !is_dangerous_candidate(&c.path));
    v
}
