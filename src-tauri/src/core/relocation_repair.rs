//! Re-point agent-side links after the library moves.
//!
//! Every deployment surface — the global agents, the lobster category, and
//! project workspaces — is a symlink (or a Windows junction) pointing into the
//! library. A relocation moves the library out from under all of them, so each
//! link keeps naming the old path and dangles.
//!
//! Relying on the next sync is not enough. The startup sync covers the active
//! preset's global targets only, and project deployments are not re-synced at
//! startup at all, so those would stay broken until the user happened to reopen
//! the project. Repair them at the moment of the move instead.
//!
//! `Copy`-mode deployments need nothing: a copy is self-contained and its
//! freshness is judged by content hash, not by path.

use std::fs;
use std::path::Path;

use anyhow::Result;

use super::{central_repo, file_watcher, skill_store::SkillStore, sync_engine};

/// Depth limit for the scan. A deployment is a direct child of an agent-side
/// root, and a library skill is a couple of levels deep; the limit stops a
/// pathological tree (or a link cycle we failed to spot) from running long.
const MAX_DEPTH: usize = 4;

/// Re-point every link that points into `from` so it points at the matching
/// place under `to`. Returns how many were re-pointed.
///
/// Best-effort per link: one unreadable entry must not abandon the rest.
pub fn repoint_agent_links(store: &SkillStore, from: &Path, to: &Path) -> Result<usize> {
    let mut repointed = 0;
    for root in file_watcher::collect_watch_paths(store) {
        repointed += repoint_links_under(&root, from, to, 0)?;
    }
    Ok(repointed)
}

fn repoint_links_under(dir: &Path, from: &Path, to: &Path, depth: usize) -> Result<usize> {
    if depth > MAX_DEPTH || !dir.is_dir() {
        return Ok(0);
    }
    let mut repointed = 0;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A root we cannot list is not a reason to skip the others.
        Err(_) => return Ok(0),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            match repoint_link(&path, from, to) {
                Ok(true) => repointed += 1,
                Ok(false) => {}
                Err(err) => {
                    log::warn!("relocation: could not re-point {} ({err:#})", path.display());
                }
            }
            // Never descend through a link: it may point outside the tree.
            continue;
        }
        if file_type.is_dir() {
            repointed += repoint_links_under(&path, from, to, depth + 1)?;
        }
    }
    Ok(repointed)
}

/// Point one link at the relocated counterpart of its target, if it points into
/// the old location. Returns whether it changed anything.
fn repoint_link(link: &Path, from: &Path, to: &Path) -> Result<bool> {
    let raw = fs::read_link(link)?;
    let absolute = if raw.is_absolute() {
        raw.clone()
    } else {
        link.parent().unwrap_or_else(|| Path::new("")).join(&raw)
    };
    let Ok(relative) = absolute.strip_prefix(from) else {
        // Points somewhere else entirely — leave it alone.
        return Ok(false);
    };
    let new_target = to.join(relative);
    if new_target == raw {
        return Ok(false);
    }

    // Keep the link's kind. A resolved directory means a directory link; if the
    // link is dangling, go by what the new target actually is.
    let is_dir_link = match fs::metadata(link) {
        Ok(metadata) => metadata.is_dir(),
        Err(_) => new_target.is_dir(),
    };

    sync_engine::remove_link(link)?;
    if is_dir_link {
        central_repo::create_dir_link(&new_target, link)?;
    } else {
        central_repo::create_file_link(&new_target, link)?;
    }
    log::info!(
        "relocation: re-pointed {} -> {}",
        link.display(),
        new_target.display()
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only links pointing into the old library are touched, and the link's kind
    /// is preserved — an agent that could read the skill before must still be
    /// able to read it after.
    #[cfg(unix)]
    #[test]
    fn repoints_only_links_that_point_into_the_old_root() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let agent = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();

        fs::create_dir_all(new.path().join("skills/alpha")).unwrap();
        fs::write(new.path().join("skills/alpha/SKILL.md"), b"a").unwrap();
        fs::write(new.path().join("skills/plain.md"), b"p").unwrap();
        // A directory link that used to point into the library.
        std::os::unix::fs::symlink(old.path().join("skills/alpha"), agent.path().join("alpha"))
            .unwrap();
        // A file link that used to point into the library.
        std::os::unix::fs::symlink(old.path().join("skills/plain.md"), agent.path().join("plain.md"))
            .unwrap();
        // A link to somewhere else entirely: not ours to move.
        std::os::unix::fs::symlink(elsewhere.path().join("other"), agent.path().join("other"))
            .unwrap();

        let repointed = repoint_links_under(agent.path(), old.path(), new.path(), 0).unwrap();

        assert_eq!(repointed, 2, "only the two library links should move");
        assert_eq!(
            fs::read_link(agent.path().join("alpha")).unwrap(),
            new.path().join("skills/alpha")
        );
        assert_eq!(
            fs::read_link(agent.path().join("plain.md")).unwrap(),
            new.path().join("skills/plain.md")
        );
        assert_eq!(
            fs::read_link(agent.path().join("other")).unwrap(),
            elsewhere.path().join("other")
        );
        // Directory link still resolves, file link is still a link.
        assert!(agent.path().join("alpha").join("SKILL.md").exists());
        assert!(fs::symlink_metadata(agent.path().join("plain.md"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(agent.path().join("plain.md").exists());
    }

    /// A link whose old target is already gone (a dangling link) is still
    /// re-pointed, so a move repairs links the user's file manager shows broken.
    #[cfg(unix)]
    #[test]
    fn repoints_a_dangling_link() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let agent = tempfile::tempdir().unwrap();

        fs::create_dir_all(new.path().join("skills/alpha")).unwrap();
        // Target does not exist under `old` — the link dangles.
        std::os::unix::fs::symlink(old.path().join("skills/alpha"), agent.path().join("alpha"))
            .unwrap();
        assert!(!agent.path().join("alpha").exists());

        let repointed = repoint_links_under(agent.path(), old.path(), new.path(), 0).unwrap();

        assert_eq!(repointed, 1);
        assert!(agent.path().join("alpha").is_dir(), "now resolves");
    }

    /// Nothing under the old root means nothing to do.
    #[cfg(unix)]
    #[test]
    fn leaves_a_root_without_library_links_alone() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let agent = tempfile::tempdir().unwrap();
        fs::write(agent.path().join("real.md"), b"x").unwrap();

        assert_eq!(
            repoint_links_under(agent.path(), old.path(), new.path(), 0).unwrap(),
            0
        );
        assert!(agent.path().join("real.md").exists());
    }
}
