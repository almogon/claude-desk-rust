//! Thin wrapper around the `git` CLI (no libgit2): working-tree diff for the
//! diff panel and per-conversation worktrees. Every function blocks on a child
//! process, so call them from a worker thread.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Hash of git's empty tree, used as diff base when the repo has no commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
/// Lines kept per file; the rest is dropped (counters still count them).
pub const MAX_LINES_PER_FILE: usize = 3000;
/// Untracked files bigger than this are listed as binary, without content.
const MAX_UNTRACKED_BYTES: u64 = 512 * 1024;
/// Safety cap on listed untracked files (a missing `.gitignore` can list thousands).
const MAX_UNTRACKED_FILES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// The whole `@@ -a,b +c,d @@ ...` line.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub status: FileStatus,
    pub added: usize,
    pub removed: usize,
    /// Binary (or too big) file: no hunks.
    pub binary: bool,
    /// More than [`MAX_LINES_PER_FILE`] lines: the hunks were cut.
    pub truncated: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    fn new(path: String, status: FileStatus) -> FileDiff {
        FileDiff {
            path,
            status,
            added: 0,
            removed: 0,
            binary: false,
            truncated: false,
            hunks: Vec::new(),
        }
    }
}

/// A git worktree created for one conversation.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub repo_root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    /// HEAD sha the worktree started from.
    pub base: String,
}

fn git_cmd(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null());
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}

/// Runs git in `dir`; returns stdout, or the stderr text on failure.
fn run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = git_cmd(dir)
        .args(args)
        .output()
        .map_err(|e| format!("No se pudo ejecutar git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(if err.is_empty() {
            format!("git {} failed", args.first().copied().unwrap_or(""))
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// True if `dir` is inside a git work tree.
pub fn is_repo(dir: &Path) -> bool {
    run(dir, &["rev-parse", "--is-inside-work-tree"]).is_ok_and(|s| s.trim() == "true")
}

/// Top level of the work tree containing `dir`.
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let out = run(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    let t = out.trim();
    (!t.is_empty()).then(|| PathBuf::from(t))
}

// ---------------------------------------------------------------------------
// Diff
// ---------------------------------------------------------------------------

/// Working tree (tracked + untracked) against HEAD, for the repo containing `cwd`.
pub fn diff(cwd: &Path) -> Result<Vec<FileDiff>, String> {
    let root = repo_root(cwd).ok_or_else(|| "The folder is not a git repository".to_owned())?;
    let base = if run(&root, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok() {
        "HEAD"
    } else {
        EMPTY_TREE
    };
    let text = run(
        &root,
        &["diff", "--no-color", "--no-ext-diff", "-M", base, "--"],
    )?;
    let mut files = parse_unified_diff(&text);

    let others = run(&root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    for rel in others
        .split('\0')
        .filter(|p| !p.is_empty())
        .take(MAX_UNTRACKED_FILES)
    {
        files.push(untracked_file(&root, rel));
    }
    Ok(files)
}

fn untracked_file(root: &Path, rel: &str) -> FileDiff {
    let mut f = FileDiff::new(rel.to_owned(), FileStatus::Untracked);
    let path = root.join(rel);
    let big = std::fs::metadata(&path).map(|m| m.len() > MAX_UNTRACKED_BYTES);
    let bytes = match big {
        Ok(false) => std::fs::read(&path).ok(),
        _ => None,
    };
    let Some(bytes) = bytes.filter(|b| !b.contains(&0)) else {
        f.binary = true;
        return f;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = Vec::new();
    let mut n = 0usize;
    for l in text.lines() {
        n += 1;
        if lines.len() < MAX_LINES_PER_FILE {
            lines.push(DiffLine {
                kind: LineKind::Add,
                text: l.to_owned(),
            });
        } else {
            f.truncated = true;
        }
    }
    f.added = n;
    if n > 0 {
        f.hunks.push(Hunk {
            header: format!("@@ -0,0 +1,{n} @@"),
            lines,
        });
    }
    f
}

/// Path from the `a/P b/P` part of a `diff --git` header (best effort; the
/// `rename to` line overrides it for renames).
fn header_path(rest: &str) -> String {
    let rest = rest.trim();
    // Same name on both sides: "a/P b/P" has odd length 2*(|P|+2)+1.
    if rest.len() % 2 == 1 {
        let mid = (rest.len() - 1) / 2;
        if let (Some(a), Some(b)) = (rest.get(..mid), rest.get(mid + 1..))
            && a.starts_with("a/")
            && b.starts_with("b/")
            && a[2..] == b[2..]
        {
            return a[2..].to_owned();
        }
    }
    let p = rest.rsplit_once(" b/").map_or(rest, |(_, b)| b);
    p.trim_matches('"').to_owned()
}

/// Parses `git diff` unified output. Tolerant: unknown lines are ignored.
pub fn parse_unified_diff(text: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut cur: Option<FileDiff> = None;
    let mut stored = 0usize;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.extend(cur.take());
            cur = Some(FileDiff::new(header_path(rest), FileStatus::Modified));
            stored = 0;
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if line.starts_with("@@") {
            f.hunks.push(Hunk {
                header: line.to_owned(),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(h) = f.hunks.last_mut() {
            let (kind, body) = match line.chars().next() {
                Some('+') => (LineKind::Add, &line[1..]),
                Some('-') => (LineKind::Del, &line[1..]),
                Some(' ') => (LineKind::Context, &line[1..]),
                None => (LineKind::Context, ""),
                _ => continue, // "\ No newline at end of file"
            };
            match kind {
                LineKind::Add => f.added += 1,
                LineKind::Del => f.removed += 1,
                LineKind::Context => {}
            }
            if stored < MAX_LINES_PER_FILE {
                stored += 1;
                h.lines.push(DiffLine {
                    kind,
                    text: body.to_owned(),
                });
            } else {
                f.truncated = true;
            }
        } else if line.starts_with("new file mode") {
            f.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            f.status = FileStatus::Deleted;
        } else if let Some(to) = line.strip_prefix("rename to ") {
            f.status = FileStatus::Renamed;
            f.path = to.to_owned();
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            f.binary = true;
        }
    }
    files.extend(cur);
    files
}

// ---------------------------------------------------------------------------
// Worktrees
// ---------------------------------------------------------------------------

/// Root of the MAIN repository: the parent of the common git dir when it is a
/// `.git` folder, otherwise `dir` itself (bare repos, errors).
fn main_repo_root(dir: &Path) -> PathBuf {
    // No `--path-format=absolute`: git < 2.31 silently echoes it as output.
    // The path may be relative to `dir`, resolved below.
    let common = run(dir, &["rev-parse", "--git-common-dir"]);
    let Ok(common) = common else {
        return dir.to_path_buf();
    };
    let mut p = PathBuf::from(common.trim());
    if p.is_relative() {
        p = dir.join(p);
    }
    match (p.file_name().is_some_and(|n| n == ".git"), p.parent()) {
        (true, Some(parent)) => parent.to_path_buf(),
        _ => dir.to_path_buf(),
    }
}

/// Makes sure `.claude/worktrees/` is in the repo's `info/exclude`.
fn ensure_excluded(repo_root: &Path) -> Result<(), String> {
    const ENTRY: &str = ".claude/worktrees/";
    let common = run(repo_root, &["rev-parse", "--git-common-dir"])?;
    let mut dir = PathBuf::from(common.trim());
    if dir.is_relative() {
        dir = repo_root.join(dir);
    }
    let file = dir.join("info").join("exclude");
    let existing = std::fs::read_to_string(&file).unwrap_or_default();
    if existing
        .lines()
        .any(|l| matches!(l.trim(), ENTRY | "/.claude/worktrees/"))
    {
        return Ok(());
    }
    let mut new = existing;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(ENTRY);
    new.push('\n');
    std::fs::create_dir_all(dir.join("info")).map_err(|e| e.to_string())?;
    std::fs::write(&file, new).map_err(|e| e.to_string())
}

fn new_slug() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        &hex[..4]
    )
}

/// Creates `<repo_root>/.claude/worktrees/<slug>` on a new branch `deck/<slug>`
/// from HEAD.
pub fn create_worktree(repo_root: &Path, slug: &str) -> Result<WorktreeInfo, String> {
    let fail = |e: String| format!("Could not create the worktree: {e}");
    // Always hang worktrees off the main repo, even when started from inside one.
    let repo_root = &main_repo_root(repo_root);
    let base = run(repo_root, &["rev-parse", "HEAD"])
        .map_err(|_| fail("the repository has no commits".into()))?
        .trim()
        .to_owned();
    ensure_excluded(repo_root).map_err(fail)?;
    let path = repo_root.join(".claude").join("worktrees").join(slug);
    let branch = format!("deck/{slug}");
    let path_s = path.to_string_lossy().into_owned();
    run(
        repo_root,
        &["worktree", "add", "-b", &branch, &path_s, "HEAD"],
    )
    .map_err(fail)?;
    Ok(WorktreeInfo {
        repo_root: repo_root.to_path_buf(),
        path,
        branch,
        base,
    })
}

/// Same as [`create_worktree`] with a generated slug
/// (`<yyyymmdd-hhmmss>-<4 hex>`).
pub fn create_worktree_auto(repo_root: &Path) -> Result<WorktreeInfo, String> {
    create_worktree(repo_root, &new_slug())
}

/// True if the worktree has no uncommitted changes and no commits beyond its
/// base. Conservative: any git error counts as "not pristine".
pub fn worktree_is_pristine(info: &WorktreeInfo) -> bool {
    let clean = run(&info.path, &["status", "--porcelain"]).is_ok_and(|s| s.trim().is_empty());
    clean
        && run(
            &info.path,
            &["rev-list", &format!("{}..HEAD", info.base), "--count"],
        )
        .is_ok_and(|s| s.trim() == "0")
}

/// Removes the worktree directory and deletes its branch. Safe to call again
/// after a partial failure.
pub fn remove_worktree(info: &WorktreeInfo) -> Result<(), String> {
    if info.path.exists() {
        let path_s = info.path.to_string_lossy().into_owned();
        run(&info.repo_root, &["worktree", "remove", &path_s])?;
    } else {
        let _ = run(&info.repo_root, &["worktree", "prune"]);
    }
    run(&info.repo_root, &["branch", "-D", &info.branch]).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODIFIED: &str = "\
diff --git a/src/a b.rs b/src/a b.rs
index 111..222 100644
--- a/src/a b.rs
+++ b/src/a b.rs
@@ -1,3 +1,3 @@ fn main() {
 keep
-old
+new
 tail
\\ No newline at end of file
@@ -10 +10,2 @@
 x
+y
";

    #[test]
    fn parses_modified_file_with_spaces() {
        let f = parse_unified_diff(MODIFIED);
        assert_eq!(f.len(), 1);
        let f = &f[0];
        assert_eq!(f.path, "src/a b.rs");
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!((f.added, f.removed), (2, 1));
        assert_eq!(f.hunks.len(), 2);
        assert_eq!(f.hunks[0].header, "@@ -1,3 +1,3 @@ fn main() {");
        assert_eq!(f.hunks[0].lines.len(), 4);
        assert_eq!(f.hunks[0].lines[1].kind, LineKind::Del);
        assert_eq!(f.hunks[0].lines[2].text, "new");
    }

    #[test]
    fn parses_added_deleted_renamed_binary() {
        let text = "\
diff --git a/n.txt b/n.txt
new file mode 100644
index 0000000..111
--- /dev/null
+++ b/n.txt
@@ -0,0 +1 @@
+hi
diff --git a/d.txt b/d.txt
deleted file mode 100644
index 111..0000000
--- a/d.txt
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old.txt b/new.txt
similarity index 90%
rename from old.txt
rename to new.txt
index 111..222 100644
--- a/old.txt
+++ b/new.txt
@@ -1 +1 @@
-a
+b
diff --git a/img.png b/img.png
index 111..222 100644
Binary files a/img.png and b/img.png differ
";
        let f = parse_unified_diff(text);
        assert_eq!(f.len(), 4);
        assert_eq!((f[0].status, f[0].added), (FileStatus::Added, 1));
        assert_eq!((f[1].status, f[1].removed), (FileStatus::Deleted, 1));
        assert_eq!(
            (f[2].status, f[2].path.as_str()),
            (FileStatus::Renamed, "new.txt")
        );
        assert!(f[3].binary && f[3].hunks.is_empty());
        assert_eq!(f[3].path, "img.png");
    }

    #[test]
    fn caps_lines_but_keeps_counters() {
        let mut text = String::from("diff --git a/big b/big\n@@ -0,0 +1,5000 @@\n");
        for i in 0..5000 {
            text.push_str(&format!("+line {i}\n"));
        }
        let f = parse_unified_diff(&text);
        assert_eq!(f[0].added, 5000);
        assert_eq!(f[0].hunks[0].lines.len(), MAX_LINES_PER_FILE);
        assert!(f[0].truncated);
    }

    #[test]
    fn empty_input_gives_no_files() {
        assert!(parse_unified_diff("").is_empty());
    }
}
