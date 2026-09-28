import { DurableObject } from "cloudflare:workers";
import { getSandbox } from "@cloudflare/sandbox";
import { type Env } from "./worker";
import {
  type Start,
  type RunResult,
  accountSchema,
  resultSchema,
  sha,
  repository,
  id,
} from "./schema";
import { digest, redact } from "./security";
import { control } from "./github";
interface Record {
  owner: string;
  attempt: string;
  input?: Start;
  status: "starting" | "running" | "succeeded" | "failed" | "cancelled";
  deadline: number;
  sandbox?: string;
  result?: RunResult;
  native?: boolean;
  cleaned?: boolean;
}
export class Session extends DurableObject<Env> {
  private queue: Promise<unknown> = Promise.resolve();
  private serial<T>(f: () => Promise<T>) {
    const n = this.queue.then(f, f);
    this.queue = n.catch(() => {});
    return n;
  }
  private sandbox(r: Record) {
    return getSandbox(this.env.SANDBOX as any, r.sandbox!, {
      sleepAfter: "10m",
    });
  }
  async fetch(req: Request) {
    return this.serial(async () => {
      const body = (await req.json()) as {
        owner: string;
        attempt: string;
        input?: Start;
      };
      const action = new URL(req.url).pathname;
      let r = await this.ctx.storage.get<Record>("run");
      if (r && (r.owner !== body.owner || r.attempt !== body.attempt))
        return new Response("Identity mismatch", { status: 403 });
      if (action === "/cancel") {
        if (!r)
          r = {
            owner: body.owner,
            attempt: body.attempt,
            status: "cancelled",
            deadline: 0,
            cleaned: true,
            result: {
              status: "cancelled",
              summary: "Cancelled",
              tests_passed: false,
              files: [],
            },
          };
        else {
          r.status = "cancelled";
          await this.ctx.storage.put("run", r);
          await this.ctx.storage.setAlarm(Date.now() + 10000);
          await this.stop(r);
          r.cleaned = true;
          r.result = {
            status: "cancelled",
            summary: "Cancelled",
            tests_passed: false,
            files: [],
          };
        }
        await this.ctx.storage.put("run", r);
        return Response.json({ ok: true });
      }
      if (action === "/start" || action === "/follow_up") {
        const input = body.input;
        if (
          !input ||
          input.owner !== body.owner ||
          input.attempt_id !== body.attempt
        )
          return new Response("Invalid start", { status: 400 });
        if (action === "/follow_up") {
          const previous = input.previous;
          if (!previous || previous.attempt_id === input.attempt_id)
            return new Response("Previous attempt required", { status: 400 });
          const prior = this.env.SESSIONS.get(
            this.env.SESSIONS.idFromName(
              await digest(input.owner + ":" + previous.attempt_id),
            ),
          );
          const response = await prior.fetch("https://session/metadata", {
            method: "POST",
            body: JSON.stringify({
              owner: input.owner,
              attempt: previous.attempt_id,
            }),
          });
          if (!response.ok)
            return new Response("Previous session unavailable", {
              status: 409,
            });
          const metadata = (await response.json()) as {
            repo: string;
            account_id: string;
            session_id?: string;
            complete: boolean;
          };
          if (
            !metadata.complete ||
            metadata.repo !== input.repo ||
            metadata.account_id !== previous.account_id ||
            metadata.session_id !== previous.session_id
          )
            return new Response("Previous session scope mismatch", {
              status: 403,
            });
        }
        accountSchema.parse(input.account);
        sha.parse(input.base_sha);
        repository.parse(input.repo);
        id.parse(input.attempt_id);
        if (
          input.account.owner !== input.owner ||
          !input.account.enabled ||
          !input.account.models.some((m) => m.model === input.model)
        )
          return new Response("Account mismatch", { status: 403 });
        if (r) {
          if (r.input && JSON.stringify(r.input) !== JSON.stringify(input))
            return new Response("Attempt already exists with different input", {
              status: 409,
            });
          return Response.json({ status: r.status });
        }
        r = {
          owner: body.owner,
          attempt: body.attempt,
          input,
          status: "starting",
          deadline: Date.now() + input.timeout_seconds * 1000,
          sandbox: (await digest(body.owner + ":" + body.attempt)).slice(0, 48),
          native: input.account.auth === "native_subscription",
        };
        await this.ctx.storage.put("run", r);
        await this.ctx.storage.setAlarm(Date.now() + 1000);
        return Response.json({ status: r.status }, { status: 202 });
      }
      if (!r) return new Response("Unknown session", { status: 404 });
      if (action === "/metadata")
        return Response.json({
          repo: r.input?.repo,
          account_id: r.input?.account.id,
          session_id: r.result?.session_id,
          complete: r.cleaned === true,
        });
      if (action === "/status")
        return Response.json(
          (r.cleaned ? r.result : undefined) ?? {
            status: "running",
            summary: "",
            tests_passed: false,
            files: [],
          },
        );
      return new Response("Not found", { status: 404 });
    });
  }
  private async native(path: string, body: unknown) {
    const url = this.env.NATIVE_RUNNER_URL ?? "https://native";
    if (!this.env.NATIVE_RUNNER && !url.startsWith("https://"))
      throw new Error("Native runner requires HTTPS");
    const init = {
      method: "POST",
      headers: { authorization: `Bearer ${this.env.INTERNAL_TOKEN}` },
      body: JSON.stringify(body),
      redirect: "error" as const,
      signal: AbortSignal.timeout(15000),
    };
    return this.env.NATIVE_RUNNER
      ? this.env.NATIVE_RUNNER.fetch("https://native" + path, init)
      : fetch(new URL(path, url), init);
  }
  async stop(r: Record) {
    if (r.native) {
      const res = await this.native("/cancel", {
        owner: r.owner,
        attempt: r.attempt,
      });
      if (!res.ok) throw new Error("Cancellation unconfirmed");
    } else if (r.sandbox) await this.sandbox(r).destroy();
  }
  async alarm() {
    await this.serial(async () => {
      const r = await this.ctx.storage.get<Record>("run");
      if (!r) return;
      if (["succeeded", "failed", "cancelled"].includes(r.status)) {
        if (!r.cleaned) {
          await this.ctx.storage.setAlarm(Date.now() + 10000);
          await this.stop(r);
          r.cleaned = true;
          await this.ctx.storage.put("run", r);
        }
        await this.ctx.storage.deleteAlarm();
        return;
      }
      // Schedule reconciliation before any I/O, so a lost invocation is retried.
      await this.ctx.storage.setAlarm(Date.now() + 10000);
      try {
        if (Date.now() > r.deadline) {
          await this.stop(r);
          r.cleaned = true;
          r.status = "failed";
          r.result = {
            status: "failed",
            error: "timeout",
            summary: "Session deadline exceeded",
            tests_passed: false,
            files: [],
          };
        } else if (r.native) {
          if (!this.env.NATIVE_RUNNER && !this.env.NATIVE_RUNNER_URL)
            throw new Error("Owner native runner not bound");
          const action = r.status === "starting" ? "start" : "status";
          const git_read_token =
            action === "start"
              ? (
                  await control<{ token: string }>(
                    this.env.CONTROL,
                    this.env.INTERNAL_TOKEN,
                    "token",
                    { repo: r.input!.repo, read_only: true },
                  )
                ).token
              : undefined;
          const response = await this.native("/" + action, {
            owner: r.owner,
            attempt: r.attempt,
            input: r.input,
            git_read_token,
          });
          if (!response.ok) throw new Error("Native runner unavailable");
          if (action === "start") r.status = "running";
          else {
            const result = resultSchema.parse(await response.json());
            if (result.status !== "running") {
              r.result = result;
              r.status = result.status;
            }
          }
        } else if (r.status === "starting") {
          const s = this.sandbox(r),
            input = r.input!;
          const secrets = JSON.parse(this.env.CREDENTIALS ?? "{}") as {
            [key: string]: { owner: string; value: string };
          };
          const credential = secrets[input.account.credential_ref];
          if (!credential || credential.owner !== r.owner) {
            r.status = "failed";
            r.result = {
              status: "failed",
              error: "auth",
              summary: "Credential unavailable for this owner",
              tests_passed: false,
              files: [],
            };
            await this.ctx.storage.put("run", r);
            await this.ctx.storage.setAlarm(Date.now() + 10000);
            return;
          }
          if (
            input.account.auth === "copilot_token" &&
            !/^(github_pat_|gho_|ghu_)/.test(credential.value)
          )
            throw new Error("Unsupported Copilot token");
          await s.setAllowedHosts(input.allowed_hosts ?? []);
          await s.mkdir("/workspace", { recursive: true });
          await s.writeFile("/workspace/input.json", JSON.stringify(input));
          let exists = false;
          try {
            const p = await s.getProcess("wreckit-job");
            exists = !!p;
          } catch {}
          if (!exists) {
            const token = await control<{ token: string }>(
              this.env.CONTROL,
              this.env.INTERNAL_TOKEN,
              "token",
              { repo: input.repo, read_only: true },
            );
            await s.startProcess(
              "node /opt/wreck-it/job.mjs /workspace/input.json",
              {
                processId: "wreckit-job",
                env: {
                  WRECKIT_PROVIDER_SECRET: credential.value,
                  WRECKIT_GIT_READ_TOKEN: token.token,
                },
                timeout: input.timeout_seconds * 1000,
              },
            );
          }
          r.status = "running";
        } else {
          const s = this.sandbox(r);
          const process = await s.getProcess("wreckit-job");
          if (process && ["starting", "running"].includes(process.status))
            return;
          let file;
          try {
            file = await s.readFile("/workspace/result.json");
          } catch {}
          if (file) {
            const result = resultSchema.parse(JSON.parse(file.content));
            result.summary = redact(result.summary);
            r.result = result;
            r.status = result.status as Record["status"];
            await this.ctx.storage.put("run", r);
            await s.destroy();
            r.cleaned = true;
          } else {
            const p = await s.getProcess("wreckit-job");
            if (
              !p ||
              ["completed", "failed", "killed", "error"].includes(p.status)
            ) {
              r.status = "failed";
              r.result = {
                status: "failed",
                error: "execution",
                summary: "Runner exited without a result",
                tests_passed: false,
                files: [],
              };
              await s.destroy();
              r.cleaned = true;
            }
          }
        }
        await this.ctx.storage.put("run", r);
        if (
          ["succeeded", "failed", "cancelled"].includes(r.status) &&
          r.cleaned
        )
          await this.ctx.storage.deleteAlarm();
      } catch {
        /* durable starting/running intent is reconciled until the deadline */
      }
    });
  }
}
