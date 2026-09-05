import { type ClassValue, clsx } from "clsx"
import { twMerge } from "tailwind-merge"

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

const PLAN_NAME_NOISE_SUFFIXES = [
  ' e2e tests',
  ' e2e test',
  ' integration tests',
  ' unit tests',
  ' tests',
  ' test',
  ' e2e',
] as const;

/**
 * Strip common noise suffixes (" E2E Tests", " Tests", etc.) from a plan name
 * for compact display. The full name should still be available in a tooltip.
 *
 * Case-insensitive on the suffix; iterates so that e.g. "Foo E2E Tests" first
 * loses " Tests" then " E2E". Returns the trimmed name; falls back to the
 * original when the result would be empty.
 */
/**
 * Render an environment option label as `name (version.build)`. Falls back to just
 * the version when no build is detected, or just the name when no version.
 * The build is appended only when the version string doesn't already include
 * it (users sometimes pre-bake the full `version.build` string).
 */
export function formatEnvironmentLabel(environment: {
  name?: string | null;
  version?: string | null;
  build?: string | null;
}): string {
  const name = environment?.name?.trim();
  if (!name) return '';
  const version = environment?.version?.trim();
  if (!version) return name;
  const build = environment?.build?.trim();
  const full = build && !version.endsWith(`.${build}`) ? `${version}.${build}` : version;
  return `${name} (${full})`;
}

export function displayPlanName(name: string): string {
  if (!name) return name;
  let current = name.trim();
  let changed = true;
  while (changed) {
    changed = false;
    const lower = current.toLowerCase();
    for (const suffix of PLAN_NAME_NOISE_SUFFIXES) {
      if (lower.endsWith(suffix)) {
        const next = current.slice(0, current.length - suffix.length).trimEnd();
        if (next.length > 0) {
          current = next;
          changed = true;
          break;
        }
      }
    }
  }
  return current;
}

const COMMON_DEFAULT_BRANCH_NAMES = ['main', 'master'];

/**
 * Picks a branch to preselect from a list of candidates when no repo-specific
 * or environment default branch is available — e.g. a multi-repo custom plan or
 * a test whose specific repo is unknown, where `branchList` is the union of
 * branches across more than one repository and there's no single
 * `default_branch` to fall back to. Prefers a conventional `main`/`master`
 * branch if one of those is present in the list; otherwise falls back to the
 * first (alphabetically sorted) entry, purely so the field isn't left blank.
 */
export function pickDefaultBranch(branchList: string[]): string {
  for (const name of COMMON_DEFAULT_BRANCH_NAMES) {
    if (branchList.includes(name)) return name;
  }
  return branchList[0] ?? '';
}
