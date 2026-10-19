import type { ManagedSkill } from "./tauri";

export interface SkillSourceInfo {
  /** Grouping unit. Two skills share a key only when they come from the same upstream. */
  key: string;
  /** What the user sees as the group title. */
  label: string;
  kind: SkillSourceKind;
  /** The raw field the key was derived from, kept for tooltips and debugging. */
  raw: string;
}

export type SkillSourceKind = "repo" | "site" | "path" | "unknown";

export interface SkillSourceGroup extends SkillSourceInfo {
  count: number;
  skills: ManagedSkill[];
}

const UNKNOWN_LABEL = "";

const stripSlashes = (value: string) => value.replace(/^\/+/, "").replace(/\/+$/, "");

/**
 * Turn any way of naming a git repo into one key.
 *
 * SSH, HTTPS and scp forms all name the same repo, and so do a trailing `.git`
 * and different letter case — git treats all of them as the same remote. Only
 * the path is case-folded: hosts that serve case-sensitive paths still differ
 * by host, and folding the whole string would merge `Host` with `host`.
 */
function parseGitRef(ref: string, raw: string): SkillSourceInfo | null {
  const trimmed = ref.trim();
  if (!trimmed) return null;

  let host = "";
  let path = "";
  const scp = trimmed.match(/^[^@/\s]+@([^:/\s]+):(.+)$/); // git@github.com:owner/repo
  const url = trimmed.match(
    /^(?:https?|ssh|git):\/\/(?:[^@/\s]+@)?([^:/\s]+)(?::\d+)?\/(.+)$/
  );
  const shorthand = trimmed.match(/^([\w.-]+)\/([\w.-]+?)(?:\.git)?$/); // owner/repo

  if (scp) {
    host = scp[1];
    path = scp[2];
  } else if (url) {
    host = url[1];
    path = url[2];
  } else if (shorthand) {
    host = "github.com";
    path = `${shorthand[1]}/${shorthand[2]}`;
  } else {
    return null;
  }

  path = stripSlashes(path).replace(/\.git$/i, "");
  if (!path) return null;
  // Branch, subpath and revision never reach this point: callers pass only the
  // repo reference, so a monorepo's skills stay in one group.
  path = path.toLowerCase();
  host = host.toLowerCase();

  const label = host === "github.com" ? path : `${host}/${path}`;
  return { key: `repo:${host}/${path}`, label, kind: "repo", raw };
}

/**
 * skills.sh site skills are snapshots published by one site. The site is the
 * only grouping available — the skills are not a repo, so they group by domain.
 * `source_ref` holds the canonical `https://skills.sh/site/<domain>/<skill>`.
 */
function parseSiteRef(ref: string, raw: string): SkillSourceInfo | null {
  const trimmed = ref.trim();
  if (!trimmed) return null;
  const segments = trimmed.replace(/[?#].*$/, "").split("/").filter(Boolean);
  const site = segments.indexOf("site");
  const domain = site >= 0 ? segments[site + 1] : undefined;
  if (domain) {
    const lower = domain.toLowerCase();
    return { key: `site:${lower}`, label: lower, kind: "site", raw };
  }
  try {
    const host = new URL(trimmed).host.toLowerCase();
    return host ? { key: `site:${host}`, label: host, kind: "site", raw } : null;
  } catch {
    return null;
  }
}

/**
 * Local and imported skills are sourced from a directory, and the path is the
 * source. Paths are not case-folded: on a case-sensitive filesystem two paths
 * differing only by case are two different sources.
 */
function parseLocalRef(ref: string, raw: string): SkillSourceInfo | null {
  const trimmed = ref.trim().replace(/\\/g, "/").replace(/\/+$/, "");
  if (!trimmed) return null;
  const label = trimmed.split("/").filter(Boolean).pop() || trimmed;
  return { key: `path:${trimmed}`, label, kind: "path", raw };
}

/**
 * A skill whose `source_ref` is its own central copy has no external source:
 * adoption re-points it there once the original directory became a deployment
 * (see scenario_service::detach_source_refs_from_adoption_target). Grouping it
 * by that path would give every such skill a group of its own.
 */
function isDetached(skill: ManagedSkill) {
  return !!skill.source_ref && skill.source_ref === skill.central_path;
}

export function getSkillSourceInfo(skill: ManagedSkill): SkillSourceInfo {
  const ref = skill.source_ref?.trim() || "";
  const resolved = skill.source_ref_resolved?.trim() || "";

  if (skill.source_type === "git") {
    const info = parseGitRef(resolved || ref, ref || resolved);
    if (info) return info;
  }

  if (skill.source_type === "skillssh") {
    // `source_ref` is `owner/repo/<skill>`, so the repo URL has to win; the
    // reference is only a fallback when the resolved URL is missing.
    const info =
      (resolved && parseGitRef(resolved, ref || resolved)) ||
      (ref && parseGitRef(ref.split("/").slice(0, -1).join("/"), ref));
    if (info) return info;
  }

  if (skill.source_type === "well-known") {
    const info = parseSiteRef(ref, ref) || parseSiteRef(resolved, ref || resolved);
    if (info) return info;
  }

  if (skill.source_type === "local" || skill.source_type === "import") {
    if (!isDetached(skill)) {
      const info = parseLocalRef(ref, ref);
      if (info) return info;
    }
  }

  if (ref || resolved) {
    const info = parseGitRef(resolved || ref, ref || resolved);
    if (info) return info;
  }

  return {
    key: "unknown",
    label: UNKNOWN_LABEL,
    kind: "unknown",
    raw: ref || resolved,
  };
}

/**
 * Groups skills by their upstream source.
 *
 * Input order is preserved inside a group, so grouping never reorders the list
 * the filters and sorting already produced. Groups are ordered large to small,
 * then by label; the unknown group is always last because it is a leftover
 * bucket rather than a source.
 */
export function buildSourceGroups(skills: ManagedSkill[]): SkillSourceGroup[] {
  const groups = new Map<string, SkillSourceGroup>();
  for (const skill of skills) {
    const info = getSkillSourceInfo(skill);
    const existing = groups.get(info.key);
    if (existing) {
      existing.count += 1;
      existing.skills.push(skill);
    } else {
      groups.set(info.key, { ...info, count: 1, skills: [skill] });
    }
  }

  return Array.from(groups.values()).sort((a, b) => {
    if (a.kind === "unknown" || b.kind === "unknown") {
      return a.kind === b.kind ? 0 : a.kind === "unknown" ? 1 : -1;
    }
    return (
      b.count - a.count ||
      (a.label || a.key).localeCompare(b.label || b.key, undefined, {
        sensitivity: "base",
      })
    );
  });
}
