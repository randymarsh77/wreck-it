// Trusted adapter invoked by an authenticated error/mail/log integration.
// Reads provider data on stdin and signs a bounded normalized signal. Never
// expose this script as an unauthenticated public HTTP relay.
import { createHmac, createHash } from "node:crypto";
const kind = process.argv[2],
  endpoint = process.env.WRECKIT_SIGNAL_URL,
  secret = process.env.WRECKIT_SIGNAL_SECRET;
if (!endpoint?.startsWith("https://") || !secret)
  throw new Error("Set an HTTPS signal URL and signing secret");
let raw = "";
for await (const chunk of process.stdin) {
  raw += chunk;
  if (raw.length > 1_000_000) throw new Error("Input too large");
}
const data = JSON.parse(raw),
  now = Math.floor(Date.now() / 1000);
const fingerprint = (s) => createHash("sha256").update(String(s)).digest("hex");
let signal;
if (kind === "sentry") {
  const event = data.data?.event ?? data.event ?? data;
  signal = {
    delivery_id: String(event.event_id ?? event.id),
    fingerprint: fingerprint(
      event.groupID ?? event.fingerprint ?? event.title ?? event.message,
    ),
    occurred_at: now,
    title: String(event.title ?? event.message ?? "Error report").slice(0, 300),
    evidence: JSON.stringify({
      exception: event.exception,
      culprit: event.culprit,
      message: event.message,
    }).slice(0, 16000),
  };
} else if (kind === "email") {
  // Mail adapters supply Message-ID/from/subject/text after their own provider's
  // signature verification. Recipient/repository mapping is fixed by the URL.
  if (!data.message_id || !data.from || !data.subject)
    throw new Error("Email requires message_id, from, subject");
  signal = {
    delivery_id: fingerprint(data.message_id),
    fingerprint: fingerprint(data.thread_id ?? data.message_id),
    occurred_at: now,
    title: String(data.subject).slice(0, 300),
    subject: String(data.subject).slice(0, 300),
    evidence: String(data.text ?? "").slice(0, 16000),
    sender: data.from.toLowerCase(),
  };
} else if (kind === "logs") {
  if (
    !data.deployment?.sha ||
    !data.deployment?.id ||
    !data.deployment?.environment
  )
    throw new Error("Logs require exact deployment identity");
  signal = {
    delivery_id: fingerprint(data.delivery_id ?? raw),
    fingerprint: fingerprint(data.fingerprint ?? data.error),
    occurred_at: now,
    title: String(data.error ?? "Post-deploy regression").slice(0, 300),
    evidence: JSON.stringify(data.logs ?? []).slice(0, 16000),
    deployment: data.deployment,
  };
} else if (kind === "health") {
  signal = {
    delivery_id: fingerprint(data.delivery_id ?? raw),
    sha: data.sha,
    environment: data.environment,
    deployment_id: data.deployment_id,
    observed_at: now,
    healthy: data.healthy,
    evidence: String(data.evidence ?? "").slice(0, 16000),
  };
} else throw new Error("Use sentry, email, logs, or health");
const body = JSON.stringify(signal),
  timestamp = String(now);
const response = await fetch(endpoint, {
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
if (!response.ok) throw new Error(`Signal rejected (${response.status})`);
console.log(await response.text());
