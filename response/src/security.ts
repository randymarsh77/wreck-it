export function redact(text: string): string {
  return text
    .replace(
      /\b(?:sk-[\w-]+|gh[pousr]_[\w]+|github_pat_[\w]+)\b/g,
      "[REDACTED]",
    )
    .replace(
      /((?:authorization|password|api[_-]?key|token|secret)\s*[=:]\s*)(?:Bearer\s+)?[^\s,;"}]+/gi,
      "$1[REDACTED]",
    )
    .replace(/[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi, "[EMAIL]");
}
export async function digest(text: string): Promise<string> {
  return [
    ...new Uint8Array(
      await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)),
    ),
  ]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
export async function signature(
  secret: string,
  timestamp: string,
  body: string,
): Promise<string> {
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  return [
    ...new Uint8Array(
      await crypto.subtle.sign(
        "HMAC",
        key,
        new TextEncoder().encode(`${timestamp}.${body}`),
      ),
    ),
  ]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
export async function verifySignature(
  secret: string,
  headers: Headers,
  body: string,
  now: number,
): Promise<boolean> {
  const ts = headers.get("x-wreckit-timestamp") ?? "",
    sig = headers.get("x-wreckit-signature") ?? "";
  if (
    !/^\d+$/.test(ts) ||
    Math.abs(now - Number(ts)) > 300 ||
    !/^sha256=[a-f0-9]{64}$/.test(sig)
  )
    return false;
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"],
  );
  return crypto.subtle.verify(
    "HMAC",
    key,
    Uint8Array.from(sig.slice(7).match(/../g)!, (h) => parseInt(h, 16)),
    new TextEncoder().encode(`${ts}.${body}`),
  );
}
export async function boundedBody(req: Request, max = 65536): Promise<string> {
  if (Number(req.headers.get("content-length")) > max)
    throw new Error("Payload too large");
  const reader = req.body?.getReader();
  if (!reader) return "";
  const parts: Uint8Array[] = [];
  let size = 0;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.length;
    if (size > max) {
      await reader.cancel();
      throw new Error("Payload too large");
    }
    parts.push(value);
  }
  const bytes = new Uint8Array(size);
  let offset = 0;
  for (const p of parts) {
    bytes.set(p, offset);
    offset += p.length;
  }
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}
export const terminal = (phase: string) =>
  ["resolved", "needs_attention", "cancelled"].includes(phase);
export function safePath(path: string): boolean {
  return (
    !!path &&
    !path.startsWith("/") &&
    !path.includes("\\") &&
    !path.includes("\0") &&
    path
      .split("/")
      .every((p) => p !== "." && p !== ".." && p !== ".git" && p !== "")
  );
}
