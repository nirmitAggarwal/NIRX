//! Read-only Git plumbing for the in-app repository dashboard.
//!
//! Everything here shells out to the system `git` binary; Nook never links
//! libgit2 and never writes to a repository except for the explicit actions the
//! dashboard offers (checkout, fetch, pull, push, init). Parsing is kept in
//! small pure functions so it can be unit tested without a repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One line of `git log --graph`: either a commit or a connector row used to
/// keep the graph continuous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRow {
    pub graph: String,
    pub commit: Option<Commit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    pub date: String,
    pub refs: String,
    pub subject: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub current: bool,
    pub upstream: String,
    pub track: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoStatus {
    pub branch: String,
    pub upstream: String,
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicted: u32,
}

impl RepoStatus {
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.modified == 0 && self.untracked == 0 && self.conflicted == 0
    }

    pub fn changes(&self) -> u32 {
        self.staged + self.modified + self.untracked + self.conflicted
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub status: RepoStatus,
    pub branches: Vec<Branch>,
    /// Lane layout of the history, used to draw the commit tree.
    pub tree: GraphModel,
    pub contributions: BTreeMap<String, u32>,
    pub remote: String,
}

/// Run git in `root`, returning stdout or a readable error.
pub fn run(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["-c", "color.ui=false", "--no-pager"])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .map_err(|err| format!("git: {err}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("git exited with {}", output.status)
        } else {
            stderr
        })
    }
}

/// Nearest ancestor (including `start`) that contains a `.git` entry.
pub fn find_repo(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

pub fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string())
}

/// Parse the branch header and porcelain records produced by
/// `git status --porcelain --branch`.
pub fn parse_status(text: &str) -> RepoStatus {
    let mut status = RepoStatus::default();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("## ") {
            parse_branch_header(rest, &mut status);
            continue;
        }
        if line.len() < 2 {
            continue;
        }
        let code = &line[..2];
        if code == "??" {
            status.untracked += 1;
        } else if code.contains('U')
            || code == "AA"
            || code == "DD"
            || code == "AU"
            || code == "UA"
            || code == "DU"
            || code == "UD"
        {
            status.conflicted += 1;
        } else {
            if code.as_bytes()[0] != b' ' && code.as_bytes()[0] != b'?' {
                status.staged += 1;
            }
            if code.as_bytes()[1] != b' ' {
                status.modified += 1;
            }
        }
    }
    status
}

fn parse_branch_header(header: &str, status: &mut RepoStatus) {
    let mut header = header.trim();
    // Detached HEAD reads "HEAD (no branch)".
    if let Some(rest) = header.strip_prefix("No commits yet on ") {
        status.branch = rest.trim().to_string();
        return;
    }
    if header.starts_with("HEAD (no branch)") {
        status.branch = "detached HEAD".into();
        header = header.trim_start_matches("HEAD (no branch)");
    } else if let Some(index) = header.find("...") {
        status.branch = header[..index].trim().to_string();
        let rest = &header[index + 3..];
        let rest = match rest.find(" [") {
            Some(bracket) => {
                status.upstream = rest[..bracket].trim().to_string();
                &rest[bracket + 1..]
            }
            None => {
                status.upstream = rest.trim().to_string();
                ""
            }
        };
        parse_tracking(rest, status);
        return;
    } else {
        status.branch = header.trim().to_string();
        return;
    }
    parse_tracking(header, status);
}

fn parse_tracking(text: &str, status: &mut RepoStatus) {
    let Some(open) = text.find('[') else { return };
    let Some(close) = text[open..].find(']').map(|i| open + i) else {
        return;
    };
    for part in text[open + 1..close].split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("ahead ") {
            status.ahead = value.parse().unwrap_or(0);
        } else if let Some(value) = part.strip_prefix("behind ") {
            status.behind = value.parse().unwrap_or(0);
        }
    }
}

/// Parse `git for-each-ref` rows separated by tabs.
pub fn parse_branches(text: &str) -> Vec<Branch> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let Some(name) = fields.next() else { continue };
        if name.trim().is_empty() {
            continue;
        }
        let head = fields.next().unwrap_or("").trim() == "*";
        let upstream = fields.next().unwrap_or("").trim().to_string();
        let track = fields.next().unwrap_or("").trim().to_string();
        out.push(Branch {
            name: name.to_string(),
            current: head,
            upstream,
            track,
        });
    }
    out.sort_by(|a, b| {
        b.current
            .cmp(&a.current)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

const FIELD: char = '\u{1f}';

/// Parse `git log --graph` where each commit line carries `\u{1f}` separated
/// fields after the graph glyphs.
pub fn parse_graph_log(text: &str) -> Vec<GraphRow> {
    let mut out = Vec::new();
    for line in text.lines() {
        if let Some(pos) = line.find(FIELD) {
            let graph = line[..pos].to_string();
            let rest = &line[pos + FIELD.len_utf8()..];
            let mut fields = rest.split(FIELD);
            let hash = fields.next().unwrap_or("").to_string();
            if hash.is_empty() {
                continue;
            }
            let short = fields.next().unwrap_or("").to_string();
            let parents = fields
                .next()
                .unwrap_or("")
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let author = fields.next().unwrap_or("").to_string();
            let email = fields.next().unwrap_or("").to_string();
            let date = fields.next().unwrap_or("").to_string();
            let refs = fields.next().unwrap_or("").to_string();
            let subject = fields.next().unwrap_or("").to_string();
            out.push(GraphRow {
                graph,
                commit: Some(Commit {
                    hash,
                    short,
                    parents,
                    author,
                    email,
                    date,
                    refs,
                    subject,
                }),
            });
        } else if !line.trim().is_empty() {
            out.push(GraphRow {
                graph: line.to_string(),
                commit: None,
            });
        }
    }
    out
}

/// One drawn row of the commit tree: which lanes run through it, where its dot
/// sits and which curves leave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphRowLayout {
    pub commit: Option<Commit>,
    /// Lane of this row's dot (only meaningful with a commit).
    pub lane: usize,
    /// Lanes drawn as a full vertical line across this row.
    pub active: Vec<usize>,
    /// Lanes that stop on this row: drawn from its top down to the dot.
    pub ending: Vec<usize>,
    /// Lanes opened by this row. The dot's own lane continues from the dot
    /// downwards; the others are reached by a curve instead.
    pub continuing: Vec<usize>,
    /// Curves leaving this row.
    pub links: Vec<GraphLink>,
}

/// A curve from lane `from` on the source row to lane `to` on `target_row`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphLink {
    pub from: usize,
    pub to: usize,
    pub target_row: usize,
    /// The curve ends at the dot of the source row (a merge landing here)
    /// instead of running down to a later row.
    pub into_dot: bool,
}

/// Lane assignment for a whole history: each commit gets a column and the
/// merges between columns become curves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphModel {
    pub lanes: usize,
    pub rows: Vec<GraphRowLayout>,
}

impl GraphModel {
    /// Lay out commits (children first) into lanes.
    pub fn from_commits(commits: Vec<Commit>) -> Self {
        use std::collections::HashMap;

        /// Lane expecting `hash`, if any.
        fn lane_of(lanes: &[Option<String>], hash: &str) -> Option<usize> {
            lanes
                .iter()
                .position(|slot| slot.as_deref() == Some(hash))
        }
        fn free_lane(lanes: &mut Vec<Option<String>>) -> usize {
            if let Some(index) = lanes.iter().position(Option::is_none) {
                return index;
            }
            lanes.push(None);
            lanes.len() - 1
        }

        let mut lanes: Vec<Option<String>> = Vec::new();
        let mut rows: Vec<GraphRowLayout> = Vec::new();
        let mut row_of: HashMap<String, usize> = HashMap::new();
        // (source row, source lane, parent hash) resolved once every row exists.
        let mut pending: Vec<(usize, usize, String)> = Vec::new();

        for commit in commits {
            let expected: Vec<usize> = lanes
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.as_deref() == Some(commit.hash.as_str()))
                .map(|(index, _)| index)
                .collect();
            let lane = match expected.first() {
                Some(&index) => index,
                None => free_lane(&mut lanes),
            };
            let before: Vec<usize> = lanes
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.is_some())
                .map(|(index, _)| index)
                .collect();
            for index in &expected {
                lanes[*index] = None;
            }

            let mut links = Vec::new();
            // Lanes of other branches converge onto this dot.
            for other in expected.iter().filter(|&&index| index != lane) {
                links.push(GraphLink {
                    from: *other,
                    to: lane,
                    target_row: rows.len(),
                    into_dot: true,
                });
            }

            for (position, parent) in commit.parents.iter().enumerate() {
                match lane_of(&lanes, parent) {
                    Some(existing) if existing == lane => {}
                    Some(_) => pending.push((rows.len(), lane, parent.clone())),
                    None => {
                        let target = if position == 0 && lanes[lane].is_none() {
                            lane
                        } else {
                            free_lane(&mut lanes)
                        };
                        lanes[target] = Some(parent.clone());
                        if target != lane {
                            pending.push((rows.len(), lane, parent.clone()));
                        }
                    }
                }
            }

            let after: Vec<usize> = lanes
                .iter()
                .enumerate()
                .filter(|(_, slot)| slot.is_some())
                .map(|(index, _)| index)
                .collect();
            let active: Vec<usize> = before
                .iter()
                .copied()
                .filter(|index| after.contains(index))
                .collect();
            let mut ending: Vec<usize> = before
                .iter()
                .copied()
                .filter(|index| !after.contains(index))
                .collect();
            ending.sort_unstable();
            let mut continuing: Vec<usize> = after
                .iter()
                .copied()
                .filter(|index| !before.contains(index))
                .collect();
            continuing.sort_unstable();
            row_of.insert(commit.hash.clone(), rows.len());
            rows.push(GraphRowLayout {
                commit: Some(commit),
                lane,
                active,
                ending,
                continuing,
                links,
            });
        }

        // Parents may sit many rows further down; resolve those curves now.
        for (source_row, source_lane, parent) in pending {
            let Some(&target_row) = row_of.get(parent.as_str()) else {
                continue;
            };
            let target_lane = rows[target_row].lane;
            // Same lane: the vertical line already connects the two rows.
            if target_lane == source_lane {
                continue;
            }
            rows[source_row].links.push(GraphLink {
                from: source_lane,
                to: target_lane,
                target_row,
                into_dot: false,
            });
        }

        let lanes = lanes.len().max(1);
        GraphModel { lanes, rows }
    }
}

/// Count commits per `YYYY-MM-DD` day.
pub fn parse_contributions(text: &str) -> BTreeMap<String, u32> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let date = line.trim();
        if date.len() == 10 && date.as_bytes()[4] == b'-' {
            *map.entry(date.to_string()).or_insert(0) += 1;
        }
    }
    map
}

/// Everything the dashboard shows, loaded in one background pass.
pub fn snapshot(root: &Path, branch: Option<&str>, limit: usize) -> Result<Snapshot, String> {
    let inside = run(root, &["rev-parse", "--is-inside-work-tree"])?;
    if inside.trim() != "true" {
        return Err("This folder is not a Git working tree.".into());
    }
    let status = parse_status(&run(
        root,
        &["status", "--porcelain", "--branch", "--untracked-files=normal"],
    )?);
    let branches = parse_branches(&run(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)\t%(HEAD)\t%(upstream:short)\t%(upstream:track)",
            "refs/heads",
        ],
    )?);
    let mut log_args = vec![
        "log",
        "--graph",
        "--topo-order",
        "--date=short",
        "--pretty=format:%x1f%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%D%x1f%s",
        "-n",
    ];
    let limit_text = limit.to_string();
    log_args.push(&limit_text);
    match branch {
        Some(name) => {
            log_args.push(name);
        }
        None => log_args.push("--all"),
    }
    let graph = parse_graph_log(&run(root, &log_args)?);
    let tree = GraphModel::from_commits(
        graph
            .iter()
            .filter_map(|row| row.commit.clone())
            .collect(),
    );
    let contributions = parse_contributions(&run(
        root,
        &[
            "log",
            "--all",
            "--date=short",
            "--pretty=format:%ad",
            "--since=1.year.ago",
        ],
    )?);
    let remote = run(root, &["remote", "get-url", "origin"])
        .map(|text| text.trim().to_string())
        .unwrap_or_default();
    Ok(Snapshot {
        status,
        branches,
        tree,
        contributions,
        remote,
    })
}

/// Full metadata and diffstat for one commit.
pub fn commit_details(root: &Path, hash: &str) -> Result<String, String> {
    run(
        root,
        &[
            "show",
            "--stat",
            "--decorate",
            "--date=format:%Y-%m-%d %H:%M",
            "--pretty=format:commit %H%nAuthor: %an <%ae>%nDate:   %ad%nRefs:   %D%n%n    %s%n%n%b",
            hash,
        ],
    )
}

/// Switch to a local branch. `git switch` is preferred, with `git checkout`
/// as a fallback for old Git versions.
pub fn checkout(root: &Path, branch: &str) -> Result<String, String> {
    run(root, &["switch", branch]).or_else(|_| run(root, &["checkout", branch]))
}

pub fn fetch(root: &Path) -> Result<String, String> {
    run(root, &["fetch", "--all", "--prune"])
}

pub fn pull(root: &Path) -> Result<String, String> {
    run(root, &["pull", "--ff-only"])
}

pub fn push(root: &Path) -> Result<String, String> {
    run(root, &["push"])
}

pub fn init(root: &Path) -> Result<String, String> {
    run(root, &["init"])
}

/// Turn a GitHub remote (`git@github.com:user/repo.git` or an https URL) into a
/// browsable web URL.
pub fn github_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    if remote.is_empty() {
        return None;
    }
    let cleaned = remote.trim_end_matches(".git");
    if let Some(rest) = cleaned.strip_prefix("git@github.com:") {
        return Some(format!("https://github.com/{rest}"));
    }
    if cleaned.starts_with("https://github.com/") || cleaned.starts_with("http://github.com/") {
        return Some(cleaned.to_string());
    }
    if let Some(rest) = cleaned.strip_prefix("ssh://git@github.com/") {
        return Some(format!("https://github.com/{rest}"));
    }
    None
}

/// Human label for a short SHA.
pub fn short_hash(hash: &str) -> String {
    hash.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_status_with_tracking() {
        let text = "## main...origin/main [ahead 2, behind 1]\n M src/a.rs\nM  src/b.rs\n?? new.txt\nUU conflict.rs\n";
        let status = parse_status(text);
        assert_eq!(status.branch, "main");
        assert_eq!(status.upstream, "origin/main");
        assert_eq!(status.ahead, 2);
        assert_eq!(status.behind, 1);
        assert_eq!(status.modified, 1);
        assert_eq!(status.staged, 1);
        assert_eq!(status.untracked, 1);
        assert_eq!(status.conflicted, 1);
    }

    #[test]
    fn parses_clean_status_and_detached_head() {
        let status = parse_status("## main...origin/main\n");
        assert_eq!(status.branch, "main");
        assert!(status.is_clean());
        let detached = parse_status("## HEAD (no branch)\n");
        assert_eq!(detached.branch, "detached HEAD");
    }

    #[test]
    fn parses_graph_rows_and_connectors() {
        let text = "* \u{1f}abc123\u{1f}abc\u{1f}def456\u{1f}Ada\u{1f}ada@example.com\u{1f}2026-01-02T10:00:00+00:00\u{1f}HEAD -> main, origin/main\u{1f}Initial import\n|\\\n| * \u{1f}def456\u{1f}def\u{1f}\u{1f}Lin\u{1f}lin@example.com\u{1f}2026-01-01T09:00:00+00:00\u{1f}\u{1f}Second commit\n";
        let rows = parse_graph_log(text);
        assert_eq!(rows.len(), 3);
        let first = rows[0].commit.as_ref().unwrap();
        assert_eq!(first.hash, "abc123");
        assert_eq!(first.parents, vec!["def456"]);
        assert_eq!(first.refs, "HEAD -> main, origin/main");
        assert!(rows[1].commit.is_none());
        assert_eq!(rows[2].commit.as_ref().unwrap().subject, "Second commit");
    }

    #[test]
    fn parses_branches_tabs() {
        let text = "main\t*\torigin/main\t[ahead 1]\nfeature\t \t\t\n";
        let branches = parse_branches(text);
        assert_eq!(branches[0].name, "main");
        assert!(branches[0].current);
        assert_eq!(branches[0].track, "[ahead 1]");
        assert_eq!(branches[1].name, "feature");
        assert!(!branches[1].current);
    }

    #[test]
    fn parses_contributions() {
        let map = parse_contributions("2026-01-01\n2026-01-01\n2026-01-02\n");
        assert_eq!(map.get("2026-01-01"), Some(&2));
        assert_eq!(map.get("2026-01-02"), Some(&1));
    }

    #[test]
    fn lays_out_merge_lanes_and_curves() {
        let commit = |hash: &str, parents: &[&str]| Commit {
            hash: hash.to_string(),
            short: hash.to_string(),
            parents: parents.iter().map(|p| (*p).to_string()).collect(),
            author: "Ada".into(),
            email: "ada@example.com".into(),
            date: "2026-01-01T00:00:00+00:00".into(),
            refs: String::new(),
            subject: format!("commit {hash}"),
        };

        // `a` merges `b` and `c`, both of which sit on top of `d`.
        let model = GraphModel::from_commits(vec![
            commit("a", &["b", "c"]),
            commit("b", &["d"]),
            commit("c", &["d"]),
            commit("d", &[]),
        ]);

        assert_eq!(model.rows.len(), 4);
        assert_eq!(model.lanes, 2);
        assert_eq!(model.rows[0].lane, 0);
        // The second parent of the merge opens lane 1 with a curve down to `c`.
        assert!(model.rows[0]
            .links
            .iter()
            .any(|link| !link.into_dot && link.to == 1 && link.target_row == 2));
        // Lane 1 runs through `b`'s row and stops on `c`'s own row.
        assert_eq!(model.rows[1].active, vec![0, 1]);
        assert_eq!(model.rows[2].ending, vec![1]);
        // `c` curves back into lane 0 where `d` sits.
        assert!(model.rows[2]
            .links
            .iter()
            .any(|link| !link.into_dot && link.to == 0 && link.target_row == 3));
        assert_eq!(model.rows[3].lane, 0);
        // The root commit ends its own lane.
        assert_eq!(model.rows[3].ending, vec![0]);
        assert!(model.rows[3].active.is_empty());
    }

    #[test]
    fn converging_lanes_land_on_the_dot() {
        let commit = |hash: &str, parents: &[&str]| Commit {
            hash: hash.to_string(),
            short: hash.to_string(),
            parents: parents.iter().map(|p| (*p).to_string()).collect(),
            author: "Ada".into(),
            email: "ada@example.com".into(),
            date: "2026-01-01T00:00:00+00:00".into(),
            refs: String::new(),
            subject: format!("commit {hash}"),
        };

        // Two branches point at the same commit; both lanes merge into it.
        let model = GraphModel::from_commits(vec![
            commit("x", &["t"]),
            commit("y", &["t"]),
            commit("t", &[]),
        ]);
        assert_eq!(model.lanes, 2);
        assert_eq!(model.rows[2].lane, 0);
        // The branch tip sits in lane 1 and curves down into lane 0, where the
        // shared parent appears two rows later.
        assert!(model.rows[1].links.iter().any(|link| !link.into_dot
            && link.from == 1
            && link.to == 0
            && link.target_row == 2));
        assert_eq!(model.rows[1].active, vec![0]);
        assert!(model.rows[1].ending.is_empty());
        // Its parent lives in another lane, so the dot's lane does not continue.
        assert!(model.rows[1].continuing.is_empty());
        // The first row opens lane 0, which carries on downwards from its dot.
        assert_eq!(model.rows[0].continuing, vec![0]);
        // The shared parent ends lane 0.
        assert_eq!(model.rows[2].ending, vec![0]);
    }

    #[test]
    fn converts_github_remotes() {
        assert_eq!(
            github_url("git@github.com:acme/nook.git").as_deref(),
            Some("https://github.com/acme/nook")
        );
        assert_eq!(
            github_url("https://github.com/acme/nook.git").as_deref(),
            Some("https://github.com/acme/nook")
        );
        assert_eq!(github_url("git@gitlab.com:acme/nook.git"), None);
    }

    /// Integration check against a real repository with two branches and a
    /// merge: the snapshot must contain both branch commits, the merge commit
    /// (two parents) and more than one graph lane.
    #[test]
    fn snapshot_of_a_real_repository_keeps_branches_and_merges() {
        use std::process::Command;

        let root = std::env::temp_dir().join(format!(
            "nook-git-graph-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .env("GIT_AUTHOR_NAME", "Nook Test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "Nook Test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .output()
                .expect("git is required for this test");
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&output.stderr)
            );
        };

        git(&["init", "-b", "main"]);
        std::fs::write(root.join("base.txt"), "base").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "base"]);
        git(&["checkout", "-b", "feature"]);
        std::fs::write(root.join("feature.txt"), "feature").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "feature work"]);
        git(&["checkout", "main"]);
        std::fs::write(root.join("main.txt"), "main").unwrap();
        git(&["add", "."]);
        git(&["commit", "-m", "main work"]);
        git(&["merge", "--no-ff", "feature", "-m", "merge feature"]);

        let snapshot = snapshot(&root, None, 100).expect("snapshot");
        let commits: Vec<_> = snapshot
            .tree
            .rows
            .iter()
            .filter_map(|row| row.commit.as_ref())
            .collect();
        assert!(commits.len() >= 4, "expected every commit, got {commits:?}");
        assert!(
            commits.iter().any(|commit| commit.parents.len() == 2),
            "the merge commit must be present with two parents"
        );
        assert!(commits.iter().any(|commit| commit.subject == "feature work"));
        assert!(commits.iter().any(|commit| commit.subject == "main work"));
        assert!(snapshot.tree.lanes >= 2, "a merge needs at least two lanes");
        let names: Vec<&str> = snapshot.branches.iter().map(|b| b.name.as_str()).collect();
        assert!(names.contains(&"main"));
        assert!(names.contains(&"feature"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
