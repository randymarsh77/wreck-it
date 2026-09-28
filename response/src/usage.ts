import { z } from "zod";
import { quotaSchema, type Quota } from "./schema";
const window = z
  .object({
    usedPercent: z.number().min(0).max(100),
    resetsAt: z.number().int().positive(),
  })
  .passthrough();
const bucket = z
  .object({
    primary: window.nullable().optional(),
    secondary: window.nullable().optional(),
    rateLimitReachedType: z.string().nullable().optional(),
  })
  .passthrough();
const limits = z
  .object({
    rateLimits: bucket.optional(),
    rateLimitsByLimitId: z.record(bucket).optional(),
  })
  .passthrough();
// Public Codex app-server account/rateLimits/read response. Include every
// reported bucket conservatively; missing limits never imply unlimited access.
export function codexQuota(
  account_id: string,
  raw: unknown,
  now: number,
): Quota {
  const v = limits.parse(raw);
  const buckets = v.rateLimitsByLimitId
    ? Object.values(v.rateLimitsByLimitId)
    : v.rateLimits
      ? [v.rateLimits]
      : [];
  const windows = buckets.flatMap((b) =>
    [b.primary, b.secondary]
      .filter((w) => w != null)
      .map((w) => ({
        remaining_basis_points: b.rateLimitReachedType
          ? 0
          : Math.round((100 - w!.usedPercent) * 100),
        resets_at: w!.resetsAt,
      })),
  );
  return quotaSchema.parse({
    source: "codex_app_server",
    complete:
      buckets.length > 0 && buckets.every((b) => !!b.primary || !!b.secondary),
    snapshot: {
      account_id,
      observed_at: now,
      windows,
      active_sessions: 0,
      blocked_until: null,
      requires_reauthentication: false,
    },
  });
}
export function unavailableQuota(account_id: string, now: number): Quota {
  return {
    source: "unavailable",
    complete: false,
    snapshot: {
      account_id,
      observed_at: now,
      windows: [],
      active_sessions: 0,
      blocked_until: null,
      requires_reauthentication: false,
    },
  };
}
