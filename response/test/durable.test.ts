import test from "node:test";
import assert from "node:assert/strict";
import {
  Miniflare,
  convertV4MiniflareOptions,
  Response as MFResponse,
} from "miniflare";
import { build } from "esbuild";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { execFileSync } from "node:child_process";
import { signature } from "../src/security";
test("real Durable Object: concurrent signed deliveries, persistent restart, atomic owner admission", async () => {
  const dir = mkdtempSync(join(tmpdir(), "wreck-do-"));
  const bundle = await build({
    entryPoints: ["src/worker.ts"],
    bundle: true,
    write: false,
    format: "esm",
    platform: "browser",
    external: ["cloudflare:*", "node:*"],
    target: "es2022",
  });
  const create = () => {
    const options = convertV4MiniflareOptions({
      name: "response-test",
      modules: true,
      script: bundle.outputFiles[0].text,
      compatibilityDate: "2026-09-01",
      compatibilityFlags: ["nodejs_compat"],
      durableObjects: {
        OWNERS: { className: "Owner", useSQLite: true },
        SESSIONS: { className: "Session", useSQLite: true },
      },
      durableObjectsPersist: dir,
      bindings: {
        INTERNAL_TOKEN: "internal-test",
        CREDENTIALS: JSON.stringify({
          "secret:ERRORS": { owner: "alice", value: "hook-test" },
        }),
      },
      serviceBindings: {
        CONTROL: async (req) => {
          const data = (await req.json()) as any;
          if (new URL(req.url).pathname.endsWith("/route")) {
            const file = join(dir, "route.json");
            writeFileSync(file, JSON.stringify(data));
            const result = execFileSync(
              resolve("../target/debug/examples/route"),
              [file],
              { encoding: "utf8" },
            );
            return new MFResponse(result, {
              headers: { "content-type": "application/json" },
            });
          }
          return MFResponse.json({ token: "test-token" });
        },
      },
      outboundService: async () => MFResponse.json({ sha: "a".repeat(40) }),
    });
    return new Miniflare({ ...options, resourcePersistencePath: dir });
  };
  let mf = create();
  const now = Math.floor(Date.now() / 1000);
  const call = async (action: string, body?: unknown) => {
    const r = await mf.dispatchFetch(
      `https://example/${action}?owner=alice&repo=org%2Frepo`,
      {
        method: body ? "POST" : "GET",
        headers: { authorization: "Bearer internal-test" },
        body: body ? JSON.stringify(body) : undefined,
      },
    );
    assert.equal(r.status, 200, await r.clone().text());
    return r.json() as Promise<any>;
  };
  try {
    await call("policy", {
      repo: "org/repo",
      enabled: true,
      base_branch: "main",
      sources: [
        { id: "errors", kind: "error_report", secret_ref: "secret:ERRORS" },
      ],
      required_checks: ["test"],
      require_review: true,
      auto_merge: false,
      environment: "production",
      deployment_workflow: "deploy.yml",
      observation_seconds: 60,
      health_max_age_seconds: 30,
      max_attempts: 3,
      session_timeout_seconds: 60,
      verification: [["npm", "test"]],
    });
    await call("account", {
      id: "codex",
      owner: "alice",
      harness: "codex",
      auth: "api_key",
      credential_ref: "secret:KEY",
      enabled: true,
      models: [{ model: "m", capabilities: ["repair"] }],
      max_concurrent_sessions: 1,
    });
    await call("quota", {
      source: "operator_report",
      complete: true,
      snapshot: {
        account_id: "codex",
        observed_at: now,
        windows: [{ remaining_basis_points: 9000, resets_at: now + 1000 }],
        active_sessions: 0,
        blocked_until: null,
        requires_reauthentication: false,
      },
    });
    const hook = async (delivery: string) => {
      const body = JSON.stringify({
        delivery_id: delivery,
        fingerprint: delivery,
        occurred_at: now,
        title: "failure",
        evidence: "trace",
      });
      const sig = await signature("hook-test", String(now), body);
      const res = await mf.dispatchFetch(
        "https://example/hook?owner=alice&repo=org%2Frepo&source=errors",
        {
          method: "POST",
          headers: {
            authorization: "Bearer internal-test",
            "x-wreckit-timestamp": String(now),
            "x-wreckit-signature": "sha256=" + sig,
          },
          body,
        },
      );
      assert.equal(res.status, 202);
      return res.json() as Promise<any>;
    };
    const [a, b, c] = await Promise.all([
      hook("one"),
      hook("one"),
      hook("two"),
    ]);
    assert.equal(a.id, b.id);
    assert.notEqual(a.id, c.id);
    await Promise.all([call("tick", {}), call("tick", {})]);
    let state = await call("view");
    assert.equal(
      state.incidents.filter((i: any) => i.phase === "running").length,
      1,
      JSON.stringify(state.incidents),
    );
    assert.equal(
      state.incidents.filter((i: any) => i.phase === "deferred").length,
      1,
    );
    await mf.dispose();
    mf = create();
    state = await call("view");
    assert.equal(state.incidents.length, 2);
    assert.equal(
      state.incidents.filter((i: any) => i.phase === "running").length,
      1,
      JSON.stringify(state.incidents),
    );
    const spoof = await mf.dispatchFetch(
      "https://example/view?owner=alice&repo=org%2Frepo",
    );
    assert.equal(spoof.status, 401);
  } finally {
    await mf.dispose();
    rmSync(dir, { recursive: true, force: true });
  }
});
