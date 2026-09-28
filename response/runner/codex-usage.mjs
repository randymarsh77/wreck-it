// Run inside the owner's authenticated Codex environment. Output is quota data,
// never auth.json. Post it to the portal codex-quota endpoint using owner auth.
import { spawn } from "node:child_process";
const child = spawn("codex", ["app-server"], {
  stdio: ["pipe", "pipe", "ignore"],
});
const timer = setTimeout(() => {
  child.kill();
  process.exitCode = 1;
}, 15000);
let buffer = "",
  done = false;
const send = (x) => child.stdin.write(JSON.stringify(x) + "\n");
send({
  id: 1,
  method: "initialize",
  params: { clientInfo: { name: "wreck-it", version: "1.0.0" } },
});
child.stdout.on("data", (chunk) => {
  buffer += chunk;
  let at;
  while ((at = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, at);
    buffer = buffer.slice(at + 1);
    let e;
    try {
      e = JSON.parse(line);
    } catch {
      continue;
    }
    if (e.id === 1) {
      send({ method: "initialized", params: {} });
      send({ id: 2, method: "account/rateLimits/read", params: {} });
    }
    if (e.id === 2) {
      if (e.result) {
        console.log(JSON.stringify(e.result));
        done = true;
      } else process.exitCode = 1;
      clearTimeout(timer);
      child.kill();
    }
  }
});
child.on("error", () => {
  clearTimeout(timer);
  process.exitCode = 1;
});
child.on("exit", () => {
  clearTimeout(timer);
  if (!done) process.exitCode = 1;
});
