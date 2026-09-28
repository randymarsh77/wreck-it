import { DurableObject } from "cloudflare:workers";
import { Engine } from "./engine";
import { GitHub, control } from "./github";
import {
  emptyState,
  type OwnerState,
  type Start,
  signalSchema,
  healthSchema,
  id,
  repository,
  type Decision,
} from "./schema";
import {
  boundedBody,
  digest,
  verifySignature,
  terminal,
  redact,
} from "./security";
import { codexQuota } from "./usage";
import { Sandbox as BaseSandbox } from "@cloudflare/sandbox";
export { ContainerProxy } from "@cloudflare/sandbox";
export class Sandbox extends BaseSandbox {
  enableInternet = false;
}
export { Session } from "./session";
export interface Env {
  OWNERS: DurableObjectNamespace;
  SESSIONS: DurableObjectNamespace;
  SANDBOX: DurableObjectNamespace;
  CONTROL: Fetcher;
  INTERNAL_TOKEN: string;
  CREDENTIALS: string;
  NATIVE_RUNNER?: Fetcher;
  NATIVE_RUNNER_URL?: string;
}
export async function sessionCall(
  env: Env,
  owner: string,
  attempt: string,
  action: string,
  body?: unknown,
) {
  const stub = env.SESSIONS.get(
    env.SESSIONS.idFromName(await digest(owner + ":" + attempt)),
  );
  const res = await stub.fetch(`https://session/${action}`, {
    method: "POST",
    body: JSON.stringify({ owner, attempt, ...((body as object) ?? {}) }),
  });
  if (!res.ok) throw new Error("Session operation failed");
  return res.json();
}
export class Owner extends DurableObject<Env> {
  private queue: Promise<unknown> = Promise.resolve();
  private serial<T>(fn: () => Promise<T>): Promise<T> {
    const next = this.queue.then(fn, fn);
    this.queue = next.catch(() => {});
    return next;
  }
  private async engine(owner?: string) {
    const rows = await this.ctx.storage.list<unknown>();
    const saved = rows.has("owner")
      ? {
          ...emptyState(String(rows.get("owner"))),
          tick_cursor: Number(rows.get("cursor") ?? 0),
        }
      : undefined;
    if (saved)
      for (const [key, value] of rows) {
        if (key.startsWith("account:"))
          saved.accounts.push(structuredClone(value) as any);
        else if (key.startsWith("quota:"))
          saved.quotas[key.slice(6)] = structuredClone(value) as any;
        else if (key.startsWith("policy:"))
          saved.policies[key.slice(7)] = structuredClone(value) as any;
        else if (key.startsWith("incident:"))
          saved.incidents[key.slice(9)] = structuredClone(value) as any;
        else if (key.startsWith("receipt:"))
          saved.deliveries[key.slice(8)] = structuredClone(value) as any;
      }
    if (saved && owner && saved.owner !== owner)
      throw new Error("Owner mismatch");
    const state = saved ?? emptyState(owner!);
    if (!state.owner) throw new Error("Owner missing");
    const gh = new GitHub(this.env.CONTROL, this.env.INTERNAL_TOKEN);
    return new Engine(state, {
      save: async (s) => {
        const entries: Record<string, unknown> = {
          owner: s.owner,
          cursor: s.tick_cursor ?? 0,
        };
        for (const a of s.accounts) entries["account:" + a.id] = a;
        for (const [k, v] of Object.entries(s.quotas))
          entries["quota:" + k] = v;
        for (const [k, v] of Object.entries(s.policies))
          entries["policy:" + k] = v;
        for (const [k, v] of Object.entries(s.incidents))
          entries["incident:" + k] = v;
        for (const [k, v] of Object.entries(s.deliveries))
          entries["receipt:" + k] = v;
        const changed = Object.entries(entries).filter(
          ([k, v]) => JSON.stringify(rows.get(k)) !== JSON.stringify(v),
        );
        await this.ctx.storage.transaction(async (tx) => {
          for (const [k, v] of changed) {
            if (JSON.stringify(v).length > 120000)
              throw new Error("Storage entry too large");
            await tx.put(k, v);
          }
          for (const k of rows.keys()) if (!(k in entries)) await tx.delete(k);
        });
        rows.clear();
        for (const [k, v] of Object.entries(entries))
          rows.set(k, structuredClone(v));
        if (Object.values(s.incidents).some((i) => !terminal(i.phase)))
          await this.ctx.storage.setAlarm(Date.now() + 15000);
      },
      route: async (accounts, quotas, request, now) =>
        control<Decision>(this.env.CONTROL, this.env.INTERNAL_TOKEN, "route", {
          accounts,
          usage: quotas.map((q) => ({
            ...q.snapshot,
            windows: q.complete ? q.snapshot.windows : [],
          })),
          request,
          now,
        }),
      head: (r, b) => gh.head(r, b),
      start: async (input: Start) => {
        await sessionCall(
          this.env,
          input.owner,
          input.attempt_id,
          input.previous?.session_id ? "follow_up" : "start",
          {
            input,
          },
        );
      },
      status: async (o, a) =>
        (await sessionCall(this.env, o, a, "status")) as any,
      cancel: async (o, a) => {
        await sessionCall(this.env, o, a, "cancel");
      },
      publish: (r, b, i, p) => gh.publish(r, b, i, p),
      pull: (r, n) => gh.pull(r, n),
      merge: (r, p) => gh.merge(r, p),
      deployment: (r, s, p) => gh.deployment(r, s, p),
    });
  }
  async alarm() {
    await this.serial(async () => {
      const e = await this.engine();
      await e.tick(Math.floor(Date.now() / 1000));
    });
  }
  async fetch(req: Request) {
    return this.serial(async () => {
      try {
        const u = new URL(req.url),
          owner = id.parse(u.searchParams.get("owner")),
          repo = repository.parse(u.searchParams.get("repo"));
        const e = await this.engine(owner),
          now = Math.floor(Date.now() / 1000);
        const action = u.pathname.slice(1);
        if (action === "hook") {
          const policy = e.state.policies[repo],
            source = policy?.sources.find(
              (s) => s.id === u.searchParams.get("source"),
            );
          if (!source || !policy.enabled)
            return new Response("Unknown/disabled source", { status: 404 });
          const body = await boundedBody(req);
          const secrets = JSON.parse(this.env.CREDENTIALS ?? "{}") as Record<
            string,
            { owner: string; value: string }
          >;
          const secret = secrets[source.secret_ref];
          if (
            !secret ||
            secret.owner !== owner ||
            !(await verifySignature(secret.value, req.headers, body, now))
          )
            return new Response("Invalid source signature", { status: 401 });
          if (source.kind === "usage") {
            const report = JSON.parse(body);
            if (report.rate_limits) {
              const account = e.state.accounts.find(
                (a) => a.id === report.account_id && a.harness === "codex",
              );
              if (!account) throw new Error("Unknown Codex account");
              await e.quota(
                codexQuota(
                  account.id,
                  report.rate_limits,
                  Number(req.headers.get("x-wreckit-timestamp")),
                ),
                now,
              );
            } else await e.quota(report, now);
            return Response.json({ accepted: true });
          }
          if (source.kind === "health") {
            await e.health(repo, healthSchema.parse(JSON.parse(body)), now);
            return Response.json({ accepted: true });
          }
          const signal = signalSchema.parse(JSON.parse(body));
          const incident = await e.ingest(
            { ...signal, kind: source.kind, repo, source: source.id },
            now,
          );
          return Response.json(
            { id: incident.id, phase: incident.phase },
            { status: 202 },
          );
        }
        if (action === "view" && req.method === "GET")
          return Response.json({
            owner,
            policy: e.state.policies[repo] ?? null,
            accounts: e.state.accounts,
            quotas: e.state.quotas,
            incidents: Object.values(e.state.incidents).filter(
              (i) => i.repo === repo,
            ),
          });
        const body = JSON.parse(await boundedBody(req));
        if (action === "account") await e.account(body);
        else if (action === "policy") await e.policy(repo, body);
        else if (action === "quota") await e.quota(body, now);
        else if (action === "codex-quota") {
          const account = e.state.accounts.find(
            (a) => a.id === body.account_id && a.harness === "codex",
          );
          if (!account) throw new Error("Unknown Codex account");
          await e.quota(codexQuota(account.id, body.rate_limits, now), now);
        } else if (action === "cancel") {
          const incident = e.state.incidents[body.id];
          if (!incident || incident.repo !== repo)
            throw new Error("Unknown incident");
          await e.cancel(body.id, now);
        } else if (action === "tick") await e.tick(now);
        else return new Response("Not found", { status: 404 });
        return Response.json({ ok: true });
      } catch {
        return Response.json(
          {
            error:
              "Invalid request or unavailable integration; check configuration and retry",
          },
          { status: 400 },
        );
      }
    });
  }
}
export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    // No public workers.dev route. Rust gateway supplies trusted owner/repo after
    // GitHub session and push-permission verification. Hooks are signed separately.
    if (
      !env.INTERNAL_TOKEN ||
      req.headers.get("authorization") !== `Bearer ${env.INTERNAL_TOKEN}`
    )
      return new Response("Unauthorized", { status: 401 });
    const url = new URL(req.url);
    const owner = url.searchParams.get("owner");
    if (!id.safeParse(owner).success)
      return new Response("Invalid owner", { status: 400 });
    const stub = env.OWNERS.get(env.OWNERS.idFromName(owner!));
    return stub.fetch(req);
  },
};
