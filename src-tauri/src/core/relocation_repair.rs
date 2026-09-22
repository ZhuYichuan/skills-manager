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
    let mut visit = |link: &Path| repoint_link(link, from, to);
    let mut repointed = 0;
    for root in file_watcher::collect_watch_paths(store) {
        repointed += walk_links(&root, 0, &mut visit)?;
    }
    Ok(repointed)
}

/// Re-point links that are already broken, whatever broke them.
///
/// A move performed by a build that predates the repair above left its links
/// dangling with no future move to fix them, so waiting for the next relocation
/// would never help those users. A `Copy`-mode relocation also leaves the old
/// library in place, which means the links still *resolve* — to a stale copy —
/// and only [`repoint_agent_links`] can tell that apart. Run both.
///
/// The test is deliberately narrow: only a **dangling** link whose target names a
/// skill (`.../skills/<name>`) that exists in the current library. A link to the
/// user's own directory is never touched, even if a skill happens to share its
/// name.
pub fn repair_stale_agent_links(store: &SkillStore) -> Result<usize> {
    let skills_root = central_repo::skills_dir();
    let mut visit = |link: &Path| repair_stale_link(link, &skills_root);
    let mut repaired = 0;
    for root in file_watcher::collect_watch_paths(store) {
        repaired += walk_links(&root, 0, &mut visit)?;
    }
    Ok(repaired)
}

/// Visit every link under `dir` (without descending through one), counting the
/// visits that report a change.
fn walk_links(
    dir: &Path,
    depth: usize,
    visit: &mut dyn FnMut(&Path) -> Result<bool>,
) -> Result<usize> {
    if depth > MAX_DEPTH || !dir.is_dir() {
        return Ok(0);
    }
    let Ok(entries) = fs::read_dir(dir) else {
        // A root we cannot list is not a reason to skip the others.
        return Ok(0);
    };
    let mut changed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            match visit(&path) {
                Ok(true) => changed += 1,
                Ok(false) => {}
                Err(err) => log::warn!("relocation: {} ({err:#})", path.display()),
            }
            // Never descend through a link: it may point outside the tree.
            continue;
        }
        if file_type.is_dir() {
            changed += walk_links(&path, depth + 1, visit)?;
        }
    }
    Ok(changed)
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

/// Repair one dangling link by aiming it at the same skill in the current
/// library. Returns whether anything changed.
fn repair_stale_link(link: &Path, skills_root: &Path) -> Result<bool> {
    // A link that resolves is either fine or (after a copy-mode move) points at
    // the stale source; only `repoint_agent_links` can tell those apart.
    if fs::metadata(link).is_ok() {
        return Ok(false);
    }
    let raw = fs::read_link(link)?;
    let absolute = if raw.is_absolute() {
        raw
    } else {
        link.parent()
            .unwrap_or_else(|| Path::new(""))
            .join(&raw)
    };
    let Some(name) = absolute.file_name() else {
        return Ok(false);
    };
    // The old target must have been a skill in a library: `<something>/skills/<name>`.
    // Without this check a dangling link to the user's own directory would be
    // re-aimed at a same-named skill.
    let aimed_at_a_skill = absolute
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|parent| parent == central_repo::SKILLS_DIR_NAME);
    if !aimed_at_a_skill {
        return Ok(false);
    }

    let candidate = skills_root.join(name);
    if !candidate.is_dir() {
        return Ok(false);
    }

    sync_engine::remove_link(link)?;
    central_repo::create_dir_link(&candidate, link)?;
    log::info!(
        "relocation: repaired stale link {} -> {}",
        link.display(),
        candidate.display()
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

        let mut visit = |link: &Path| repoint_link(link, old.path(), new.path());
        let repointed = walk_links(agent.path(), 0, &mut visit).unwrap();

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

        let mut visit = |link: &Path| repoint_link(link, old.path(), new.path());
        let repointed = walk_links(agent.path(), 0, &mut visit).unwrap();

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

        let mut visit = |link: &Path| repoint_link(link, old.path(), new.path());
        assert_eq!(walk_links(agent.path(), 0, &mut visit).unwrap(), 0);
        assert!(agent.path().join("real.md").exists());
    }

    /// A link broken by an *earlier* build's relocation has no future move to fix
    /// it, so the stale-link repair has to be able to.
    #[cfg(unix)]
    #[test]
    fn repairs_a_link_left_dangling_by_an_older_build() {
        let library = tempfile::tempdir().unwrap();
        let gone = tempfile::tempdir().unwrap();
        let agent = tempfile::tempdir().unwrap();

        let skills_root = library.path().join(central_repo::SKILLS_DIR_NAME);
        fs::create_dir_all(skills_root.join("grill-me")).unwrap();
        fs::write(skills_root.join("grill-me/SKILL.md"), b"x").unwrap();

        let link = agent.path().join("grill-me");
        std::os::unix::fs::symlink(gone.path().join("skills/grill-me"), &link).unwrap();
        assert!(!link.exists(), "dangling to start with");

        assert!(repair_stale_link(&link, &skills_root).unwrap());
        assert_eq!(fs::read_link(&link).unwrap(), skills_root.join("grill-me"));
        assert!(link.join("SKILL.md").exists());
    }

    /// A dangling link that never pointed at a library is not ours to re-aim,
    /// even when a skill happens to share its name.
    #[cfg(unix)]
    #[test]
    fn does_not_re_aim_a_link_that_never_pointed_at_a_library() {
        let library = tempfile::tempdir().unwrap();
        let agent = tempfile::tempdir().unwrap();
        let skills_root = library.path().join(central_repo::SKILLS_DIR_NAME);
        fs::create_dir_all(skills_root.join("grill-me")).unwrap();

        let user_dir = tempfile::tempdir().unwrap();
        let link = agent.path().join("grill-me");
        std::os::unix::fs::symlink(user_dir.path().join("grill-me"), &link).unwrap();

        assert!(!repair_stale_link(&link, &skills_root).unwrap());
        assert_eq!(
            fs::read_link(&link).unwrap(),
            user_dir.path().join("grill-me")
        );
    }

    /// A link that still resolves is left alone: after a copy-mode move it points
    /// at the stale source, which only the migration-keyed repair may correct.
    #[cfg(unix)]
    #[test]
    fn leaves_a_resolving_link_alone() {
        let library = tempfile::tempdir().unwrap();
        let agent = tempfile::tempdir().unwrap();
        let skills_root = library.path().join(central_repo::SKILLS_DIR_NAME);
        fs::create_dir_all(skills_root.join("grill-me")).unwrap();

        let stale = tempfile::tempdir().unwrap();
        fs::create_dir_all(stale.path().join("skills/grill-me")).unwrap();
        let link = agent.path().join("grill-me");
        std::os::unix::fs::symlink(stale.path().join("skills/grill-me"), &link).unwrap();

        assert!(!repair_stale_link(&link, &skills_root).unwrap());
    }
}
