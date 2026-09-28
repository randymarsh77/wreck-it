// Owner-side autonomous usage reporting through a dedicated signed source.
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { createHmac } from "node:crypto";
import { fileURLToPath } from "node:url";
const run = promisify(execFile),
  account = process.env.WRECKIT_ACCOUNT_ID,
  url = process.env.WRECKIT_USAGE_URL,
  secret = process.env.WRECKIT_USAGE_SECRET;
const interval = Number(process.env.WRECKIT_USAGE_INTERVAL_SECONDS ?? 60);
if (
  !account ||
  !url?.startsWith("https://") ||
  !secret ||
  !Number.isInteger(interval) ||
  interval < 30
)
  throw new Error(
    "Configure account, HTTPS usage source, signing secret, and interval >= 30 seconds",
  );
const argv = process.env.WRECKIT_USAGE_COMMAND
  ? JSON.parse(process.env.WRECKIT_USAGE_COMMAND)
  : [
      process.execPath,
      fileURLToPath(new URL("./codex-usage.mjs", import.meta.url)),
    ];
if (
  !Array.isArray(argv) ||
  !argv.length ||
  !argv.every((s) => typeof s === "string" && s)
)
  throw new Error("Usage command must be an argv array");
while (true) {
  const timestamp = String(Math.floor(Date.now() / 1000));
  let report;
  try {
    const { stdout } = await run(argv[0], argv.slice(1), {
      timeout: 20000,
      maxBuffer: 64000,
    });
    const data = JSON.parse(stdout);
    report = process.env.WRECKIT_USAGE_COMMAND
      ? data
      : { account_id: account, rate_limits: data };
  } catch {
    report = {
      source: "unavailable",
      complete: false,
      snapshot: {
        account_id: account,
        observed_at: Number(timestamp),
        windows: [],
        active_sessions: 0,
        blocked_until: null,
        requires_reauthentication: false,
      },
    };
  }
  const body = JSON.stringify(report);
  try {
    const r = await fetch(url, {
      method: "POST",
      redirect: "error",
      headers: {
        "content-type": "application/json",
        "x-wreckit-timestamp": timestamp,
        "x-wreckit-signature":
          "sha256=" +
          createHmac("sha256", secret)
            .update(timestamp + "." + body)
            .digest("hex"),
      },
      body,
      signal: AbortSignal.timeout(15000),
    });
    if (!r.ok) console.error(`Usage report rejected (${r.status})`);
  } catch {
    console.error("Usage report transport unavailable");
  }
  await new Promise((resolve) => setTimeout(resolve, interval * 1000));
}
