mod cache_paths;
mod tui;

use std::path::PathBuf;

use clap::Parser;
use humansize::{DECIMAL, format_size};
use walkdir::WalkDir;

/// Find the largest files under a directory tree.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Directory to search
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Only show files at least this many bytes (supports suffixes like 10M, 1G)
    #[arg(short = 's', long, default_value = "0", value_parser = parse_size)]
    min_size: u64,

    /// Number of results to show (--list mode only)
    #[arg(short = 'n', long, default_value_t = 20)]
    limit: usize,

    /// Follow symlinks while walking
    #[arg(long)]
    follow_links: bool,

    /// Print a flat sorted list instead of launching the interactive file explorer
    #[arg(long)]
    list: bool,
}

pub(crate) fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult): (&str, u64) = if let Some(prefix) = s.strip_suffix(['k', 'K']) {
        (prefix, 1_000)
    } else if let Some(prefix) = s.strip_suffix(['m', 'M']) {
        (prefix, 1_000_000)
    } else if let Some(prefix) = s.strip_suffix(['g', 'G']) {
        (prefix, 1_000_000_000)
    } else if let Some(prefix) = s.strip_suffix(['t', 'T']) {
        (prefix, 1_000_000_000_000)
    } else {
        (s, 1)
    };

    let value: f64 = num.parse().map_err(|_| format!("invalid size: {s}"))?;
    Ok((value * mult as f64) as u64)
}

fn main() {
    let args = Args::parse();

    if !args.list {
        let opts = tui::TuiOptions {
            root: args.path,
            min_size: args.min_size,
            follow_links: args.follow_links,
        };
        if let Err(err) = tui::run(opts) {
            eprintln!("error: {err}");
            std::process::exit(1);
        }
        return;
    }

    let mut entries: Vec<(PathBuf, u64)> = WalkDir::new(&args.path)
        .follow_links(args.follow_links)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(err) => {
                eprintln!("warning: {err}");
                None
            }
        })
        .filter(|entry| entry.file_type().is_file())
        .filter_map(|entry| {
            let size = entry.metadata().ok()?.len();
            (size >= args.min_size).then(|| (entry.into_path(), size))
        })
        .collect();

    entries.sort_by_key(|(_, size)| std::cmp::Reverse(*size));
    entries.truncate(args.limit);

    for (path, size) in &entries {
        println!("{:>12}  {}", format_size(*size, DECIMAL), path.display());
    }
}
