//! Filesystem model and operations.
//!
//! Directory reads, sorting, conflict-safe copies/moves and trash handling live
//! here so the UI layer never touches `std::fs` directly. Expensive or
//! potentially blocking work is invoked from background threads by the UI.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitState {
    Modified,
    Added,
    Deleted,
    Untracked,
    Conflicted,
}

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_broken: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub mime: String,
    pub is_hidden: bool,
    pub git: Option<GitState>,
    pub mode: u32,
    /// Depth below the root in a recursive listing (0 for direct children).
    pub depth: u8,
    /// Folder the entry lives in, relative to a recursive listing's root.
    /// Empty for direct children.
    pub rel_parent: String,
}

impl FileEntry {
    pub fn from_path(path: &Path, git: Option<GitState>) -> Option<Self> {
        let name = path.file_name()?.to_string_lossy().to_string();
        let link_meta = fs::symlink_metadata(path).ok()?;
        let is_symlink = link_meta.file_type().is_symlink();
        let resolved = if is_symlink {
            fs::metadata(path).ok()
        } else {
            Some(link_meta.clone())
        };
        let is_dir = resolved.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = resolved.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = resolved.as_ref().and_then(|m| m.modified().ok());
        let mode = link_meta.mode();
        let symlink_broken = is_symlink && resolved.is_none();
        let mime = if is_dir {
            "inode/directory".to_string()
        } else {
            gio::content_type_guess(Some(name.as_str()), None::<&[u8]>)
                .0
                .to_string()
        };
        Some(Self {
            path: path.to_path_buf(),
            is_hidden: name.starts_with('.'),
            name,
            is_dir,
            is_symlink,
            symlink_broken,
            size,
            modified,
            mime,
            git,
            mode,
            depth: 0,
            rel_parent: String::new(),
        })
    }

    pub fn extension(&self) -> String {
        Path::new(&self.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    }
}

use std::os::unix::fs::MetadataExt;

/// Read a directory, applying hidden filtering and git decoration.
pub fn list_dir(path: &Path, show_hidden: bool) -> io::Result<Vec<FileEntry>> {
    let git = git_status(path);
    let mut out = Vec::new();
    for dent in fs::read_dir(path)? {
        let Ok(dent) = dent else { continue };
        let name = dent.file_name();
        if name == OsStr::new(".git") {
            continue;
        }
        let hidden = name.to_string_lossy().starts_with('.');
        if hidden && !show_hidden {
            continue;
        }
        let git_state = git.get(name.to_string_lossy().as_ref()).copied();
        if let Some(entry) = FileEntry::from_path(&dent.path(), git_state) {
            out.push(entry);
        }
    }
    Ok(out)
}

pub fn sort_entries(entries: &mut [FileEntry], key: crate::settings::SortKey, desc: bool) {
    use crate::settings::SortKey;
    entries.sort_by(|a, b| {
        // Directories always group before files, independent of direction.
        let dir_cmp = b.is_dir.cmp(&a.is_dir);
        if dir_cmp != std::cmp::Ordering::Equal {
            return dir_cmp;
        }
        let ord = match key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Size => a
                .size
                .cmp(&b.size)
                .then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Kind => a
                .mime
                .cmp(&b.mime)
                .then_with(|| natural_cmp(&a.name, &b.name)),
        };
        if desc {
            ord.reverse()
        } else {
            ord
        }
    });
}

/// Case-insensitive comparison that keeps `file2` before `file10`.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let na: String =
                        std::iter::from_fn(|| ai.next_if(|c| c.is_ascii_digit())).collect();
                    let nb: String =
                        std::iter::from_fn(|| bi.next_if(|c| c.is_ascii_digit())).collect();
                    let va = na.trim_start_matches('0');
                    let vb = nb.trim_start_matches('0');
                    let ord = va.len().cmp(&vb.len()).then_with(|| va.cmp(vb));
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                    continue;
                }
                if ca != cb {
                    return ca.cmp(&cb);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Map of entry name (first path component) to its git state for `dir`.
pub fn git_status(dir: &Path) -> HashMap<String, GitState> {
    let mut map = HashMap::new();
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=normal",
            "--ignored=no",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output();
    let Ok(output) = output else { return map };
    if !output.status.success() {
        return map;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut fields = text.split('\0');
    while let Some(record) = fields.next() {
        if record.len() < 4 {
            continue;
        }
        let code = &record[..2];
        let path = &record[3..];
        // Rename records carry the original path as the next NUL field.
        if code.starts_with('R') || code.starts_with('C') {
            let _ = fields.next();
        }
        let top = path.split('/').next().unwrap_or(path);
        if top.is_empty() || top == ".git" {
            continue;
        }
        let state = classify_git(code);
        let replace = match (map.get(top), state) {
            (None, _) => true,
            (Some(GitState::Conflicted), _) => false,
            (_, GitState::Conflicted) => true,
            (Some(GitState::Modified), _) => false,
            _ => true,
        };
        if replace {
            map.insert(top.to_string(), state);
        }
    }
    map
}

fn classify_git(code: &str) -> GitState {
    if code.contains('U') || code == "AA" || code == "DD" {
        GitState::Conflicted
    } else if code.contains('A') || code.contains('?') {
        if code.contains('?') {
            GitState::Untracked
        } else {
            GitState::Added
        }
    } else if code.contains('D') {
        GitState::Deleted
    } else {
        GitState::Modified
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_time(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "—".into();
    };
    let secs = time
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now = glib::DateTime::now_local()
        .ok()
        .map(|d| d.to_unix())
        .unwrap_or(secs);
    let Ok(dt) = glib::DateTime::from_unix_local(secs) else {
        return "—".into();
    };
    let pattern = if now - secs < 86_400 * 300 {
        "%b %e  %H:%M"
    } else {
        "%b %e  %Y"
    };
    dt.format(pattern)
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "—".into())
}

pub fn kind_label(entry: &FileEntry) -> String {
    if entry.is_dir {
        return "Folder".into();
    }
    if entry.is_symlink {
        return "Link".into();
    }
    let mime = &entry.mime;
    if mime.is_empty() || mime == "application/octet-stream" {
        let ext = entry.extension();
        if ext.is_empty() {
            return "File".into();
        }
        return format!("{} file", ext.to_uppercase());
    }
    let sub = mime.split('/').nth(1).unwrap_or("");
    let sub = match sub {
        "x-executable" => "executable",
        "x-sharedlib" => "library",
        "vnd.rar" => "archive",
        "x-tar" => "archive",
        "x-7z-compressed" => "archive",
        "zip" => "archive",
        "x-gzip" => "archive",
        other => other,
    };
    let mut label = sub.replace(['-', '.'], " ");
    if let Some(c) = label.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    label
}

/// Themed icon name for an entry.
///
/// File types use the icon theme's full-colour MIME icons (the names come from
/// GIO, so they follow whichever theme is active, Colloid included). Folders use
/// the theme's `folder` icon; the pane replaces it with a recoloured variant when
/// the user configured folder colours.
/// Symbolic names are avoided on purpose: they are monochrome and bypass the
/// icon theme's colours.
///
/// Symbolic links are not special-cased here; the pane draws a link emblem next
/// to them so that they still show the icon of their target type.
pub fn icon_name(entry: &FileEntry) -> String {
    if entry.is_dir {
        return crate::icons::FOLDER_ICON.into();
    }
    let icon = gio::content_type_get_icon(&entry.mime);
    if let Some(themed) = icon.downcast_ref::<gio::ThemedIcon>() {
        for name in themed.names() {
            let name = name.to_string();
            if !name.ends_with("-symbolic") && !name.is_empty() {
                return name;
            }
        }
    }
    "text-x-generic".into()
}

/// Split a path into breadcrumb segments: (label, path).
pub fn crumbs(path: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut acc = PathBuf::new();
    for comp in path.components() {
        acc.push(comp.as_os_str());
        let label = match comp {
            std::path::Component::RootDir => "/".to_string(),
            std::path::Component::Normal(s) => s.to_string_lossy().to_string(),
            other => other.as_os_str().to_string_lossy().to_string(),
        };
        out.push((label, acc.clone()));
    }
    out
}

/// A destination filename that does not exist yet: `name (1).ext`.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = p.extension().map(|s| format!(".{}", s.to_string_lossy()));
    for i in 1..10_000 {
        let fname = match &ext {
            Some(ext) => format!("{stem} ({i}){ext}"),
            None => format!("{stem} ({i})"),
        };
        let candidate = dir.join(fname);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!(
        "{stem}-{}{}",
        std::process::id(),
        ext.unwrap_or_default()
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conflict {
    Replace,
    Skip,
    KeepBoth,
}

/// Result of a batch operation.
#[derive(Default, Debug)]
pub struct OpReport {
    pub done: usize,
    pub bytes: u64,
    pub skipped: Vec<PathBuf>,
    pub errors: Vec<(PathBuf, String)>,
}

impl OpReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
    pub fn summary(&self, verb: &str) -> String {
        let mut s = format!(
            "{verb} {} item{}",
            self.done,
            if self.done == 1 { "" } else { "s" }
        );
        if !self.skipped.is_empty() {
            s.push_str(&format!(", {} skipped", self.skipped.len()));
        }
        if !self.errors.is_empty() {
            s.push_str(&format!(", {} failed", self.errors.len()));
        }
        s
    }
}

type ConflictFn<'a> = &'a dyn Fn(&Path) -> Conflict;
/// `(items completed, bytes transferred)` after each source.
type ProgressFn<'a> = &'a dyn Fn(usize, u64);

pub fn copy_paths(
    sources: &[PathBuf],
    dest_dir: &Path,
    on_conflict: ConflictFn,
    progress: ProgressFn,
) -> OpReport {
    let mut report = OpReport::default();
    for src in sources {
        let Some(name) = src.file_name() else {
            continue;
        };
        let mut target = dest_dir.join(name);
        if target.exists() {
            match on_conflict(&target) {
                Conflict::Skip => {
                    report.skipped.push(src.clone());
                    continue;
                }
                Conflict::KeepBoth => {
                    target = unique_path(dest_dir, &name.to_string_lossy());
                }
                Conflict::Replace => {
                    if target.is_dir() {
                        if let Err(err) = fs::remove_dir_all(&target) {
                            report.errors.push((src.clone(), err.to_string()));
                            continue;
                        }
                    } else if let Err(err) = fs::remove_file(&target) {
                        report.errors.push((src.clone(), err.to_string()));
                        continue;
                    }
                }
            }
        }
        match copy_recursive(src, &target) {
            Ok(bytes) => {
                report.done += 1;
                report.bytes += bytes;
                progress(report.done, report.bytes);
            }
            Err(err) => report.errors.push((src.clone(), err.to_string())),
        }
    }
    report
}

fn copy_recursive(src: &Path, dst: &Path) -> io::Result<u64> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        let target = fs::read_link(src)?;
        let _ = fs::remove_file(dst);
        std::os::unix::fs::symlink(target, dst)?;
        return Ok(0);
    }
    if meta.is_dir() {
        fs::create_dir_all(dst)?;
        let mut total = 0;
        for dent in fs::read_dir(src)? {
            let dent = dent?;
            total += copy_recursive(&dent.path(), &dst.join(dent.file_name()))?;
        }
        return Ok(total);
    }
    let mut input = fs::File::open(src)?;
    let mut output = fs::File::create(dst)?;
    let copied = io::copy(&mut input, &mut output)?;
    let _ = fs::set_permissions(dst, meta.permissions());
    Ok(copied)
}

pub fn move_paths(
    sources: &[PathBuf],
    dest_dir: &Path,
    on_conflict: ConflictFn,
    progress: ProgressFn,
) -> OpReport {
    let mut report = OpReport::default();
    for src in sources {
        let Some(name) = src.file_name() else {
            continue;
        };
        let mut target = dest_dir.join(name);
        if src == &target {
            report.skipped.push(src.clone());
            continue;
        }
        if target.exists() {
            match on_conflict(&target) {
                Conflict::Skip => {
                    report.skipped.push(src.clone());
                    continue;
                }
                Conflict::KeepBoth => target = unique_path(dest_dir, &name.to_string_lossy()),
                Conflict::Replace => {
                    let _ = if target.is_dir() {
                        fs::remove_dir_all(&target)
                    } else {
                        fs::remove_file(&target)
                    };
                }
            }
        }
        // Same-filesystem rename is atomic and instant; fall back to copy+delete.
        match fs::rename(src, &target) {
            Ok(()) => {
                report.done += 1;
                progress(report.done, report.bytes);
            }
            Err(_) => match copy_recursive(src, &target) {
                Ok(bytes) => {
                    if let Err(err) = remove_path(src) {
                        report.errors.push((
                            src.clone(),
                            format!("copied but could not remove source: {err}"),
                        ));
                    }
                    report.done += 1;
                    report.bytes += bytes;
                    progress(report.done, report.bytes);
                }
                Err(err) => report.errors.push((src.clone(), err.to_string())),
            },
        }
    }
    report
}

#[derive(Default, Debug)]
pub struct OrganizeReport {
    pub moved: usize,
    pub groups: usize,
    pub skipped: Vec<(PathBuf, String)>,
    pub errors: Vec<(PathBuf, String)>,
}

impl OrganizeReport {
    pub fn summary(&self) -> String {
        let mut text = format!(
            "Moved {} file{} into {} folder{}",
            self.moved,
            if self.moved == 1 { "" } else { "s" },
            self.groups,
            if self.groups == 1 { "" } else { "s" }
        );
        if !self.skipped.is_empty() {
            text.push_str(&format!(", {} skipped", self.skipped.len()));
        }
        if !self.errors.is_empty() {
            text.push_str(&format!(", {} failed", self.errors.len()));
        }
        text
    }
}

/// Folder name used when sorting by extension.
fn organize_group_name(extension: &str) -> String {
    if extension.is_empty() {
        "No extension".to_string()
    } else {
        extension.to_uppercase()
    }
}

/// Direct children of `dir` grouped by extension, ready to be moved.
///
/// Directories and hidden files are left alone; every group is named after the
/// upper-cased extension (`PNG`, `PDF`, …) with a `No extension` group for the
/// rest.
pub fn extension_groups(dir: &Path) -> Vec<(String, Vec<PathBuf>)> {
    let mut groups: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let Ok(read) = fs::read_dir(dir) else {
        return groups;
    };
    for dent in read.flatten() {
        let name = dent.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = dent.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            continue;
        }
        let path = dent.path();
        let extension = path
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default();
        let group = organize_group_name(&extension);
        match groups.iter_mut().find(|(name, _)| name == &group) {
            Some((_, files)) => files.push(path),
            None => groups.push((group, vec![path])),
        }
    }
    groups.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    groups
}

/// Move every file in `dir` into a sub-folder named after its extension.
///
/// Existing destination files are skipped rather than overwritten, so running
/// this twice is safe.
pub fn organize_by_extension(dir: &Path) -> OrganizeReport {
    let mut report = OrganizeReport::default();
    for (group, files) in extension_groups(dir) {
        let target_dir = dir.join(&group);
        let mut created = false;
        if !target_dir.exists() {
            match fs::create_dir(&target_dir) {
                Ok(()) => created = true,
                Err(err) => {
                    for file in files {
                        report
                            .errors
                            .push((file, format!("cannot create {group}: {err}")));
                    }
                    continue;
                }
            }
        }
        let mut moved_here = 0;
        for src in files {
            let Some(name) = src.file_name() else {
                continue;
            };
            let dst = target_dir.join(name);
            if dst.exists() {
                report
                    .skipped
                    .push((src, format!("{} already exists", dst.display())));
                continue;
            }
            match fs::rename(&src, &dst) {
                Ok(()) => {
                    report.moved += 1;
                    moved_here += 1;
                }
                Err(err) => report.errors.push((src, err.to_string())),
            }
        }
        if created && moved_here == 0 {
            let _ = fs::remove_dir(&target_dir);
        } else if moved_here > 0 {
            report.groups += 1;
        }
    }
    report
}

/// Recursively collect directories below `root` (excluding `root` itself).
///
/// Hidden directories, `.git` and symlinked directories are skipped so a scan
/// stays cheap and never leaves the tree; `limit` bounds pathological cases.
pub fn folders_recursive(root: &Path, show_hidden: bool, limit: usize) -> Vec<PathBuf> {
    use std::collections::VecDeque;
    let mut out = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back(root.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        if out.len() >= limit {
            break;
        }
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for dent in read.flatten() {
            let name = dent.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let Ok(file_type) = dent.file_type() else {
                continue;
            };
            // `is_dir` on the symlink_metadata-like DirEntry type is false for
            // symlinks, so linked trees are never followed.
            if !file_type.is_dir() {
                continue;
            }
            if out.len() >= limit {
                break;
            }
            let path = dent.path();
            out.push(path.clone());
            queue.push_back(path);
        }
    }
    out.sort();
    out
}

pub fn remove_path(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

/// Send paths to the freedesktop trash through GIO and return the reports.
pub fn trash_paths(sources: &[PathBuf]) -> OpReport {
    let mut report = OpReport::default();
    for src in sources {
        let file = gio::File::for_path(src);
        match file.trash(gio::Cancellable::NONE) {
            Ok(()) => report.done += 1,
            Err(err) => report.errors.push((src.clone(), err.to_string())),
        }
    }
    report
}

#[derive(Clone, Debug)]
pub struct TrashedItem {
    pub name: String,
    pub info_path: PathBuf,
    pub original: PathBuf,
    pub trashed_at: String,
    pub is_dir: bool,
}

fn trash_home() -> PathBuf {
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("Trash");
    }
    crate::settings::home_dir().join(".local/share/Trash")
}

/// Items currently in the freedesktop trash, newest first.
pub fn list_trash() -> Vec<TrashedItem> {
    let trash = trash_home();
    let info_dir = trash.join("info");
    let files_dir = trash.join("files");
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(&info_dir) else {
        return out;
    };
    for dent in entries.flatten() {
        let info_path = dent.path();
        if info_path.extension() != Some(OsStr::new("trashinfo")) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&info_path) else {
            continue;
        };
        let mut original = PathBuf::new();
        let mut date = String::new();
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("Path=") {
                original = PathBuf::from(percent_decode(v.trim()));
            } else if let Some(v) = line.strip_prefix("DeletionDate=") {
                date = v.trim().to_string();
            }
        }
        if original.as_os_str().is_empty() {
            continue;
        }
        if let Some(name) = info_path.file_stem() {
            let name = name.to_string_lossy().to_string();
            let payload = files_dir.join(&name);
            out.push(TrashedItem {
                is_dir: payload.is_dir(),
                name,
                info_path,
                original,
                trashed_at: date,
            });
        }
    }
    out.sort_by(|a, b| b.trashed_at.cmp(&a.trashed_at));
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Restore a trashed item to its recorded original location.
pub fn restore_trashed(item: &TrashedItem) -> io::Result<PathBuf> {
    let payload = trash_home().join("files").join(&item.name);
    let mut target = item.original.clone();
    if target.exists() {
        let dir = target.parent().unwrap_or(Path::new("/")).to_path_buf();
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| item.name.clone());
        target = unique_path(&dir, &name);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(&payload, &target) {
        Ok(()) => {}
        Err(_) => {
            copy_recursive(&payload, &target)?;
            remove_path(&payload)?;
        }
    }
    let _ = fs::remove_file(&item.info_path);
    Ok(target)
}

/// Permanently delete one trashed item and its `.trashinfo` record.
pub fn purge_trashed(item: &TrashedItem) -> io::Result<()> {
    let payload = trash_home().join("files").join(&item.name);
    if payload.exists() {
        remove_path(&payload)?;
    }
    let _ = fs::remove_file(&item.info_path);
    Ok(())
}

/// Recursively compute the size of a path (used by the properties dialog).
pub fn total_size(path: &Path) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_file() {
        return meta.len();
    }
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for dent in entries.flatten() {
            total += total_size(&dent.path());
        }
    }
    total
}

pub fn count_items(path: &Path) -> u64 {
    fs::read_dir(path)
        .map(|it| it.flatten().count() as u64)
        .unwrap_or(0)
}

/// Permission string such as `drwxr-xr-x`.
pub fn mode_string(mode: u32, is_dir: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_dir { 'd' } else { '-' });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 0b100 != 0 { 'r' } else { '-' });
        s.push(if bits & 0b010 != 0 { 'w' } else { '-' });
        s.push(if bits & 0b001 != 0 { 'x' } else { '-' });
    }
    s
}

/// Extensions Nook can extract with tar, unzip or 7z.
pub fn is_archive(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        ".zip", ".tar", ".tar.gz", ".tgz", ".tar.xz", ".tar.bz2", ".tar.zst", ".7z",
        ".rar", ".gz", ".xz", ".bz2", ".zst", ".iso",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

/// Start a terminal in `dir`, optionally running `command` first.
///
/// Every supported terminal gets its native working-directory flag; the
/// command itself is passed to `sh -c` and followed by an interactive shell so
/// failures stay readable when the window does not close on its own.
pub fn spawn_terminal(
    preferred: &[String],
    dir: &Path,
    command: Option<&str>,
) -> Result<std::process::Child, String> {
    let terminal = terminal_command(preferred);
    let program = terminal.first().cloned().unwrap_or_else(|| "xterm".into());
    let base = Path::new(&program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| program.clone());
    let script = match command {
        Some(command) if !command.trim().is_empty() => format!(
            "{{\n{command}\n}}\nstatus=$?\n\
             printf '\\n[command exited with status %s] press Enter to close…\\n' \"$status\"\n\
             read -r _"
        ),
        _ => "exec \"${SHELL:-sh}\"".to_string(),
    };

    let mut cmd = std::process::Command::new(&program);
    cmd.current_dir(dir);
    match base.as_str() {
        name if name.contains("kitty") => {
            cmd.arg("--directory").arg(dir).arg("sh").arg("-c").arg(&script);
        }
        name if name.contains("foot") => {
            cmd.arg(format!("--working-directory={}", dir.display()))
                .arg("sh")
                .arg("-c")
                .arg(&script);
        }
        name if name.contains("alacritty") => {
            cmd.arg("--working-directory")
                .arg(dir)
                .arg("-e")
                .arg("sh")
                .arg("-c")
                .arg(&script);
        }
        name if name.contains("wezterm") => {
            cmd.arg("start")
                .arg("--cwd")
                .arg(dir)
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg(&script);
        }
        name if name.contains("ghostty") => {
            cmd.arg(format!("--working-directory={}", dir.display()))
                .arg("-e")
                .arg("sh")
                .arg("-c")
                .arg(&script);
        }
        name if name.contains("konsole") => {
            cmd.arg("--workdir")
                .arg(dir)
                .arg("-e")
                .arg("sh")
                .arg("-c")
                .arg(&script);
        }
        name if name.contains("ptyxis") => {
            cmd.arg("--working-directory")
                .arg(dir)
                .arg("--")
                .arg("sh")
                .arg("-c")
                .arg(&script);
        }
        _ => {
            cmd.arg("-e").arg("sh").arg("-c").arg(&script);
        }
    }
    cmd.spawn().map_err(|err| format!("{program}: {err}"))
}

/// The user's configured terminal, falling back to any known terminal on PATH.
pub fn terminal_command(preferred: &[String]) -> Vec<String> {
    let candidates: Vec<String> = preferred
        .iter()
        .cloned()
        .chain(
            [
                "kitty",
                "foot",
                "alacritty",
                "wezterm",
                "ghostty",
                "konsole",
                "ptyxis",
            ]
            .iter()
            .map(|s| s.to_string()),
        )
        .collect();
    for c in candidates {
        if which(&c) {
            return vec![c];
        }
    }
    vec!["xterm".to_string()]
}

pub fn which(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(program).exists())
}

#[cfg(test)]
mod organize_tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nook-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn groups_and_moves_files_by_extension() {
        let dir = temp_dir("organize");
        fs::write(dir.join("a.PNG"), b"x").unwrap();
        fs::write(dir.join("b.png"), b"x").unwrap();
        fs::write(dir.join("notes.txt"), b"x").unwrap();
        fs::write(dir.join("README"), b"x").unwrap();
        fs::write(dir.join(".hidden"), b"x").unwrap();
        fs::create_dir(dir.join("folder")).unwrap();

        let groups = extension_groups(&dir);
        assert_eq!(groups.len(), 3, "groups: {groups:?}");
        assert!(groups
            .iter()
            .any(|(name, files)| name == "PNG" && files.len() == 2));

        let report = organize_by_extension(&dir);
        assert_eq!(report.moved, 4);
        assert_eq!(report.groups, 3);
        assert!(report.errors.is_empty());
        assert!(dir.join("PNG/a.PNG").is_file());
        assert!(dir.join("PNG/b.png").is_file());
        assert!(dir.join("TXT/notes.txt").is_file());
        assert!(dir.join("No extension/README").is_file());
        // Hidden files and existing folders are left where they are.
        assert!(dir.join(".hidden").is_file());
        assert!(dir.join("folder").is_dir());

        // Running again finds no loose files and changes nothing.
        let again = organize_by_extension(&dir);
        assert_eq!(again.moved, 0);
        assert!(again.skipped.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recursive_scan_skips_hidden_git_and_symlinks() {
        let dir = temp_dir("recursive");
        fs::create_dir_all(dir.join("a/b")).unwrap();
        fs::create_dir(dir.join(".hidden")).unwrap();
        fs::create_dir(dir.join(".git")).unwrap();
        std::os::unix::fs::symlink(dir.join("a"), dir.join("link")).unwrap();

        let visible = folders_recursive(&dir, false, 1000);
        assert!(visible.contains(&dir.join("a")));
        assert!(visible.contains(&dir.join("a/b")));
        assert!(!visible.contains(&dir.join(".hidden")));
        assert!(!visible.contains(&dir.join(".git")));
        assert!(!visible.contains(&dir.join("link")));

        let with_hidden = folders_recursive(&dir, true, 1000);
        assert!(with_hidden.contains(&dir.join(".hidden")));
        assert!(!with_hidden.contains(&dir.join(".git")));

        let capped = folders_recursive(&dir, true, 1);
        assert_eq!(capped.len(), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_overwrite_existing_target() {
        let dir = temp_dir("organize-conflict");
        fs::write(dir.join("keep.txt"), b"old").unwrap();
        fs::create_dir(dir.join("TXT")).unwrap();
        fs::write(dir.join("TXT/keep.txt"), b"existing").unwrap();

        let report = organize_by_extension(&dir);
        assert_eq!(report.moved, 0);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(
            fs::read_to_string(dir.join("TXT/keep.txt")).unwrap(),
            "existing"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
