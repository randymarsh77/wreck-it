import { z } from "zod";
export const id = z.string().regex(/^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,99}$/);
export const repository = z
  .string()
  .regex(/^[a-zA-Z0-9_.-]+\/[a-zA-Z0-9_.-]+$/);
export const sha = z.string().regex(/^[a-f0-9]{40}$/);
export const harness = z.enum(["codex", "claude_code", "copilot"]);
export const accountSchema = z
  .object({
    id,
    owner: id,
    harness,
    auth: z.enum(["api_key", "native_subscription", "copilot_token"]),
    credential_ref: z.string().regex(/^(secret|runner):[A-Z_a-z0-9.-]+$/),
    enabled: z.boolean(),
    models: z
      .array(
        z
          .object({
            model: z.string().min(1).max(100),
            capabilities: z
              .array(z.enum(["triage", "repair", "deep_repair"]))
              .min(1),
          })
          .strict(),
      )
      .min(1)
      .max(20),
    max_concurrent_sessions: z.number().int().min(1).max(10),
  })
  .strict()
  .superRefine((a, c) => {
    if (!(
      (a.harness === "codex" &&
        ["api_key", "native_subscription"].includes(a.auth)) ||
      (a.harness === "claude_code" && a.auth === "api_key") ||
      (a.harness === "copilot" && a.auth === "copilot_token")
    ))
      c.addIssue({
        code: "custom",
        message: "Unsupported provider authentication",
      });
    if (a.auth === "native_subscription" && a.max_concurrent_sessions !== 1)
      c.addIssue({
        code: "custom",
        message: "Native account refresh requires a single session lease",
      });
    if (
      (a.auth === "native_subscription") !==
      a.credential_ref.startsWith("runner:")
    )
      c.addIssue({
        code: "custom",
        message: "Native auth requires an owner-managed runner reference",
      });
  });
export type Account = z.infer<typeof accountSchema>;
export const usageSchema = z
  .object({
    account_id: id,
    observed_at: z.number().int().nonnegative(),
    windows: z
      .array(
        z
          .object({
            remaining_basis_points: z.number().int().min(0).max(10000),
            resets_at: z.number().int().positive(),
          })
          .strict(),
      )
      .max(10),
    active_sessions: z.number().int().nonnegative().default(0),
    blocked_until: z.number().int().nonnegative().nullable(),
    requires_reauthentication: z.boolean(),
  })
  .strict();
export type Usage = z.infer<typeof usageSchema>;
export const quotaSchema = z
  .object({
    source: z.enum(["codex_app_server", "operator_report", "unavailable"]),
    complete: z.boolean(),
    snapshot: usageSchema,
  })
  .strict();
export type Quota = z.infer<typeof quotaSchema> & { invalidated_at?: number };
export const sourceSchema = z
  .object({
    id,
    kind: z.enum([
      "error_report",
      "support_email",
      "deployment_regression",
      "health",
      "usage",
    ]),
    secret_ref: z.string().regex(/^secret:[A-Z_a-z0-9]+$/),
    capability: z.enum(["triage", "repair", "deep_repair"]).default("repair"),
    senders: z.array(z.string().email()).max(100).default([]),
  })
  .strict();
export const policySchema = z
  .object({
    repo: repository,
    enabled: z.boolean(),
    base_branch: z
      .string()
      .regex(/^[a-zA-Z0-9][a-zA-Z0-9/_.-]{0,99}$/)
      .refine((s) => !s.includes("..")),
    sources: z.array(sourceSchema).min(1).max(20),
    required_checks: z.array(z.string().min(1).max(100)).min(1).max(30),
    require_review: z.boolean().default(true),
    auto_merge: z.boolean().default(false),
    allowed_hosts: z
      .array(z.string().regex(/^[a-zA-Z0-9.-]+$/))
      .max(100)
      .default([
        "github.com",
        "api.github.com",
        "api.openai.com",
        "chatgpt.com",
        "auth.openai.com",
        "api.anthropic.com",
        "api.githubcopilot.com",
        "copilot-proxy.githubusercontent.com",
        "registry.npmjs.org",
        "index.crates.io",
        "static.crates.io",
      ]),
    blocked_paths: z
      .array(z.string().min(1).max(200))
      .default([".github/", ".wreck-it/"]),
    environment: id,
    deployment_workflow: z.string().regex(/^[a-zA-Z0-9_.-]+\.ya?ml$/),
    observation_seconds: z.number().int().min(60).max(86400),
    health_max_age_seconds: z.number().int().min(30).max(3600),
    max_attempts: z.number().int().min(1).max(5),
    session_timeout_seconds: z.number().int().min(60).max(3600),
    verification: z
      .array(z.array(z.string().min(1).max(500)).min(1).max(30))
      .min(1)
      .max(10),
    minimum_remaining_basis_points: z
      .number()
      .int()
      .min(0)
      .max(10000)
      .default(1000),
    usage_max_age_seconds: z.number().int().min(30).max(3600).default(300),
  })
  .strict()
  .superRefine((p, c) => {
    if (new Set(p.sources.map((s) => s.id)).size !== p.sources.length)
      c.addIssue({ code: "custom", message: "Duplicate source IDs" });
  });
export type Policy = z.infer<typeof policySchema>;
export const signalSchema = z
  .object({
    delivery_id: id,
    fingerprint: z.string().min(1).max(200),
    occurred_at: z.number().int().nonnegative(),
    title: z.string().min(1).max(300),
    evidence: z.string().max(16000),
    reference: z
      .string()
      .url()
      .max(1000)
      .refine((s) => s.startsWith("https://"))
      .optional(),
    deployment: z.object({ id, sha, environment: id }).strict().optional(),
    sender: z.string().email().optional(),
    subject: z.string().max(300).optional(),
  })
  .strict();
export type Signal = z.infer<typeof signalSchema> & {
  kind: z.infer<typeof sourceSchema>["kind"];
  source: string;
  repo: string;
};
export const healthSchema = z
  .object({
    delivery_id: id,
    sha,
    environment: id,
    deployment_id: id,
    observed_at: z.number().int().nonnegative(),
    healthy: z.boolean(),
    evidence: z.string().max(16000).default(""),
  })
  .strict();
export type Health = z.infer<typeof healthSchema>;
export type Selection = {
  account_id: string;
  harness: Account["harness"];
  model: string;
  credential_ref: string;
  remaining_basis_points: number;
  resets_at: number;
};
export type Decision = {
  selection: Selection | null;
  excluded: { account_id: string; reason: string }[];
};
export const resultSchema = z
  .object({
    status: z.enum(["running", "succeeded", "failed", "cancelled"]),
    session_id: z
      .string()
      .regex(/^[a-zA-Z0-9][a-zA-Z0-9_.:-]{0,199}$/)
      .optional(),
    error: z
      .enum(["auth", "quota", "timeout", "execution", "validation"])
      .optional(),
    retry_after: z.number().int().min(1).max(86400).optional(),
    summary: z.string().max(8000).default(""),
    tests_passed: z.boolean().default(false),
    files: z
      .array(
        z
          .object({
            path: z.string().max(300),
            content: z.string().max(150000).nullable(),
            mode: z.enum(["100644", "100755"]),
          })
          .strict(),
      )
      .max(50)
      .default([]),
    assessment: z.enum(["repair", "deep_repair", "no_change"]).optional(),
  })
  .strict()
  .refine(
    (r) => JSON.stringify(r).length <= 48000,
    "Result exceeds durable artifact limit",
  );
export type RunResult = z.infer<typeof resultSchema>;
export type Phase =
  | "queued"
  | "deferred"
  | "starting"
  | "running"
  | "publishing"
  | "awaiting_checks"
  | "merging"
  | "deploying"
  | "observing"
  | "resolved"
  | "needs_attention"
  | "cancelled";
export type Incident = {
  id: string;
  repo: string;
  signal: Signal;
  phase: Phase;
  attempt: number;
  capability: string;
  created_at: number;
  updated_at: number;
  occurrences: number;
  history: { at: number; from: Phase; to: Phase; reason: string }[];
  reason: string;
  start_input?: Start;
  last_heartbeat?: number;
  attempt_id?: string;
  selection?: Selection;
  decision?: Decision;
  lease_until?: number;
  base_sha?: string;
  result?: RunResult;
  pr?: { number: number; url: string; head: string; node_id: string };
  merge_sha?: string;
  deployment?: {
    id: string;
    sha: string;
    environment: string;
    url: string;
    at: number;
  };
  health?: Health;
  observe_since?: number;
  observe_deadline?: number;
  draft?: string;
  outbox?: {
    id: string;
    kind: "start" | "publish" | "merge" | "cancel";
    created_at: number;
  };
  regression_key?: string;
  integration_failures?: number;
  previous?: { attempt_id: string; session_id?: string; account_id: string };
};
export type OwnerState = {
  owner: string;
  accounts: Account[];
  quotas: Record<string, Quota>;
  policies: Record<string, Policy>;
  tick_cursor?: number;
  incidents: Record<string, Incident>;
  deliveries: Record<string, { incident: string; at: number }>;
};
export const emptyState = (owner: string): OwnerState => ({
  owner,
  accounts: [],
  quotas: {},
  policies: {},
  incidents: {},
  deliveries: {},
});
export interface Start {
  owner: string;
  repo: string;
  attempt_id: string;
  base_sha: string;
  account: Account;
  model: string;
  prompt: string;
  verification: string[][];
  timeout_seconds: number;
  allowed_hosts: string[];
  previous?: Incident["previous"];
  capability: string;
}
