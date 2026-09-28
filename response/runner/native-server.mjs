// Owner-managed runner for native Codex subscription auth. Place behind a private
// Cloudflare Tunnel/service binding; requires Docker and an owner-provisioned
// auth directory. The native auth file never crosses the HTTP boundary.
import http from "node:http";
import { spawnSync } from "node:child_process";
import { mkdir, readFile, writeFile, rename } from "node:fs/promises";
import { createHash, timingSafeEqual } from "node:crypto";
import path from "node:path";
const root = process.env.WRECKIT_NATIVE_STATE;
const token = process.env.WRECKIT_NATIVE_TOKEN;
const accounts = JSON.parse(process.env.WRECKIT_NATIVE_ACCOUNTS ?? "[]");
if (!root || !path.isAbsolute(root) || !token)
  throw new Error("Configure native runner state, token, and account homes");
const image = process.env.WRECKIT_NATIVE_IMAGE ?? "wreck-it-response:0.1";
await mkdir(root, { recursive: true, mode: 0o700 });
const docker = (args, env = {}) => {
  const r = spawnSync("docker", args, {
    encoding: "utf8",
    timeout: 20000,
    maxBuffer: 1000000,
    env: { ...process.env, ...env },
  });
  if (r.status !== 0) throw new Error("Docker operation failed");
  return r.stdout;
};
let queue = Promise.resolve();
http
  .createServer((req, res) => {
    queue = queue
      .then(async () => {
        try {
          const supplied = Buffer.from(req.headers.authorization ?? ""),
            expected = Buffer.from("Bearer " + token);
          if (
            supplied.length !== expected.length ||
            !timingSafeEqual(supplied, expected)
          ) {
            res.writeHead(401);
            res.end();
            return;
          }
          let raw = "";
          for await (const chunk of req) {
            raw += chunk;
            if (raw.length > 65536) throw new Error("Payload too large");
          }
          const b = JSON.parse(raw);
          if (!/^[\w.-]+$/.test(b.owner) || !/^[\w.-]+$/.test(b.attempt))
            throw new Error("Invalid identity");
          const key = createHash("sha256")
              .update(b.owner + ":" + b.attempt)
              .digest("hex"),
            dir = path.join(root, key),
            workspace = path.join(root, key, "workspace"),
            name = "wreckit-" + key;
          let record;
          try {
            record = JSON.parse(
              await readFile(path.join(dir, "record.json"), "utf8"),
            );
          } catch {}
          const save = async (r) => {
            await mkdir(dir, { recursive: true });
            await writeFile(path.join(dir, "record.tmp"), JSON.stringify(r), {
              mode: 0o600,
            });
            await rename(
              path.join(dir, "record.tmp"),
              path.join(dir, "record.json"),
            );
          };
          if (req.url === "/cancel") {
            // Stop is idempotent; absence is confirmed with inspect.
            if (record) {
              const exists = docker([
                "ps",
                "-a",
                "--filter",
                `name=^${name}$`,
                "--format",
                "{{.Names}}",
              ]).trim();
              if (exists) docker(["rm", "-f", name]);
            }
            await save({ status: "cancelled" });
            res.end(JSON.stringify({ ok: true }));
            return;
          }
          if (req.url === "/start") {
            const input = b.input;
            const a = accounts.find(
              (a) =>
                a.owner === b.owner && a.ref === input?.account?.credential_ref,
            );
            if (
              !a ||
              input.account.auth !== "native_subscription" ||
              input.account.harness !== "codex" ||
              input.owner !== b.owner ||
              input.attempt_id !== b.attempt ||
              !path.isAbsolute(a.home)
            )
              throw new Error("Unauthorized native account");
            if (!record) {
              await mkdir(workspace, { recursive: true });
              await writeFile(
                path.join(workspace, "input.json"),
                JSON.stringify(input),
                { mode: 0o600 },
              );
              record = { status: "starting", input };
              await save(record);
            }
            if (
              record.input &&
              JSON.stringify(record.input) !== JSON.stringify(input)
            )
              throw new Error("Conflicting attempt");
            if (record.status === "starting") {
              let exists = false;
              try {
                docker(["inspect", name]);
                exists = true;
              } catch {}
              if (!exists) {
                // Network policy must be provided by the operator through this Docker
                // network. No GitHub write credential or Docker socket enters the job.
                docker(
                  [
                    "run",
                    "-d",
                    "--name",
                    name,
                    "--init",
                    "--cap-drop=ALL",
                    "--security-opt",
                    "no-new-privileges",
                    "--pids-limit",
                    "256",
                    "--memory",
                    "4g",
                    "--cpus",
                    "2",
                    "--network",
                    process.env.WRECKIT_NATIVE_NETWORK ?? "bridge",
                    "--mount",
                    `type=bind,src=${workspace},dst=/workspace`,
                    "--mount",
                    `type=bind,src=${a.home},dst=/codex-home`,
                    "-e",
                    "CODEX_HOME=/codex-home",
                    "-e",
                    "WRECKIT_GIT_READ_TOKEN",
                    "--entrypoint",
                    "node",
                    image,
                    "/opt/wreck-it/job.mjs",
                    "/workspace/input.json",
                  ],
                  { WRECKIT_GIT_READ_TOKEN: b.git_read_token ?? "" },
                );
              }
              record.status = "running";
              record.deadline = Date.now() + input.timeout_seconds * 1000;
              await save(record);
            }
            res.end(JSON.stringify({ status: record.status }));
            return;
          }
          if (req.url === "/status") {
            if (!record) throw new Error("Unknown attempt");
            let result;
            if (record.status === "cancelled")
              result = {
                status: "cancelled",
                summary: "Cancelled",
                tests_passed: false,
                files: [],
              };
            else {
              try {
                result = JSON.parse(
                  await readFile(path.join(workspace, "result.json"), "utf8"),
                );
              } catch {}
            }
            if (!result && Date.now() > record.deadline) {
              try {
                docker(["rm", "-f", name]);
              } catch {}
              result = {
                status: "failed",
                error: "timeout",
                summary: "Deadline exceeded",
                tests_passed: false,
                files: [],
              };
            }
            if (result) {
              record.status = result.status;
              record.result = result;
              await save(record);
              try {
                docker(["rm", "-f", name]);
              } catch {}
            }
            res.end(
              JSON.stringify(
                result ??
                  record.result ?? {
                    status: "running",
                    summary: "",
                    tests_passed: false,
                    files: [],
                  },
              ),
            );
            return;
          }
          res.writeHead(404);
          res.end();
        } catch {
          res.writeHead(400);
          res.end(JSON.stringify({ error: "Native runner request failed" }));
        }
      })
      .catch(() => {
        res.writeHead(500);
        res.end();
      });
  })
  .listen(Number(process.env.PORT ?? 8789), "127.0.0.1");
