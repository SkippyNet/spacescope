//! The filter language.
//!
//! Terms are separated by `;`:
//!
//! * `*.jpg`, `report?.doc`  name globs (case-insensitive)
//! * `steam`                 plain text matches anywhere in the name
//! * `>100mb`, `<2gb`        size limits (`b`, `kb`, `mb`, `gb`, `tb`)
//! * `>2years`, `<30days`    older than / newer than
//!   (`sec`, `min`, `hours`, `days`, `weeks`, `months`, `years`)
//! * `|*.tmp` or `!*.tmp`    exclude
//!
//! Name terms are OR-ed together; everything else is AND-ed. A directory whose
//! name matches an include term brings its whole content with it.

use crate::tree::{flags, Tree, NO_NODE};
use crate::Metric;

#[derive(Clone, Debug, PartialEq)]
enum Pattern {
    /// Lower-cased glob with `*` and `?`.
    Glob(Vec<u8>),
    /// Lower-cased substring.
    Contains(Vec<u8>),
    /// `*suffix` with no other wildcard: the common `*.ext` case.
    Suffix(Vec<u8>),
    /// `prefix*` with no other wildcard.
    Prefix(Vec<u8>),
}

#[inline]
fn eq_ci(text: &[u8], lower: &[u8]) -> bool {
    text.len() == lower.len()
        && text
            .iter()
            .zip(lower)
            .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

impl Pattern {
    fn new(text: &str) -> Pattern {
        let lower = text.to_lowercase().into_bytes();
        let wild = |s: &[u8]| s.iter().any(|&b| b == b'*' || b == b'?');
        let n = lower.len();
        if n > 1 && lower[0] == b'*' && !wild(&lower[1..]) {
            Pattern::Suffix(lower[1..].to_vec())
        } else if n > 1 && lower[n - 1] == b'*' && !wild(&lower[..n - 1]) {
            Pattern::Prefix(lower[..n - 1].to_vec())
        } else if wild(&lower) {
            Pattern::Glob(lower)
        } else {
            Pattern::Contains(lower)
        }
    }

    fn matches(&self, name: &[u8]) -> bool {
        match self {
            Pattern::Glob(p) => glob(p, name),
            Pattern::Contains(p) => contains_ci(name, p),
            Pattern::Suffix(p) => name.len() >= p.len() && eq_ci(&name[name.len() - p.len()..], p),
            Pattern::Prefix(p) => name.len() >= p.len() && eq_ci(&name[..p.len()], p),
        }
    }
}

fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if hay.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    for start in 0..=hay.len() - needle.len() {
        if hay[start].to_ascii_lowercase() == first
            && hay[start..start + needle.len()]
                .iter()
                .zip(needle)
                .all(|(a, b)| a.to_ascii_lowercase() == *b)
        {
            return true;
        }
    }
    false
}

/// Iterative glob match; `pat` must already be lower-case.
fn glob(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while t < text.len() {
        if p < pat.len() && (pat[p] == b'?' || pat[p] == text[t].to_ascii_lowercase()) {
            p += 1;
            t += 1;
        } else if p < pat.len() && pat[p] == b'*' {
            star = p;
            mark = t;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            t = mark;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    /// Keep files modified at or after this time.
    newer_than: Option<i64>,
    /// Keep files modified at or before this time.
    older_than: Option<i64>,
}

enum Quantity {
    Bytes(u64),
    Seconds(i64),
}

fn parse_quantity(s: &str) -> Result<Quantity, String> {
    let s = s.trim().to_lowercase();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num
        .replace(',', ".")
        .parse()
        .map_err(|_| format!("'{s}' is not a number with a unit, e.g. 100mb or 2years"))?;
    let unit = unit.trim();
    let bytes = |m: f64| Ok(Quantity::Bytes((value * m) as u64));
    let secs = |m: f64| Ok(Quantity::Seconds((value * m) as i64));
    match unit {
        "" | "b" | "byte" | "bytes" => bytes(1.0),
        "k" | "kb" | "kib" => bytes(1024.0),
        "m" | "mb" | "mib" => bytes(1024.0 * 1024.0),
        "g" | "gb" | "gib" => bytes(1024.0 * 1024.0 * 1024.0),
        "t" | "tb" | "tib" => bytes(1024.0 * 1024.0 * 1024.0 * 1024.0),
        "s" | "sec" | "secs" | "second" | "seconds" => secs(1.0),
        "min" | "mins" | "minute" | "minutes" => secs(60.0),
        "h" | "hour" | "hours" => secs(3600.0),
        "d" | "day" | "days" => secs(86_400.0),
        "w" | "week" | "weeks" => secs(604_800.0),
        "month" | "months" => secs(2_629_800.0),
        "y" | "year" | "years" => secs(31_557_600.0),
        other => Err(format!("unknown unit '{other}'")),
    }
}

impl Filter {
    /// Parses a filter expression. `now` is the current unix time, used to
    /// resolve age terms.
    pub fn parse(text: &str, now: i64) -> Result<Filter, String> {
        let mut f = Filter::default();
        for term in text.split(';') {
            let term = term.trim();
            if term.is_empty() {
                continue;
            }
            if let Some(rest) = term.strip_prefix('>') {
                match parse_quantity(rest.trim_start_matches('='))? {
                    Quantity::Bytes(b) => f.min_size = Some(f.min_size.map_or(b, |x| x.max(b))),
                    Quantity::Seconds(s) => {
                        let t = now - s;
                        f.older_than = Some(f.older_than.map_or(t, |x| x.min(t)));
                    }
                }
            } else if let Some(rest) = term.strip_prefix('<') {
                match parse_quantity(rest.trim_start_matches('='))? {
                    Quantity::Bytes(b) => f.max_size = Some(f.max_size.map_or(b, |x| x.min(b))),
                    Quantity::Seconds(s) => {
                        let t = now - s;
                        f.newer_than = Some(f.newer_than.map_or(t, |x| x.max(t)));
                    }
                }
            } else if let Some(rest) = term.strip_prefix(['|', '!']) {
                let rest = rest.trim();
                if !rest.is_empty() {
                    f.exclude.push(Pattern::new(rest));
                }
            } else {
                f.include.push(Pattern::new(term));
            }
        }
        Ok(f)
    }

    pub fn is_empty(&self) -> bool {
        *self == Filter::default()
    }

    #[inline]
    fn name_included(&self, name: &[u8]) -> bool {
        self.include.iter().any(|p| p.matches(name))
    }

    /// Everything except the include-name test.
    #[inline]
    fn passes_limits(&self, name: &[u8], size: u64, mtime: i64) -> bool {
        if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
            return false;
        }
        if self.newer_than.is_some_and(|t| mtime < t) || self.older_than.is_some_and(|t| mtime > t)
        {
            return false;
        }
        !self.exclude.iter().any(|p| p.matches(name))
    }

    /// Does a file pass the filter on its own?
    pub fn matches(&self, name: &[u8], size: u64, mtime: i64) -> bool {
        (self.include.is_empty() || self.name_included(name))
            && self.passes_limits(name, size, mtime)
    }

    /// Computes the filtered weight of every node into `out`.
    pub fn compute_weights(&self, tree: &Tree, metric: Metric, out: &mut Vec<u64>) {
        let n = tree.nodes.len();
        out.clear();
        out.resize(n, 0);

        // Forward pass: a directory matching an include term (and not excluded)
        // carries its content; an excluded directory drops its content.
        // 0 = undecided, 1 = forced in, 2 = forced out.
        let need_inherit = !self.include.is_empty() || !self.exclude.is_empty();
        let mut inherit: Vec<u8> = Vec::new();
        if need_inherit {
            inherit.resize(n, 0);
            for i in 1..n {
                let node = &tree.nodes[i];
                let up = inherit[node.parent as usize];
                if node.flags & flags::DIR == 0 {
                    inherit[i] = up;
                    continue;
                }
                let name = tree.name(i as u32);
                inherit[i] = if up == 2 || self.exclude.iter().any(|p| p.matches(name)) {
                    2
                } else if up == 1 || self.name_included(name) {
                    1
                } else {
                    0
                };
            }
        }

        for i in (0..n).rev() {
            let node = &tree.nodes[i];
            if node.flags & flags::DEAD != 0 {
                out[i] = 0;
                continue;
            }
            if node.flags & flags::DIR == 0 {
                let state = if need_inherit { inherit[i] } else { 0 };
                let name = tree.name(i as u32);
                let ok = match state {
                    2 => false,
                    1 => self.passes_limits(name, node.size, node.mtime),
                    _ => self.matches(name, node.size, node.mtime),
                };
                out[i] = if ok {
                    match metric {
                        Metric::Allocated => node.alloc,
                        Metric::Logical => node.size,
                    }
                } else {
                    0
                };
            }
            if out[i] > 0 && node.parent != NO_NODE {
                out[node.parent as usize] += out[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob(b"*.jpg", b"Photo.JPG"));
        assert!(!glob(b"*.jpg", b"photo.jpeg"));
        assert!(glob(b"a?c*", b"abcdef"));
        assert!(glob(b"*", b""));
        assert!(glob(b"*a*b*", b"xxaxxbxx"));
        assert!(!glob(b"a*b", b"ac"));
    }

    #[test]
    fn parse_and_match() {
        let now = 1_000_000_000;
        let f = Filter::parse("*.mp4; *.mkv; >100mb; <2years; |sample*", now).unwrap();
        let mb = 1024 * 1024;
        assert!(f.matches(b"film.mkv", 200 * mb, now - 1000));
        assert!(!f.matches(b"film.mkv", 50 * mb, now - 1000));
        assert!(!f.matches(b"film.avi", 200 * mb, now - 1000));
        assert!(!f.matches(b"sample.mkv", 200 * mb, now - 1000));
        assert!(!f.matches(b"film.mkv", 200 * mb, now - 3 * 31_557_600));

        let old = Filter::parse(">1year", now).unwrap();
        assert!(old.matches(b"x", 1, now - 2 * 31_557_600));
        assert!(!old.matches(b"x", 1, now - 10));

        assert!(Filter::parse("", now).unwrap().is_empty());
        assert!(Filter::parse(" ; ", now).unwrap().is_empty());
        assert!(Filter::parse(">abc", now).is_err());
        assert!(Filter::parse(">5parsecs", now).is_err());

        let sub = Filter::parse("Steam", now).unwrap();
        assert!(sub.matches(b"my_steam_file", 1, 0));
        let pre = Filter::parse("IMG_*", now).unwrap();
        assert!(pre.matches(b"img_0001.jpg", 1, 0));
        assert!(!pre.matches(b"my_img_1", 1, 0));
        let suf = Filter::parse("*.JPG", now).unwrap();
        assert!(suf.matches(b"a.jpg", 1, 0));
        assert!(!suf.matches(b"jpg", 1, 0));
        assert!(!sub.matches(b"other", 1, 0));
    }
}
