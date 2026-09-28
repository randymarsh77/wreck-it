import test from "node:test";
import assert from "node:assert/strict";
import { GitHub } from "../src/github";
import { policySchema, type Incident } from "../src/schema";
const sha = "a".repeat(40),
  head = "b".repeat(40);
const policy = policySchema.parse({
  repo: "org/repo",
  enabled: true,
  base_branch: "main",
  sources: [{ id: "errors", kind: "error_report", secret_ref: "secret:ERROR" }],
  required_checks: ["test"],
  environment: "production",
  deployment_workflow: "deploy.yml",
  observation_seconds: 60,
  health_max_age_seconds: 30,
  max_attempts: 2,
  session_timeout_seconds: 60,
  verification: [["true"]],
});
const control = {
  fetch: async () => Response.json({ token: "installation-token" }),
};
test("GitHub publication recovers a lost create-PR response without a second PR or commit", async () => {
  let pr: any,
    ref: any,
    commits = 0,
    creates = 0;
  const github = new GitHub(control, "internal", async (input, init) => {
    const u = new URL(String(input)),
      path = u.pathname.replace("/repos/org/repo", "");
    if (path === "/pulls" && init?.method === "GET")
      return Response.json(pr ? [pr] : []);
    if (path === `/git/commits/${sha}`)
      return Response.json({ tree: { sha: "base-tree" } });
    if (path === "/git/blobs") return Response.json({ sha: "blob" });
    if (path === "/git/trees") return Response.json({ sha: "tree" });
    if (path === "/git/commits") {
      commits++;
      return Response.json({ sha: head });
    }
    if (path.startsWith("/git/ref/heads/"))
      return ref ? Response.json(ref) : new Response("", { status: 404 });
    if (path === "/git/refs") {
      ref = { object: { sha: head } };
      return Response.json(ref);
    }
    if (path === "/pulls" && init?.method === "POST") {
      creates++;
      pr = {
        number: 1,
        html_url: "https://github.com/org/repo/pull/1",
        head: { sha: head },
        node_id: "node",
      };
      throw new Error("Lost response after server committed PR");
    }
    throw new Error("Unexpected API operation");
  });
  const incident = {
    id: "incident",
    attempt_id: "attempt",
    created_at: 100,
    signal: { title: "Repair" },
    result: {
      summary: "Fixed",
      files: [{ path: "src/fix.ts", content: "Zml4", mode: "100644" }],
    },
  } as Incident;
  await assert.rejects(github.publish("org/repo", sha, incident, policy));
  const recovered = await github.publish("org/repo", sha, incident, policy);
  assert.equal(recovered.number, 1);
  assert.equal(creates, 1);
  assert.equal(commits, 1);
});
test("merge request atomically binds the checked head and fails when GitHub refuses it", async () => {
  const github = new GitHub(control, "internal", async (_, init) => {
    const body = JSON.parse(String(init?.body));
    assert.equal(body.sha, head);
    assert.equal(body.merge_method, "squash");
    return Response.json({ merged: false });
  });
  await assert.rejects(
    github.merge("org/repo", { number: 1, head } as any),
    /not accepted/,
  );
});
test("deployment requires exact workflow run identity, revision, and environment", async () => {
  let log = "https://github.com/org/repo/actions/runs/123";
  const github = new GitHub(control, "internal", async (input) => {
    const url = String(input);
    if (url.includes("/actions/workflows/"))
      return Response.json({
        workflow_runs: [
          { id: 12, head_sha: sha, status: "completed", conclusion: "success" },
        ],
      });
    if (url.includes("/statuses"))
      return Response.json([
        { state: "success", log_url: log, created_at: "2026-01-01T00:00:00Z" },
      ]);
    return Response.json([{ id: 4, sha, environment: "production" }]);
  });
  assert.equal(await github.deployment("org/repo", sha, policy), undefined);
  log = "https://evil.example/org/repo/actions/runs/12";
  assert.equal(await github.deployment("org/repo", sha, policy), undefined);
  log = "https://github.com/org/repo/actions/runs/12";
  assert.equal(
    (await github.deployment("org/repo", sha, policy))?.status,
    "success",
  );
});
