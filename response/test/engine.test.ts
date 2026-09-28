import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
import {
  Engine,
  deliveryAllowed,
  move,
  type Ports,
  type PullState,
} from "../src/engine";
import {
  emptyState,
  policySchema,
  accountSchema,
  type Signal,
  type RunResult,
  type OwnerState,
  type Start,
} from "../src/schema";
import { signature, verifySignature, redact } from "../src/security";
import { codexQuota } from "../src/usage";
const sha = "a".repeat(40),
  head = "b".repeat(40),
  merge = "c".repeat(40);
const policy = policySchema.parse({
  repo: "org/repo",
  enabled: true,
  base_branch: "main",
  sources: [
    { id: "errors", kind: "error_report", secret_ref: "secret:ERRORS" },
    {
      id: "mail",
      kind: "support_email",
      secret_ref: "secret:MAIL",
      senders: ["customer@example.com"],
    },
    { id: "logs", kind: "deployment_regression", secret_ref: "secret:LOGS" },
  ],
  required_checks: ["test"],
  require_review: true,
  auto_merge: true,
  environment: "production",
  deployment_workflow: "deploy.yml",
  observation_seconds: 60,
  health_max_age_seconds: 30,
  max_attempts: 3,
  session_timeout_seconds: 60,
  verification: [["npm", "test"]],
});
const account = accountSchema.parse({
  id: "codex",
  owner: "alice",
  harness: "codex",
  auth: "api_key",
  credential_ref: "secret:CODEX",
  enabled: true,
  models: [
    { model: "configured", capabilities: ["triage", "repair", "deep_repair"] },
  ],
  max_concurrent_sessions: 1,
});
function signal(
  kind: Signal["kind"] = "error_report",
  delivery = "one",
): Signal {
  return {
    repo: policy.repo,
    source:
      kind === "support_email"
        ? "mail"
        : kind === "deployment_regression"
          ? "logs"
          : "errors",
    kind,
    delivery_id: delivery,
    fingerprint: delivery,
    occurred_at: 100,
    title: "A failure",
    evidence: "secret=sk-test-secret customer@example.com",
    sender: "customer@example.com",
    deployment:
      kind === "deployment_regression"
        ? { id: "old", sha, environment: "production" }
        : undefined,
  };
}
function fixture() {
  let persisted: OwnerState = emptyState("alice"),
    starts = new Set<string>(),
    cancelled = new Set<string>(),
    published = 0,
    merges = 0,
    loseStart = false,
    losePublish = false;
  let result: RunResult = {
    status: "running",
    summary: "",
    tests_passed: false,
    files: [],
  };
  let pr: PullState = {
    number: 1,
    url: "https://github.com/org/repo/pull/1",
    head,
    node_id: "node",
    merged: false,
    closed: false,
    approved: true,
    blocked: false,
    files: ["src/fix.ts"],
    checks: [{ name: "test", sha: head, success: true }],
  };
  let deployment: any;
  const ports: Ports = {
    save: async (s) => {
      persisted = structuredClone(s);
    },
    route: async (accounts, quotas, request, now) => {
      const dir = mkdtempSync(join(tmpdir(), "wreck-route-"));
      try {
        const path = join(dir, "input.json");
        writeFileSync(
          path,
          JSON.stringify({
            accounts,
            usage: quotas.map((q) => q.snapshot),
            request,
            now,
          }),
        );
        return JSON.parse(
          execFileSync(resolve("../target/debug/examples/route"), [path], {
            encoding: "utf8",
          }),
        );
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    },
    head: async () => sha,
    start: async (s: Start) => {
      starts.add(s.attempt_id);
      if (loseStart) {
        loseStart = false;
        throw new Error("lost response");
      }
    },
    status: async () => result,
    cancel: async (_, id) => {
      cancelled.add(id);
    },
    publish: async () => {
      published = 1;
      if (losePublish) {
        losePublish = false;
        throw new Error("lost publish");
      }
      return {
        number: pr.number,
        url: pr.url,
        head: pr.head,
        node_id: pr.node_id,
      };
    },
    pull: async () => pr,
    merge: async () => {
      merges++;
      pr = { ...pr, merged: true, merge_sha: merge };
    },
    deployment: async () => deployment,
  };
  let engine = new Engine(emptyState("alice"), ports);
  return {
    get engine() {
      return engine;
    },
    get state() {
      return engine.state;
    },
    get starts() {
      return starts;
    },
    get cancelled() {
      return cancelled;
    },
    get published() {
      return published;
    },
    get merges() {
      return merges;
    },
    setup: async () => {
      await engine.policy(policy.repo, policy);
      await engine.account(account);
      await engine.quota(
        {
          source: "operator_report",
          complete: true,
          snapshot: {
            account_id: "codex",
            observed_at: 100,
            windows: [{ remaining_basis_points: 9000, resets_at: 10000 }],
            active_sessions: 0,
            blocked_until: null,
            requires_reauthentication: false,
          },
        },
        100,
      );
    },
    restart: () => {
      engine = new Engine(structuredClone(persisted), ports);
    },
    result: (r: Partial<RunResult>) => {
      result = { ...result, ...r };
    },
    pr: (p: Partial<PullState>) => {
      pr = { ...pr, ...p };
    },
    deployment: (d: any) => {
      deployment = d;
    },
    loseStart: () => {
      loseStart = true;
    },
    losePublish: () => {
      losePublish = true;
    },
  };
}
const successful = {
  status: "succeeded" as const,
  tests_passed: true,
  summary: "Fixed",
  session_id: "vendor-id",
  files: [{ path: "src/fix.ts", content: "Zml4", mode: "100644" as const }],
};
for (const kind of [
  "error_report",
  "support_email",
  "deployment_regression",
] as const)
  test(`${kind}: complete signal-to-verified-deployment lifecycle`, async () => {
    const f = fixture();
    await f.setup();
    const i = await f.engine.ingest(signal(kind), 100);
    assert.equal(i.phase, "queued");
    await f.engine.tick(101);
    assert.equal(i.phase, "running");
    f.result(successful);
    await f.engine.tick(102);
    assert.equal(i.phase, "awaiting_checks");
    await f.engine.tick(103);
    assert.equal(i.phase, "merging");
    await f.engine.tick(104);
    assert.equal(i.phase, "deploying");
    assert.notEqual(i.phase, "resolved");
    f.deployment({
      id: "12",
      sha: merge,
      environment: "production",
      url: "https://app.example.com",
      at: 105,
      status: "success",
    });
    await f.engine.tick(105);
    assert.equal(i.phase, "observing");
    for (const t of [106, 126, 146, 166])
      await f.engine.health(
        policy.repo,
        {
          delivery_id: String(t),
          sha: merge,
          environment: "production",
          deployment_id: "12",
          observed_at: t,
          healthy: true,
          evidence: "",
        },
        t,
      );
    assert.equal(i.phase, "resolved");
    assert.equal(f.merges, 1);
    assert.equal(f.starts.size, 1);
    if (kind === "support_email") assert.match(i.draft!, /deployed/);
  });
test("duplicate deliveries and incident fingerprints dispatch once", async () => {
  const f = fixture();
  await f.setup();
  const a = await f.engine.ingest(signal(), 100);
  const b = await f.engine.ingest(signal(), 100);
  assert.equal(a.id, b.id);
  await f.engine.ingest({ ...signal(), delivery_id: "two" }, 101);
  await f.engine.tick(101);
  assert.equal(a.occurrences, 2);
  assert.equal(f.starts.size, 1);
});
test("restart after lost start and publish responses reconciles durable outbox", async () => {
  const f = fixture();
  await f.setup();
  const id = (await f.engine.ingest(signal(), 100)).id;
  f.loseStart();
  await f.engine.tick(101);
  assert.equal(f.state.incidents[id].phase, "starting");
  f.restart();
  await f.engine.tick(102);
  assert.equal(f.starts.size, 1);
  f.result(successful);
  f.losePublish();
  await f.engine.tick(103);
  assert.equal(f.state.incidents[id].phase, "publishing");
  f.restart();
  await f.engine.tick(104);
  assert.equal(f.published, 1);
  assert.equal(f.state.incidents[id].phase, "awaiting_checks");
});
test("owner lease admits one run across repositories and refuses expired lease reuse", async () => {
  const f = fixture();
  await f.setup();
  await f.engine.ingest(signal(), 100);
  const second = await f.engine.ingest(signal("error_report", "two"), 100);
  await f.engine.tick(101);
  assert.equal(second.phase, "deferred");
  assert.equal(f.starts.size, 1);
  await f.engine.tick(300);
  assert.equal(f.cancelled.size, 1);
});
test("unknown quota and wrong capability defer", async () => {
  const f = fixture();
  await f.setup();
  delete f.state.quotas.codex;
  const i = await f.engine.ingest(signal(), 100);
  await f.engine.tick(101);
  assert.equal(i.phase, "deferred");
  assert.equal(f.starts.size, 0);
});
test("auth and quota failures invalidate usage and preserve cooldown", async () => {
  for (const error of ["auth", "quota"] as const) {
    const f = fixture();
    await f.setup();
    await f.engine.ingest(signal(), 100);
    await f.engine.tick(101);
    f.result({ status: "failed", error, retry_after: 600 });
    await f.engine.tick(102);
    const q = f.state.quotas.codex.snapshot;
    assert.equal(q.observed_at, 0);
    if (error === "auth") assert.equal(q.requires_reauthentication, true);
    else assert.equal(q.blocked_until, 702);
  }
});
test("failed/missing/stale checks, changed paths, and missing approval block delivery", () => {
  const base: PullState = {
    number: 1,
    url: "",
    head,
    node_id: "",
    merged: false,
    closed: false,
    approved: true,
    blocked: false,
    files: ["src/x"],
    checks: [{ name: "test", sha: head, success: true }],
  };
  assert.equal(deliveryAllowed(policy, base), true);
  for (const change of [
    { checks: [] },
    { checks: [{ name: "test", sha, success: true }] },
    { checks: [{ name: "test", sha: head, success: false }] },
    { approved: false },
    { files: [".github/workflows/x.yml"] },
    { files: ["../secret"] },
    { blocked: true },
  ])
    assert.equal(deliveryAllowed(policy, { ...base, ...change }), false);
});
test("wrong SHA, environment, and observation gaps never resolve", async () => {
  const f = fixture();
  await f.setup();
  const i = await f.engine.ingest(signal(), 100);
  await f.engine.tick(101);
  f.result(successful);
  await f.engine.tick(102);
  await f.engine.tick(103);
  await f.engine.tick(104);
  f.deployment({
    id: "12",
    sha: merge,
    environment: "production",
    url: "",
    at: 105,
    status: "success",
  });
  await f.engine.tick(105);
  const h = {
    delivery_id: "h",
    sha,
    environment: "production",
    deployment_id: "12",
    observed_at: 106,
    healthy: true,
    evidence: "",
  };
  await f.engine.health(policy.repo, h, 106);
  assert.equal(i.health, undefined);
  await assert.rejects(
    f.engine.health(policy.repo, { ...h, environment: "staging" }, 106),
  );
  await f.engine.health(policy.repo, { ...h, sha: merge }, 106);
  await f.engine.health(
    policy.repo,
    { ...h, sha: merge, observed_at: 150 },
    150,
  );
  assert.equal(i.phase, "observing");
  assert.equal(i.observe_since, 150);
});
test("failed deployment follows up until attempt cap then needs attention", async () => {
  const f = fixture();
  await f.setup();
  const i = await f.engine.ingest(signal(), 100);
  await f.engine.tick(101);
  f.result(successful);
  await f.engine.tick(102);
  await f.engine.tick(103);
  await f.engine.tick(104);
  f.deployment({
    id: "12",
    sha: merge,
    environment: "production",
    url: "",
    at: 105,
    status: "failure",
  });
  await f.engine.tick(105);
  assert.equal(i.phase, "queued");
  assert.equal(i.previous?.session_id, "vendor-id");
  i.attempt = policy.max_attempts;
  await f.engine.tick(106);
  assert.equal(i.phase, "needs_attention");
});
test("cancellation and forbidden transitions", async () => {
  const f = fixture();
  await f.setup();
  const i = await f.engine.ingest(signal(), 100);
  await f.engine.tick(101);
  await f.engine.cancel(i.id, 102);
  assert.equal(i.phase, "cancelled");
  assert.equal(f.cancelled.size, 1);
  assert.throws(() => move(i, "resolved", "bad", 103));
});
test("cross-owner accounts, unsupported Claude auth, unknown credential properties rejected", async () => {
  const f = fixture();
  await assert.rejects(f.engine.account({ ...account, owner: "bob" }));
  await assert.rejects(
    f.engine.account({
      ...account,
      harness: "claude_code",
      auth: "native_subscription",
      credential_ref: "runner:claude",
    }),
  );
  await assert.rejects(f.engine.account({ ...account, token: "secret" }));
});
test("source signatures bind timestamp and exact payload, reject spoof and replay", async () => {
  const raw = '{"message":"hello"}',
    sig = await signature("secret", "100", raw),
    headers = new Headers({
      "x-wreckit-timestamp": "100",
      "x-wreckit-signature": "sha256=" + sig,
    });
  assert.equal(await verifySignature("secret", headers, raw, 100), true);
  assert.equal(await verifySignature("secret", headers, raw + " ", 100), false);
  assert.equal(await verifySignature("secret", headers, raw, 401), false);
  assert.equal(await verifySignature("other", headers, raw, 100), false);
});
test("support spoofing and missing deployment context cannot ingest", async () => {
  const f = fixture();
  await f.setup();
  await assert.rejects(
    f.engine.ingest(
      { ...signal("support_email"), sender: "evil@example.com" },
      100,
    ),
  );
  await assert.rejects(
    f.engine.ingest(
      { ...signal("deployment_regression"), deployment: undefined },
      100,
    ),
  );
});
test("redaction and documented Codex quota normalization", () => {
  assert.equal(redact("token=abc a@example.com"), "token=[REDACTED] [EMAIL]");
  const q = codexQuota(
    "a",
    {
      rateLimits: {
        primary: { usedPercent: 25, resetsAt: 200 },
        secondary: { usedPercent: 100, resetsAt: 400 },
      },
    },
    100,
  );
  assert.equal(q.snapshot.windows[0].remaining_basis_points, 7500);
  assert.equal(q.snapshot.windows[1].remaining_basis_points, 0);
});

test("pre-completion quota reports cannot restore invalidated allowance", async () => {
  const f = fixture();
  await f.setup();
  await f.engine.ingest(signal(), 100);
  await f.engine.tick(101);
  f.result(successful);
  await f.engine.tick(102);
  const old = structuredClone(f.state.quotas.codex);
  delete old.invalidated_at;
  old.snapshot.observed_at = 101;
  await assert.rejects(f.engine.quota(old, 103), /predates/);
});

test("model-specific Codex buckets are included and malformed windows rejected", () => {
  const q = codexQuota(
    "codex",
    {
      rateLimitsByLimitId: {
        general: { primary: { usedPercent: 20, resetsAt: 200 } },
        special: { primary: { usedPercent: 100, resetsAt: 300 } },
      },
    },
    100,
  );
  assert.equal(q.snapshot.windows.length, 2);
  assert.equal(q.snapshot.windows[1].remaining_basis_points, 0);
  assert.throws(() =>
    codexQuota(
      "codex",
      { rateLimits: { primary: { usedPercent: -1, resetsAt: 200 } } },
      100,
    ),
  );
});

test("deployment log regressions correlate to one bounded follow-up, including repeated deliveries", async () => {
  const f = fixture();
  await f.setup();
  const original = await f.engine.ingest(signal(), 100);
  await f.engine.tick(101);
  f.result(successful);
  await f.engine.tick(102);
  await f.engine.tick(103);
  await f.engine.tick(104);
  f.deployment({
    id: "12",
    sha: merge,
    environment: "production",
    url: "",
    at: 105,
    status: "success",
  });
  await f.engine.tick(105);
  const regression = {
    ...signal("deployment_regression", "log-one"),
    deployment: { id: "12", sha: merge, environment: "production" },
  };
  const followup = await f.engine.ingest(regression, 106);
  assert.equal(followup.id, original.id);
  assert.equal(original.phase, "queued");
  const repeated = await f.engine.ingest(
    { ...regression, delivery_id: "log-two", fingerprint: "another-log" },
    107,
  );
  assert.equal(repeated.id, original.id);
  assert.equal(Object.keys(f.state.incidents).length, 1);
  original.attempt = policy.max_attempts;
  await f.engine.tick(108);
  assert.equal(original.phase, "needs_attention");
  const exhausted = await f.engine.ingest(
    { ...regression, delivery_id: "log-three" },
    109,
  );
  assert.equal(exhausted.id, original.id);
  assert.equal(exhausted.phase, "needs_attention");
});
