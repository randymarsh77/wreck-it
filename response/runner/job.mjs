import { spawn } from "node:child_process";
import {
  readFile,
  writeFile,
  rename,
  mkdir,
  rm,
  lstat,
  chown,
  chmod,
} from "node:fs/promises";
import { command, parse } from "./adapters.mjs";
import path from "node:path";
const input = JSON.parse(await readFile(process.argv[2], "utf8"));
const workspace = process.env.WRECKIT_WORKSPACE ?? "/workspace";
const root = path.join(workspace, "repo"),
  output = path.join(workspace, "result.json");
const isolated = process.getuid?.() === 0;
const identity = isolated ? { uid: 1000, gid: 1000 } : {};
const secrets = [
  process.env.WRECKIT_PROVIDER_SECRET,
  process.env.WRECKIT_GIT_READ_TOKEN,
].filter(Boolean);
const deadline = Date.now() + input.timeout_seconds * 1000;
const cleanEnv = {
  PATH: process.env.PATH,
  HOME: path.join(workspace, "home"),
  LANG: "C.UTF-8",
  CI: "1",
  RUSTUP_HOME: process.env.RUSTUP_HOME ?? "/opt/rustup",
  COPILOT_AUTO_UPDATE: "false",
};
async function exec(file, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(file, args, {
      cwd: options.cwd ?? root,
      env: { ...cleanEnv, ...options.env },
      stdio: ["pipe", "pipe", "pipe"],
      detached: true,
      ...identity,
    });
    let stdout = "",
      stderr = "",
      overflow = false;
    const stop = () => {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {}
    };
    const timer = setTimeout(stop, Math.max(1, deadline - Date.now()));
    child.stdout.on("data", (d) => {
      stdout += d;
      if (stdout.length > 2_000_000) {
        overflow = true;
        stop();
      }
    });
    child.stderr.on("data", (d) => {
      stderr += d;
      if (stderr.length > 100000) {
        overflow = true;
        stop();
      }
    });
    child.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ stdout, stderr, code: code ?? -1, overflow });
    });
    child.stdin.end(options.stdin);
  });
}
const checked = async (file, args, options) => {
  const r = await exec(file, args, options);
  if (r.code !== 0 || r.overflow) throw new Error("Command failed");
  return r.stdout;
};
let result = {
  status: "failed",
  error: "execution",
  summary: "Sandbox execution failed",
  tests_passed: false,
  files: [],
};
try {
  await mkdir(root, { recursive: true });
  await mkdir(cleanEnv.HOME, { recursive: true });
  if (isolated) {
    await chown(root, 1000, 1000);
    await chown(cleanEnv.HOME, 1000, 1000);
    await chown(workspace, 0, 0);
    await chmod(workspace, 0o755);
  }
  await chmod(process.argv[2], 0o600);
  const readToken = process.env.WRECKIT_GIT_READ_TOKEN;
  const auth = readToken
    ? {
        GIT_CONFIG_COUNT: "1",
        GIT_CONFIG_KEY_0: "http.https://github.com/.extraheader",
        GIT_CONFIG_VALUE_0:
          "AUTHORIZATION: basic " +
          Buffer.from("x-access-token:" + readToken).toString("base64"),
      }
    : {};
  delete process.env.WRECKIT_GIT_READ_TOKEN;
  await checked("git", ["init", root], { cwd: workspace });
  await checked("git", [
    "remote",
    "add",
    "origin",
    process.env.WRECKIT_REPOSITORY_URL ??
      `https://github.com/${input.repo}.git`,
  ]);
  await checked("git", ["fetch", "--depth=1", "origin", input.base_sha], {
    env: auth,
  });
  await checked("git", ["checkout", "--detach", input.base_sha]);
  const cliEnv = {};
  if (input.account.auth === "api_key")
    cliEnv[
      input.account.harness === "codex" ? "CODEX_API_KEY" : "ANTHROPIC_API_KEY"
    ] = process.env.WRECKIT_PROVIDER_SECRET;
  if (input.account.auth === "copilot_token")
    cliEnv.COPILOT_GITHUB_TOKEN = process.env.WRECKIT_PROVIDER_SECRET;
  if (input.account.auth === "native_subscription") {
    if (!process.env.CODEX_HOME) throw new Error("Native auth unavailable");
    cliEnv.CODEX_HOME = process.env.CODEX_HOME;
  }
  // Native Codex authentication is never transported through this job. It is
  // delegated to an owner-managed runner implementing the same service contract.
  delete process.env.WRECKIT_PROVIDER_SECRET;
  const resume =
    input.account.auth === "native_subscription" &&
    input.previous?.account_id === input.account.id
      ? input.previous.session_id
      : undefined;
  const c = command({ ...input, session_id: resume });
  const r = await exec(c.file, c.args, { env: cliEnv });
  result = parse(input.account.harness, r.stdout, r.stderr, r.code);
  if (r.overflow)
    result = {
      ...result,
      status: "failed",
      error: "execution",
      summary: "CLI output exceeded limit",
    };
  if (result.status === "succeeded" && input.capability !== "triage") {
    for (const argv of input.verification)
      await checked(argv[0], argv.slice(1));
    await checked("git", ["add", "-A"]);
    const names = (
      await checked("git", [
        "diff",
        "--cached",
        "--name-only",
        "-z",
        input.base_sha,
      ])
    )
      .split("\0")
      .filter(Boolean);
    if (names.length > 50) throw new Error("Too many files");
    const files = [];
    let bytes = 0;
    for (const name of names) {
      if (
        name.split("/").some((p) => p === ".." || p === ".git") ||
        path.isAbsolute(name)
      )
        throw new Error("Invalid file");
      const info = await exec("git", ["ls-files", "--stage", "--", name]);
      if (!info.stdout) {
        files.push({ path: name, content: null, mode: "100644" });
        continue;
      }
      const mode = info.stdout.slice(0, 6);
      if (!["100644", "100755"].includes(mode))
        throw new Error("Symlinks/submodules unsupported");
      // Read the staged blob rather than following repository-controlled paths.
      const blob = await exec("git", ["show", ":" + name]);
      if (blob.code !== 0) throw new Error("Missing blob");
      if (blob.stdout.includes("\uFFFD"))
        throw new Error("Non-UTF8 patch unsupported");
      const data = Buffer.from(blob.stdout);
      bytes += data.length;
      if (data.length > 24000 || bytes > 24000)
        throw new Error("Patch too large");
      files.push({ path: name, content: data.toString("base64"), mode });
    }
    result = { ...result, tests_passed: true, files };
  }
} catch {
  result = {
    ...result,
    status: "failed",
    error: Date.now() >= deadline ? "timeout" : "validation",
    summary: "Execution or verification failed",
    tests_passed: false,
    files: [],
  };
}
// Never return raw CLI output or provider credentials. Summary redaction is also
// applied by the coordinator before persistence and publication.
for (const value of [...secrets, ...Object.values(process.env)])
  if (value && value.length > 12)
    result.summary = result.summary.split(value).join("[REDACTED]");
if (
  result.files.some(
    (f) =>
      f.content &&
      secrets.some((s) =>
        Buffer.from(f.content, "base64").includes(Buffer.from(s)),
      ),
  )
)
  result = {
    status: "failed",
    error: "validation",
    summary: "Credential detected in patch",
    tests_passed: false,
    files: [],
  };
await writeFile(output + ".tmp", JSON.stringify(result), { mode: 0o644 });
await rename(output + ".tmp", output);
