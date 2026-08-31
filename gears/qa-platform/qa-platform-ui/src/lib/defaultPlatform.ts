import { PlatformInfo } from '@/api/types';

/**
 * Resolve what the dialogs' **"Default cluster"** option should launch against.
 *
 * # Why this exists, and why it is not legacy's behaviour
 *
 * Legacy's "Default cluster" meant *no platform at all*: `manager/src/routes/runs.rs`
 * resolves the platform context to `(None, None, None, None)` when no platform is named,
 * and `manager/src/services/argo.rs` only mounts a kubeconfig `if let Some(platform_name)`.
 * The runner therefore fell through to its own in-cluster ServiceAccount and tested the
 * cluster it was running inside — which worked because legacy's manager was deployed
 * *into* the VHP cluster (`README.md`: manager in `vhp-tests`, VHP in `vhp-platform`).
 *
 * qa-platform is deployed standalone, beside the clusters it tests rather than inside
 * one, so "the cluster I am running in" holds no product and legacy's meaning is dead
 * here. **Decision (user, 2026-08-31): "Default cluster" resolves to the product's
 * platform instead.** That is deliberately a behaviour change from legacy, and it is the
 * only reading that does something useful on this topology.
 *
 * # Two tiers: the explicit flag, then the sole platform
 *
 * 1. **`is_default`** — an operator marked one platform as the product's default. This
 *    is the answer whenever it exists, and it is the only thing that can disambiguate a
 *    product with several platforms.
 * 2. **The product's only platform** — with exactly one candidate there is nothing to
 *    disambiguate, so requiring a flag would make the common single-platform setup fail
 *    for no reason.
 *
 * With several platforms and **no** flag set this returns `null` rather than guessing.
 * Picking the first would silently run a suite against an environment nobody chose,
 * which is the quiet wrong-target failure the flag exists to prevent; the caller
 * refuses with a message naming the real problem instead.
 *
 * If more than one platform somehow carries the flag — the service clears the previous
 * holder, but that clear and the promotion are two statements with no transaction
 * around them — this still returns a single platform rather than `null`, because a UI
 * that refuses to launch is a worse outcome than one that picks the first of two rows
 * that should not both exist. The service-side rule is where that invariant is kept.
 *
 * A platform with no `product_id` is never a candidate: it is not attached to the
 * product whose plan is being launched.
 */
export function defaultPlatformForProduct(
  platforms: PlatformInfo[],
  productId: string | null | undefined
): PlatformInfo | null {
  if (!productId) return null;
  const owned = platforms.filter((platform) => platform.product_id === productId);
  const flagged = owned.find((platform) => platform.is_default);
  if (flagged) return flagged;
  return owned.length === 1 ? owned[0] : null;
}

/**
 * The "Default cluster" option's label, naming what it will actually run against.
 *
 * An option that silently resolves to something is an option the reader cannot audit,
 * and this one resolves to a whole test environment. When the target is known the label
 * says so; when it is not, the bare label is left alone rather than made to promise a
 * resolution that will fail on submit.
 */
export function defaultPlatformLabel(resolved: PlatformInfo | null): string {
  return resolved ? `Default cluster (${resolved.name})` : 'Default cluster';
}
