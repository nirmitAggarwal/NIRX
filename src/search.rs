//! Fuzzy matching and bounded recursive search.
//!
//! Matching is a subsequence scorer with bonuses for consecutive characters,
//! word boundaries and a prefix hit, which behaves like the fuzzy finders used
//! by the rest of the DMS stack without pulling in a dependency.

use crate::fsutil::FileEntry;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Score `needle` against `haystack`; `None` when it does not match at all.
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.to_lowercase().chars().collect();
    let neg: Vec<char> = needle.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut hi = 0usize;
    let mut last_match: Option<usize> = None;
    for nc in neg {
        let mut found = None;
        while hi < hay.len() {
            if hay[hi] == nc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let idx = found?;
        score += 10;
        if let Some(last) = last_match {
            if idx == last + 1 {
                score += 8;
            }
            score -= (idx - last - 1).min(6) as i32;
        } else if idx == 0 {
            score += 12;
        }
        let boundary = idx == 0
            || matches!(
                hay.get(idx - 1),
                Some(' ') | Some('_') | Some('-') | Some('.') | Some('/')
            );
        if boundary {
            score += 6;
        }
        if haystack
            .chars()
            .nth(idx)
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
        {
            score += 2;
        }
        last_match = Some(idx);
        hi = idx + 1;
    }
    // Prefer shorter, more targeted names.
    score -= (hay.len() as i32 / 8).min(20);
    Some(score)
}

pub fn matches(needle: &str, haystack: &str) -> bool {
    fuzzy_score(needle, haystack).is_some()
}

/// What the search field matches against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchMode {
    Name,
    Extension,
}

impl Default for SearchMode {
    fn default() -> Self {
        SearchMode::Name
    }
}

/// Match a query against a file extension.
///
/// The query may be written with or without a leading dot and is treated as a
/// prefix, so `jp` finds `.jpg` and `.jpeg`, while `png` only finds `.png`.
/// `none` matches files without an extension.
pub fn matches_extension(query: &str, extension: &str) -> bool {
    let query = query.trim().trim_start_matches('.').to_lowercase();
    if query.is_empty() {
        return true;
    }
    let ext = extension.trim_start_matches('.').to_lowercase();
    if ext.is_empty() {
        return query == "none" || query == "no-extension";
    }
    ext.starts_with(&query)
}

/// Recursively collect entries under `root` that match `query`.
///
/// The walk is breadth-first, skips hidden directories unless asked, never
/// follows symlinked directories and stops at `limit` results. `cancel` lets
/// the UI abandon a stale search without waiting for it to finish.
pub fn search_recursive(
    root: &Path,
    query: &str,
    mode: SearchMode,
    show_hidden: bool,
    limit: usize,
    cancel: &AtomicBool,
) -> Vec<FileEntry> {
    use std::collections::VecDeque;
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) || out.len() >= limit {
            break;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for dent in read.flatten() {
            if cancel.load(Ordering::Relaxed) || out.len() >= limit {
                break;
            }
            let name = dent.file_name().to_string_lossy().to_string();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let path = dent.path();
            let Ok(meta) = dent.metadata() else { continue };
            let is_dir = meta.is_dir();
            if is_dir {
                if !meta.file_type().is_symlink() {
                    queue.push_back(path.clone());
                }
            }
            let hit = match mode {
                SearchMode::Name => fuzzy_score(query, &name).is_some(),
                SearchMode::Extension => {
                    let ext = path
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_default();
                    matches_extension(query, &ext)
                }
            };
            if hit {
                if let Some(entry) = FileEntry::from_path(&path, None) {
                    out.push(entry);
                }
            }
        }
    }
    match mode {
        SearchMode::Name => out.sort_by(|a, b| {
            let sa = fuzzy_score(query, &a.name).unwrap_or(0);
            let sb = fuzzy_score(query, &b.name).unwrap_or(0);
            sb.cmp(&sa).then_with(|| a.name.cmp(&b.name))
        }),
        SearchMode::Extension => out.sort_by(|a, b| {
            a.extension()
                .to_lowercase()
                .cmp(&b.extension().to_lowercase())
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        }),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches() {
        assert!(fuzzy_score("dms", "DankMaterialShell").is_some());
        assert!(fuzzy_score("nook", "nook").is_some());
        assert!(fuzzy_score("xyz", "nook").is_none());
    }

    #[test]
    fn prefix_scores_higher_than_scattered() {
        let prefix = fuzzy_score("re", "report.txt").unwrap();
        let scattered = fuzzy_score("re", "very_large_reminder.txt").unwrap();
        assert!(prefix > scattered, "{prefix} vs {scattered}");
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn extension_search_accepts_prefix_and_optional_dot() {
        assert!(matches_extension("png", "png"));
        assert!(matches_extension(".PNG", "png"));
        assert!(matches_extension("jp", "jpg"));
        assert!(matches_extension("jp", "jpeg"));
        assert!(!matches_extension("png", "jpg"));
        assert!(matches_extension("none", ""));
        assert!(!matches_extension("pdf", ""));
        assert!(matches_extension("  ", ""));
    }
}
