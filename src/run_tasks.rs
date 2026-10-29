//! Directory-defined Run commands.
//!
//! Nook reads commands from `fm_run_task.json`. Lookup walks up from the folder
//! being browsed to the nearest file, so one file at a project root serves the
//! whole tree; if there is none, a file in the current folder is used when the
//! user saves. The document is validated before it is written, and an invalid
//! existing document is reported instead of being overwritten, so a typo can
//! never silently discard somebody's tasks.
//!
//! ```json
//! {
//!   "version": 1,
//!   "commands": [
//!     { "name": "Dev server", "command": "npm run dev" }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::settings::RunCommand;

pub const FILE_NAME: &str = "fm_run_task.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct TaskDocument {
    #[serde(default = "schema_version")]
    version: u32,
    commands: Vec<RunCommand>,
}

fn schema_version() -> u32 {
    1
}

/// Where the commands for `dir` come from, and the file that saving updates.
#[derive(Clone, Debug)]
pub struct TaskSource {
    pub path: PathBuf,
    pub commands: Vec<RunCommand>,
    /// The file exists and was parsed successfully.
    pub exists: bool,
}

/// Nearest `fm_run_task.json` at or above `dir`.
pub fn locate(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|ancestor| ancestor.join(FILE_NAME))
        .find(|candidate| candidate.is_file())
}

/// Read the commands that apply to `dir`.
///
/// Returns an error (with the file and the parse failure) when the document is
/// malformed; callers must show it rather than falling back silently.
pub fn load_for(dir: &Path) -> Result<TaskSource, String> {
    match locate(dir) {
        Some(path) => {
            let text = std::fs::read_to_string(&path)
                .map_err(|err| format!("Cannot read {}: {err}", path.display()))?;
            let document: TaskDocument = serde_json::from_str(&text)
                .map_err(|err| format!("{} is not valid JSON: {err}", path.display()))?;
            Ok(TaskSource {
                path,
                commands: document.commands,
                exists: true,
            })
        }
        // Saving creates this file, so the editor always knows its target.
        None => Ok(TaskSource {
            path: dir.join(FILE_NAME),
            commands: Vec::new(),
            exists: false,
        }),
    }
}

/// Validate and atomically write `commands` to `path`.
pub fn save(path: &Path, commands: &[RunCommand]) -> Result<(), String> {
    let cleaned: Vec<RunCommand> = commands
        .iter()
        .filter_map(|command| {
            let text = command.command.trim();
            if text.is_empty() {
                return None;
            }
            let name = command.name.trim();
            Some(RunCommand {
                name: if name.is_empty() {
                    text.to_string()
                } else {
                    name.to_string()
                },
                command: text.to_string(),
            })
        })
        .collect();

    let document = TaskDocument {
        version: schema_version(),
        commands: cleaned,
    };
    let text = serde_json::to_string_pretty(&document)
        .map_err(|err| format!("Could not serialise run tasks: {err}"))?;
    // Round-trip first: never write a document Nook could not load again.
    serde_json::from_str::<TaskDocument>(&text)
        .map_err(|err| format!("Refusing to write an invalid task file: {err}"))?;

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(directory)
        .map_err(|err| format!("Cannot create {}: {err}", directory.display()))?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, format!("{text}\n"))
        .map_err(|err| format!("Cannot write {}: {err}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|err| {
        let _ = std::fs::remove_file(&temp);
        format!("Cannot save {}: {err}", path.display())
    })
}

/// Commands that can destroy data if the user did not mean to run them.
pub fn is_destructive(command: &str) -> bool {
    let lower = command.to_lowercase();
    [
        "rm ",
        "rm -",
        "rmdir",
        "sudo",
        "doas ",
        "dd ",
        "mkfs",
        "shred",
        "truncate",
        "shutdown",
        "reboot",
        "poweroff",
        "git clean",
        "git reset --hard",
        "git checkout --",
        "npm publish",
        "cargo publish",
        "chmod 777",
        "> /dev/",
        ":(){",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nook-run-tasks-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_returns_target_in_folder() {
        let dir = temp_dir("missing");
        let source = load_for(&dir).unwrap();
        assert!(!source.exists);
        assert_eq!(source.path, dir.join(FILE_NAME));
        assert!(source.commands.is_empty());
    }

    #[test]
    fn saves_and_loads_round_trip() {
        let dir = temp_dir("roundtrip");
        let path = dir.join(FILE_NAME);
        save(
            &path,
            &[RunCommand {
                name: "Test".into(),
                command: "cargo test".into(),
            }],
        )
        .unwrap();
        let source = load_for(&dir).unwrap();
        assert!(source.exists);
        assert_eq!(source.commands.len(), 1);
        assert_eq!(source.commands[0].command, "cargo test");
    }

    #[test]
    fn lookup_walks_up_to_the_project_root() {
        let root = temp_dir("nested");
        let nested = root.join("a/b");
        std::fs::create_dir_all(&nested).unwrap();
        save(
            &root.join(FILE_NAME),
            &[RunCommand {
                name: "Build".into(),
                command: "make".into(),
            }],
        )
        .unwrap();
        assert_eq!(locate(&nested), Some(root.join(FILE_NAME)));
    }

    #[test]
    fn malformed_json_is_reported_not_ignored() {
        let dir = temp_dir("malformed");
        std::fs::write(dir.join(FILE_NAME), "{ not json").unwrap();
        assert!(load_for(&dir).is_err());
    }

    #[test]
    fn empty_commands_are_dropped_on_save() {
        let dir = temp_dir("empty");
        let path = dir.join(FILE_NAME);
        save(
            &path,
            &[RunCommand {
                name: "Blank".into(),
                command: "   ".into(),
            }],
        )
        .unwrap();
        assert!(load_for(&dir).unwrap().commands.is_empty());
    }

    #[test]
    fn destructive_commands_are_flagged() {
        assert!(is_destructive("rm -rf build"));
        assert!(is_destructive("sudo systemctl restart nginx"));
        assert!(!is_destructive("npm run dev"));
    }
}
