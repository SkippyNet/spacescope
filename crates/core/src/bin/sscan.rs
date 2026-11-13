//! Headless scanner: prints a summary, optionally exports CSV. Doubles as the
//! benchmark harness for the scanner and layout.

use spacescope_core::fmt::{human_count, human_size};
use spacescope_core::{export, layout, scan_path, Filter, LayoutOptions, Metric, ScanOptions};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const HELP: &str = "\
sscan - fast disk usage scanner

USAGE:
    sscan [PATH] [OPTIONS]

OPTIONS:
    --top N          Show the N largest entries (default 15)
    --threads N      Worker threads (default: one per CPU, max 32)
    --logical        Rank by logical size instead of size on disk
    --filter EXPR    Only count matching files, e.g. \"*.mp4;>100mb\"
    --csv FILE       Export every file and directory as CSV
    --bench          Also time treemap layout and filtering
    --no-mft         Windows: always walk folders, never read the NTFS
                     master file table (used automatically when run as
                     administrator)
    -V, --version    Show the version
    -h, --help       Show this help
";

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}\n\n{HELP}");
    std::process::exit(2)
}

fn main() {
    let mut path: Option<PathBuf> = None;
    let mut top = 15usize;
    let mut threads = 0usize;
    let mut metric = Metric::Allocated;
    let mut filter_text: Option<String> = None;
    let mut csv: Option<PathBuf> = None;
    let mut bench = false;
    let mut no_mft = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .unwrap_or_else(|| fail(&format!("{name} needs a value")))
        };
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "--top" => {
                top = value("--top")
                    .parse()
                    .unwrap_or_else(|_| fail("--top expects a number"))
            }
            "--threads" => {
                threads = value("--threads")
                    .parse()
                    .unwrap_or_else(|_| fail("--threads expects a number"))
            }
            "--logical" => metric = Metric::Logical,
            "--filter" => filter_text = Some(value("--filter")),
            "--csv" => csv = Some(PathBuf::from(value("--csv"))),
            "--bench" => bench = true,
            "--no-mft" => no_mft = true,
            "-V" | "--version" => {
                println!("sscan {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            s if s.starts_with('-') => fail(&format!("unknown option {s}")),
            s if path.is_none() => path = Some(PathBuf::from(s)),
            s => fail(&format!("unexpected argument {s}")),
        }
    }
    let path = path.unwrap_or_else(|| PathBuf::from("."));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let filter = match &filter_text {
        Some(t) => match Filter::parse(t, now) {
            Ok(f) => Some(f),
            Err(e) => fail(&format!("bad filter: {e}")),
        },
        None => None,
    };

    let opts = ScanOptions {
        threads,
        on_done: None,
        no_mft,
    };
    let (shared, progress) = match scan_path(&path, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: cannot scan {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    progress.wait();
    let secs = progress.elapsed().as_secs_f64();
    let tree = shared.tree.read();
    let root = *tree.node(0);
    let files = progress.files.load(Ordering::Relaxed);
    let dirs = progress.dirs.load(Ordering::Relaxed);

    println!("Path        {}", tree.root_path.display());
    println!("Files       {}", human_count(files));
    println!("Folders     {}", human_count(dirs));
    println!(
        "Size        {} ({} bytes)",
        human_size(root.size),
        human_count(root.size)
    );
    println!(
        "On disk     {} ({} bytes)",
        human_size(root.alloc),
        human_count(root.alloc)
    );
    println!(
        "Unreadable  {}",
        human_count(progress.errors.load(Ordering::Relaxed))
    );
    println!(
        "Time        {:.3} s  ({} entries/s)",
        secs,
        human_count(((files + dirs) as f64 / secs.max(1e-9)) as u64)
    );
    println!("Tree memory {}", human_size(tree.memory_bytes() as u64));
    println!(
        "Method      {}",
        if progress.used_mft() {
            "NTFS master file table"
        } else {
            "folder walk"
        }
    );

    let mut weights = Vec::new();
    if let Some(f) = &filter {
        let t = Instant::now();
        f.compute_weights(&tree, metric, &mut weights);
        println!(
            "Filter      {} match in {:.1} ms",
            human_size(weights[0]),
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    let weight = |id: u32| -> u64 {
        if filter.is_some() {
            weights[id as usize]
        } else {
            match metric {
                Metric::Allocated => tree.node(id).alloc,
                Metric::Logical => tree.node(id).size,
            }
        }
    };

    if top > 0 {
        let mut kids: Vec<u32> = tree.children(0).filter(|&c| weight(c) > 0).collect();
        kids.sort_unstable_by_key(|&c| std::cmp::Reverse(weight(c)));
        let total = weight(0).max(1);
        println!();
        for &c in kids.iter().take(top) {
            let n = tree.node(c);
            println!(
                "{:>10}  {:>5.1}%  {}{}",
                human_size(weight(c)),
                weight(c) as f64 * 100.0 / total as f64,
                tree.name_lossy(c),
                if n.is_dir() {
                    std::path::MAIN_SEPARATOR_STR
                } else {
                    ""
                }
            );
        }
        if kids.len() > top {
            println!("            ...   {} more", kids.len() - top);
        }
    }

    if bench {
        println!();
        for (w, h) in [(1920.0, 1080.0), (3840.0, 2160.0)] {
            let o = LayoutOptions {
                width: w,
                height: h,
                metric,
                ..Default::default()
            };
            let t = Instant::now();
            let l = layout::compute(&tree, None, 0, &o);
            println!(
                "Layout {w}x{h}: {} tiles, {} labels in {:.2} ms",
                l.tiles.len(),
                l.labels.len(),
                t.elapsed().as_secs_f64() * 1e3
            );
        }
        let f = Filter::parse("*.dll;>1mb", now).unwrap();
        let t = Instant::now();
        f.compute_weights(&tree, metric, &mut weights);
        println!(
            "Filter '*.dll;>1mb' over {} nodes in {:.1} ms",
            human_count(tree.len() as u64),
            t.elapsed().as_secs_f64() * 1e3
        );
    }

    if let Some(file) = csv {
        let t = Instant::now();
        let result = std::fs::File::create(&file)
            .map(std::io::BufWriter::new)
            .and_then(|w| export::write_csv(&tree, 0, w));
        match result {
            Ok(rows) => println!(
                "\nWrote {} rows to {} in {:.2} s",
                human_count(rows),
                file.display(),
                t.elapsed().as_secs_f64()
            ),
            Err(e) => {
                eprintln!("error: cannot write {}: {e}", file.display());
                std::process::exit(1);
            }
        }
    }
}
