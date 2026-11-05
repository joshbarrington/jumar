//! Deleting a directory takes as long as it has files, but renaming one is instant. A
//! worktree's ignored directories, such as `node_modules`, hold most of its files, so they
//! are renamed out of the way before the removal and deleted in the background after it.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::iter;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::git;
use crate::model::Worktree;

/// Inside the git dir: on the repo's volume, and invisible to `git status` and to source globs.
const TRASH: &str = "jumar-trash";
/// Where each directory came from. Written before the first rename and deleted once the
/// removal succeeds, so its presence means the directories may still be wanted.
const MANIFEST: &str = "manifest";
/// Marks directories that could not go back, which the sweep must never delete.
const KEPT: &str = "kept";

/// Trash younger than this may belong to a removal still in progress.
const LEFTOVER_AGE: Duration = Duration::from_hours(1);

/// Read-only directories, such as Go's module cache, stop `rm` until they are made writable.
const DELETE: &str = r#"rm -rf -- "$1" 2>/dev/null || { chmod -R u+w -- "$1"; rm -rf -- "$1"; }"#;

#[derive(Debug)]
pub struct Stash {
    dir: PathBuf,
    checkout: PathBuf,
    /// The original path of each directory; the one at index `i` is now at `dir/i`, unless
    /// it could not move.
    paths: Vec<PathBuf>,
}

impl Stash {
    /// Moves `worktree`'s ignored directories to the trash. `None` when nothing moved. This
    /// is only a speedup: a directory that cannot move, such as one on another volume, stays
    /// for the removal to delete.
    #[must_use]
    pub fn ignored_dirs(worktree: &Worktree) -> Option<Self> {
        let dirs = git::ignored_dirs(&worktree.path).ok()?;
        if dirs.is_empty() {
            return None;
        }
        let git_dir = git::common_dir(&worktree.repo).ok()?;
        let stash = Self {
            dir: git_dir.join(TRASH).join(unique_name()),
            checkout: worktree.path.clone(),
            paths: dirs
                .iter()
                .map(|dir| worktree.path.join(dir.trim_end_matches('/')))
                .collect(),
        };
        fs::create_dir_all(&stash.dir).ok()?;
        let mut moved = false;
        if stash.write_manifest().is_ok() {
            for (index, from) in stash.paths.iter().enumerate() {
                moved |= fs::rename(from, stash.dir.join(index.to_string())).is_ok();
            }
        }
        if !moved {
            let _ = fs::remove_dir_all(&stash.dir);
            return None;
        }
        Some(stash)
    }

    /// Puts the directories back after a removal that failed. When the checkout went anyway,
    /// there is nothing to put them back into, so they are deleted as the removal intended.
    ///
    /// # Errors
    ///
    /// Returns a note saying where the directories that could not go back were kept.
    pub fn restore(self) -> Result<(), String> {
        if !self.checkout.exists() {
            self.delete_in_background();
            return Ok(());
        }
        let mut stuck = 0;
        for (index, to) in self.paths.iter().enumerate() {
            let from = self.dir.join(index.to_string());
            if from.exists() && put_back(&from, to).is_err() {
                stuck += 1;
            }
        }
        if stuck == 0 {
            let _ = fs::remove_dir_all(&self.dir);
            return Ok(());
        }
        let _ = fs::write(self.dir.join(KEPT), "");
        Err(format!(
            "could not put back {stuck} ignored directories; they are in {}",
            self.dir.display()
        ))
    }

    pub fn delete_in_background(self) {
        let _ = fs::remove_file(self.dir.join(MANIFEST));
        delete_detached(&self.dir);
    }

    fn write_manifest(&self) -> io::Result<()> {
        let mut bytes = Vec::new();
        for path in iter::once(&self.checkout).chain(&self.paths) {
            bytes.extend_from_slice(path.as_os_str().as_bytes());
            bytes.push(0);
        }
        let mut file = File::create(self.dir.join(MANIFEST))?;
        file.write_all(&bytes)?;
        file.sync_all()
    }

    fn read(dir: &Path) -> Option<Self> {
        let bytes = fs::read(dir.join(MANIFEST)).ok()?;
        let mut paths = bytes
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| PathBuf::from(OsStr::from_bytes(path)));
        let checkout = paths.next()?;
        Some(Self {
            dir: dir.to_path_buf(),
            checkout,
            paths: paths.collect(),
        })
    }
}

/// Settles old stashes in `repo`, left behind when jumar or a delete was interrupted. One
/// with a manifest never finished its removal, so its directories go back if they can.
pub fn sweep(repo: &Path) {
    let Ok(git_dir) = git::common_dir(repo) else {
        return;
    };
    let Ok(entries) = fs::read_dir(git_dir.join(TRASH)) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !is_old(&entry) || dir.join(KEPT).exists() {
            continue;
        }
        match Stash::read(&dir) {
            Some(stash) => drop(stash.restore()),
            None => delete_detached(&dir),
        }
    }
}

fn is_old(entry: &fs::DirEntry) -> bool {
    entry
        .metadata()
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| {
            modified
                .elapsed()
                .is_ok_and(|elapsed| elapsed >= LEFTOVER_AGE)
        })
}

/// A partly finished removal may have taken the parent, such as `web/` of `web/node_modules`.
fn put_back(from: &Path, to: &Path) -> io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(from, to)
}

fn unique_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{}-{nanos}", process::id())
}

/// Its own process group, so the delete outlives jumar and the popup jumar may be running in.
fn delete_detached(path: &Path) {
    let child = Command::new("sh")
        .args(["-c", DELETE, "sh"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Ok(mut child) = child {
        thread::spawn(move || child.wait());
    }
}
