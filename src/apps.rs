//! Desktop integration: MIME associations, `.desktop` entries and launching.
//!
//! Nook never writes `.desktop` files and never edits system-wide MIME
//! databases. Default handlers are resolved through GIO (which already honours
//! `/usr/share/applications/mimeapps.list`, `~/.config/mimeapps.list`,
//! `XDG_CURRENT_DESKTOP`-specific lists and user overrides). The only writes we
//! perform are opt-in user-level changes made by `set_as_default`, plus Nook's
//! own override map, which is purely local to this application.

use gtk::gio;
use gtk::prelude::*;
use std::path::{Path, PathBuf};

/// The `.desktop` id of an application, e.g. `org.gnome.Nautilus.desktop`.
pub fn desktop_id(app: &gio::AppInfo) -> Option<String> {
    app.id().map(|s| s.to_string())
}

/// All applications that can handle `mime`, system association order first.
pub fn apps_for_mime(mime: &str) -> Vec<gio::AppInfo> {
    gio::AppInfo::all_for_type(mime)
}

/// The current system default for a MIME type.
pub fn default_for_mime(mime: &str) -> Option<gio::AppInfo> {
    gio::AppInfo::default_for_type(mime, false)
}

pub fn find_by_desktop_id(id: &str) -> Option<gio::AppInfo> {
    gio::AppInfo::all()
        .into_iter()
        .find(|a| a.id().map(|i| i.as_str() == id).unwrap_or(false))
}

/// Resolve the application Nook should use for a path.
///
/// Order: user override for extension, user override for MIME, GIO default for
/// the MIME type, then the desktop environment's default handler for the URI.
pub fn resolve_app(
    path: &Path,
    mime: &str,
    overrides: &std::collections::HashMap<String, String>,
) -> Option<gio::AppInfo> {
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if let Some(id) = overrides.get(&path.to_string_lossy().to_string()) {
        if let Some(app) = find_by_desktop_id(id) {
            return Some(app);
        }
    }
    if !ext.is_empty() {
        if let Some(id) = overrides.get(&ext) {
            if let Some(app) = find_by_desktop_id(id) {
                return Some(app);
            }
        }
    }
    if let Some(id) = overrides.get(mime) {
        if let Some(app) = find_by_desktop_id(id) {
            return Some(app);
        }
    }
    default_for_mime(mime)
}

/// The custom shell command configured for a path, extension or MIME type.
///
/// Precedence mirrors application overrides: the exact path wins, then its
/// extension, then the MIME type.
pub fn custom_command_for(
    path: &Path,
    mime: &str,
    openers: &std::collections::HashMap<String, String>,
) -> Option<String> {
    let ext = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    if let Some(command) = openers.get(&path.to_string_lossy().to_string()) {
        return Some(command.clone());
    }
    if !ext.is_empty() {
        if let Some(command) = openers.get(&ext) {
            return Some(command.clone());
        }
    }
    openers.get(mime).cloned()
}

/// Quote one path for a POSIX shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Expand a custom opener command for the given paths.
///
/// `%f` (and `{}`) are replaced by the first path; when a command contains no
/// placeholder the quoted paths are appended so `gimp` and `gimp %f` both work.
pub fn expand_opener(command: &str, paths: &[PathBuf]) -> String {
    let first = paths
        .first()
        .map(|p| shell_quote(&p.to_string_lossy()))
        .unwrap_or_default();
    let all = paths
        .iter()
        .map(|p| shell_quote(&p.to_string_lossy()))
        .collect::<Vec<_>>()
        .join(" ");
    let mut out = command.replace("%f", &first).replace("{}", &first);
    if !command.contains("%f") && !command.contains("{}") {
        out.push(' ');
        out.push_str(&all);
    }
    out
}

/// Run a user-configured opener through the shell, detached from Nook.
pub fn launch_command(command: &str, paths: &[PathBuf]) -> Result<(), String> {
    let expanded = expand_opener(command, paths);
    std::process::Command::new("sh")
        .arg("-c")
        .arg(&expanded)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{command}: {e}"))
}

/// Launch one or more files with the resolved/default handler.
pub fn open_paths(
    paths: &[PathBuf],
    mime_of: &dyn Fn(&Path) -> String,
    overrides: &std::collections::HashMap<String, String>,
    custom_openers: &std::collections::HashMap<String, String>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let context = gtk::gdk::Display::default().map(|d| d.app_launch_context());
    let ctx = context
        .as_ref()
        .map(|c| c.upcast_ref::<gio::AppLaunchContext>());

    // Group paths by the application that will handle them so multi-selection
    // opens one window per app instead of one per file.
    let mut groups: Vec<(String, gio::AppInfo, Vec<gio::File>)> = Vec::new();
    let mut commands: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut failures = Vec::new();
    for path in paths {
        let mime = mime_of(path);
        if let Some(command) = custom_command_for(path, &mime, custom_openers) {
            match commands.iter_mut().find(|(found, _)| found == &command) {
                Some((_, found)) => found.push(path.clone()),
                None => commands.push((command, vec![path.clone()])),
            }
            continue;
        }
        let Some(app) = resolve_app(path, &mime, overrides) else {
            failures.push(format!(
                "No application registered for {mime}: {}",
                path.display()
            ));
            continue;
        };
        let id = desktop_id(&app).unwrap_or_else(|| app.name().to_string());
        let file = gio::File::for_path(path);
        match groups.iter_mut().find(|(gid, _, _)| gid == &id) {
            Some((_, _, files)) => files.push(file),
            None => groups.push((id, app, vec![file])),
        }
    }
    let mut launched = 0usize;
    for (command, group) in commands {
        match launch_command(&command, &group) {
            Ok(()) => launched += 1,
            Err(err) => failures.push(err),
        }
    }
    for (_, app, files) in groups {
        match app.launch(&files, ctx) {
            Ok(()) => launched += 1,
            Err(err) => failures.push(format!("{}: {err}", app.name())),
        }
    }
    if !failures.is_empty() {
        if launched == 0 {
            return Err(failures.join("\n"));
        }
        eprintln!("nook: partial open failure: {}", failures.join("; "));
    }
    Ok(())
}

/// Launch with an explicitly chosen application.
pub fn launch_with(app: &gio::AppInfo, paths: &[PathBuf]) -> Result<(), String> {
    let files: Vec<gio::File> = paths.iter().map(|p| gio::File::for_path(p)).collect();
    let context = gtk::gdk::Display::default().map(|d| d.app_launch_context());
    let ctx = context
        .as_ref()
        .map(|c| c.upcast_ref::<gio::AppLaunchContext>());
    app.launch(&files, ctx).map_err(|e| e.to_string())
}

/// Make an application the user-level default for a MIME type.
/// This writes `~/.config/mimeapps.list`-style entries through GIO; it never
/// touches `/usr/share`.
pub fn set_as_default(app: &gio::AppInfo, mime: &str) -> Result<(), String> {
    app.set_as_default_for_type(mime).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn custom_openers_prefer_path_then_extension_then_mime() {
        let mut openers = HashMap::new();
        openers.insert("image/png".to_string(), "mime-tool".to_string());
        openers.insert(".png".to_string(), "ext-tool".to_string());
        openers.insert("/tmp/special.png".to_string(), "path-tool".to_string());

        assert_eq!(
            custom_command_for(Path::new("/tmp/special.png"), "image/png", &openers).as_deref(),
            Some("path-tool")
        );
        assert_eq!(
            custom_command_for(Path::new("/tmp/other.PNG"), "image/png", &openers).as_deref(),
            Some("ext-tool")
        );
        assert_eq!(
            custom_command_for(Path::new("/tmp/other.jpg"), "image/png", &openers).as_deref(),
            Some("mime-tool")
        );
        assert_eq!(
            custom_command_for(Path::new("/tmp/other.jpg"), "image/jpeg", &openers),
            None
        );
    }

    #[test]
    fn opener_placeholders_and_appended_paths_are_quoted() {
        let paths = vec![PathBuf::from("/tmp/a file.txt")];
        assert_eq!(
            expand_opener("editor %f", &paths),
            "editor '/tmp/a file.txt'"
        );
        assert_eq!(
            expand_opener("editor {}", &paths),
            "editor '/tmp/a file.txt'"
        );
        assert_eq!(
            expand_opener("editor", &paths),
            "editor '/tmp/a file.txt'"
        );
        let two = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        assert_eq!(expand_opener("view", &two), "view '/tmp/a' '/tmp/b'");
        assert_eq!(expand_opener("view %f", &two), "view '/tmp/a'");
    }

    #[test]
    fn single_quotes_in_paths_are_escaped() {
        let paths = vec![PathBuf::from("/tmp/it's here")];
        assert_eq!(expand_opener("open %f", &paths), "open '/tmp/it'\\''s here'");
    }
}
