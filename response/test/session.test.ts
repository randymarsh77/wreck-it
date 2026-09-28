import test from "node:test";
import assert from "node:assert/strict";
import {
  Miniflare,
  convertV4MiniflareOptions,
  Response as MFResponse,
} from "miniflare";
import { build } from "esbuild";
import { digest } from "../src/security";
test("session DO: idempotent start, SDK-compatible IDs, terminal artifact persistence and cancellation", async () => {
  const source = `import {DurableObject} from 'cloudflare:workers';import {Session} from './src/session';
 export class TestSession extends Session {async reconcile(){return this.alarm()} async expire(){const r=await this.ctx.storage.get('run');r.deadline=0;await this.ctx.storage.put('run',r)}}
 export class FakeSandbox extends DurableObject {
  async configure(){} async setAllowedHosts(){} async mkdir(){} async writeFile(){}
  async getProcess(){return await this.ctx.storage.get('p')??null}
  async startProcess(){await this.ctx.storage.put('p',{status:'running'});await this.ctx.storage.put('starts',(await this.ctx.storage.get('starts')??0)+1)}
  async finish(){await this.ctx.storage.put('p',{status:'completed'})}
  async readFile(){return {content:JSON.stringify({status:'succeeded',session_id:'native-session',summary:'Fixed',tests_passed:true,files:[{path:'x',content:'eA==',mode:'100644'}]})}}
  async failCleanup(){await this.ctx.storage.put('fail',true)} async destroy(){if(await this.ctx.storage.get('fail')){await this.ctx.storage.put('fail',false);throw new Error('transient cleanup')}await this.ctx.storage.put('destroyed',true)}
  async state(){return {starts:await this.ctx.storage.get('starts'),destroyed:await this.ctx.storage.get('destroyed')}}
 }
 export default {fetch(){return new Response('ok')}};`;
  const bundle = await build({
    stdin: { contents: source, resolveDir: process.cwd(), loader: "ts" },
    bundle: true,
    write: false,
    format: "esm",
    platform: "browser",
    external: ["cloudflare:*", "node:*"],
    target: "es2022",
  });
  const mf = new Miniflare(
    convertV4MiniflareOptions({
      name: "session-test",
      modules: true,
      script: bundle.outputFiles[0].text,
      compatibilityDate: "2026-09-01",
      compatibilityFlags: ["nodejs_compat"],
      durableObjects: {
        SESSIONS: { className: "TestSession", useSQLite: true },
        SANDBOX: { className: "FakeSandbox", useSQLite: true },
      },
      bindings: {
        INTERNAL_TOKEN: "test",
        CREDENTIALS: JSON.stringify({
          "secret:KEY": { owner: "alice", value: "provider-key" },
        }),
      },
      serviceBindings: {
        CONTROL: async () => MFResponse.json({ token: "read-token" }),
      },
    }),
  );
  try {
    const ns = await mf.getDurableObjectNamespace("SESSIONS"),
      stub = ns.get(ns.idFromName(await digest("alice:attempt"))) as any;
    const input = {
      owner: "alice",
      repo: "org/repo",
      attempt_id: "attempt",
      base_sha: "a".repeat(40),
      account: {
        id: "a",
        owner: "alice",
        harness: "codex",
        auth: "api_key",
        credential_ref: "secret:KEY",
        enabled: true,
        models: [{ model: "m", capabilities: ["repair"] }],
        max_concurrent_sessions: 1,
      },
      model: "m",
      prompt: "fix",
      verification: [["true"]],
      timeout_seconds: 300,
      capability: "repair",
    };
    const call = (action: string) =>
      stub.fetch("https://session/" + action, {
        method: "POST",
        body: JSON.stringify({ owner: "alice", attempt: "attempt", input }),
      });
    const starts = await Promise.all([call("start"), call("start")]);
    assert.ok(starts.every((r) => r.ok));
    await stub.reconcile();
    await stub.reconcile();
    const sandboxes = await mf.getDurableObjectNamespace("SANDBOX"),
      sandbox = sandboxes.get(
        sandboxes.idFromName((await digest("alice:attempt")).slice(0, 48)),
      ) as any;
    assert.equal((await sandbox.state()).starts, 1);
    assert.equal((await (await call("status")).json()).status, "running");
    await sandbox.finish();
    await sandbox.failCleanup();
    await stub.reconcile();
    assert.equal((await (await call("status")).json()).status, "running");
    await stub.reconcile();
    assert.equal((await (await call("status")).json()).status, "succeeded");
    assert.equal((await sandbox.state()).destroyed, true);
    const next = ns.get(ns.idFromName(await digest("alice:next"))) as any;
    const previous = {
      attempt_id: "attempt",
      account_id: "a",
      session_id: "native-session",
    };
    const follow = (repo: string) =>
      next.fetch("https://session/follow_up", {
        method: "POST",
        body: JSON.stringify({
          owner: "alice",
          attempt: "next",
          input: { ...input, attempt_id: "next", repo, previous },
        }),
      });
    assert.equal((await follow("other/repo")).status, 403);
    assert.equal((await follow("org/repo")).status, 202);
    assert.equal((await call("cancel")).status, 200);
    assert.equal((await (await call("status")).json()).status, "cancelled");
    const timeout = ns.get(ns.idFromName("timeout")) as any;
    const request = (action: string) =>
      timeout.fetch("https://session/" + action, {
        method: "POST",
        body: JSON.stringify({
          owner: "alice",
          attempt: "timeout",
          input: { ...input, attempt_id: "timeout" },
        }),
      });
    await request("start");
    await timeout.reconcile();
    await timeout.expire();
    await timeout.reconcile();
    assert.equal((await (await request("status")).json()).error, "timeout");
  } finally {
    await mf.dispose();
  }
});
