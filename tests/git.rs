//! git helpers against throwaway repos (generic identity passed per command).

use std::path::Path;
use std::process::Command;

use claude_deck::git::{self, FileStatus};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn repo_with_commit() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "init"]);
    dir
}

#[test]
fn repo_detection() {
    let repo = repo_with_commit();
    assert!(git::is_repo(repo.path()));
    assert!(git::repo_root(repo.path()).is_some());
}

#[test]
fn diff_reports_modified_and_untracked() {
    let repo = repo_with_commit();
    std::fs::write(repo.path().join("a.txt"), "one\nTWO\nthree\n").unwrap();
    std::fs::write(repo.path().join("new.txt"), "x\ny\n").unwrap();
    let files = git::diff(repo.path()).unwrap();
    let a = files.iter().find(|f| f.path == "a.txt").unwrap();
    assert_eq!(a.status, FileStatus::Modified);
    assert_eq!((a.added, a.removed), (2, 1));
    let n = files.iter().find(|f| f.path == "new.txt").unwrap();
    assert_eq!(n.status, FileStatus::Untracked);
    assert_eq!((n.added, n.removed), (2, 0));
}

#[test]
fn diff_without_commits_uses_empty_tree() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    std::fs::write(dir.path().join("f.txt"), "hi\n").unwrap();
    git(dir.path(), &["add", "f.txt"]);
    let files = git::diff(dir.path()).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, FileStatus::Added);
}

#[test]
fn worktree_pristine_then_modified() {
    let repo = repo_with_commit();
    let root = git::repo_root(repo.path()).unwrap();
    let info = git::create_worktree(&root, "20260101-000000-abcd").unwrap();
    assert_eq!(info.branch, "deck/20260101-000000-abcd");
    assert!(info.path.join("a.txt").exists());
    assert!(git::worktree_is_pristine(&info));
    // The worktree folder is excluded from the main repo's status.
    let exclude = std::fs::read_to_string(root.join(".git").join("info").join("exclude")).unwrap();
    assert!(exclude.contains(".claude/worktrees/"));

    std::fs::write(info.path.join("a.txt"), "changed\n").unwrap();
    assert!(!git::worktree_is_pristine(&info));
}

#[test]
fn worktree_with_new_commit_is_not_pristine() {
    let repo = repo_with_commit();
    let root = git::repo_root(repo.path()).unwrap();
    let info = git::create_worktree(&root, "c1").unwrap();
    std::fs::write(info.path.join("b.txt"), "b\n").unwrap();
    git(&info.path, &["add", "."]);
    git(&info.path, &["commit", "-q", "-m", "more"]);
    assert!(!git::worktree_is_pristine(&info));
}

#[test]
fn worktree_remove_deletes_dir_and_branch() {
    let repo = repo_with_commit();
    let root = git::repo_root(repo.path()).unwrap();
    let info = git::create_worktree_auto(&root).unwrap();
    assert!(info.path.exists());
    git::remove_worktree(&info).unwrap();
    assert!(!info.path.exists());
    let branches = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["branch", "--list", &info.branch])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&branches.stdout).trim().is_empty());
}

#[test]
fn worktree_from_inside_a_worktree_is_a_sibling() {
    let repo = repo_with_commit();
    let root = git::repo_root(repo.path()).unwrap();
    let first = git::create_worktree(&root, "w1").unwrap();
    let inner_root = git::repo_root(&first.path).unwrap();
    let second = git::create_worktree(&inner_root, "w2").unwrap();
    assert!(!second.path.starts_with(&first.path));
    assert_eq!(second.path.parent(), first.path.parent());
    assert_eq!(second.repo_root, first.repo_root);
    assert!(second.path.exists());
}
