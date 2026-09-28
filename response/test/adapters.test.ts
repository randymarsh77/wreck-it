import test from "node:test";
import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  writeFileSync,
  readFileSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { execFileSync } from "node:child_process";
// @ts-expect-error The same native ESM module is shipped into the sandbox image.
import { command, parse } from "../runner/adapters.mjs";
const fixtures = {
  codex: [
    { type: "thread.started", thread_id: "native-session" },
    { type: "item.completed", item: { type: "agent_message", text: "Fixed" } },
  ],
  claude_code: [
    { type: "system", session_id: "native-session" },
    {
      type: "result",
      result: "Fixed",
      session_id: "native-session",
      is_error: false,
    },
  ],
  copilot: [
    { type: "session.start", data: { sessionId: "native-session" } },
    { type: "assistant.message", data: { content: "Fixed" } },
  ],
};
for (const [harness, events] of Object.entries(fixtures)) {
  test(`${harness}: official JSONL, failure classification, explicit resume, argv safety`, () => {
    const result = parse(
      harness,
      events.map((e) => JSON.stringify(e)).join("\n"),
      "",
      0,
    );
    assert.equal(result.status, "succeeded");
    assert.equal(result.session_id, "native-session");
    assert.equal(result.summary, "Fixed");
    assert.equal(
      parse(harness, "", "401 authentication failed", 1).error,
      "auth",
    );
    assert.equal(parse(harness, "", "429 quota exceeded", 1).error, "quota");
    const input = {
      account: { harness },
      model: "model",
      prompt: "$(touch /tmp/should-never-run)",
      session_id: "native-session",
    };
    const c = command(input);
    assert.ok(c.args.includes(input.prompt));
    assert.ok(c.args.includes(input.session_id));
  });
  test(`${harness}: fake executable performs a real git patch and verification without provider access`, () => {
    const dir = mkdtempSync(join(tmpdir(), "wreck-runner-"));
    try {
      const seed = join(dir, "seed"),
        bin = join(dir, "bin"),
        workspace = join(dir, "work");
      mkdirSync(seed);
      mkdirSync(bin);
      mkdirSync(workspace);
      const git = (args: string[]) =>
        execFileSync("git", args, {
          cwd: seed,
          encoding: "utf8",
          stdio: ["pipe", "pipe", "pipe"],
        });
      git(["init"]);
      git(["config", "user.email", "test@example.com"]);
      git(["config", "user.name", "test"]);
      writeFileSync(join(seed, "file.txt"), "broken");
      git(["add", "."]);
      git(["commit", "-m", "seed"]);
      const sha = git(["rev-parse", "HEAD"]).trim();
      const file =
        harness === "codex"
          ? "codex"
          : harness === "claude_code"
            ? "claude"
            : "copilot";
      const fake = `#!${process.execPath}\nrequire('fs').writeFileSync('file.txt','fixed');\n${events.map((e) => `console.log(${JSON.stringify(JSON.stringify(e))});`).join("\n")}`;
      writeFileSync(join(bin, file), fake, { mode: 0o755 });
      const input = {
        repo: "org/repo",
        base_sha: sha,
        timeout_seconds: 30,
        capability: "repair",
        account: {
          id: "test",
          harness,
          auth: harness === "copilot" ? "copilot_token" : "api_key",
        },
        model: "model",
        prompt: "Fix the defect",
        verification: [
          [
            process.execPath,
            "-e",
            "if(require('fs').readFileSync('file.txt','utf8')!=='fixed')process.exit(1)",
          ],
        ],
      };
      const inputPath = join(dir, "input.json");
      writeFileSync(inputPath, JSON.stringify(input));
      execFileSync(process.execPath, [resolve("runner/job.mjs"), inputPath], {
        env: {
          ...process.env,
          PATH: bin + ":" + process.env.PATH,
          WRECKIT_WORKSPACE: workspace,
          WRECKIT_REPOSITORY_URL: seed,
          WRECKIT_PROVIDER_SECRET: "never-return-this-secret",
        },
        timeout: 40000,
      });
      const result = JSON.parse(
        readFileSync(join(workspace, "result.json"), "utf8"),
      );
      assert.equal(result.status, "succeeded");
      assert.equal(result.tests_passed, true);
      assert.equal(
        Buffer.from(result.files[0].content, "base64").toString(),
        "fixed",
      );
      assert.ok(!JSON.stringify(result).includes("never-return-this-secret"));
    } finally {
      rmSync(dir, { recursive: true, force: true });
    }
  });
}
test("malformed output and semantic failure are not success", () => {
  assert.equal(
    parse(
      "claude_code",
      '{"type":"result","is_error":true,"result":"failed"}',
      "",
      0,
    ).status,
    "failed",
  );
  assert.equal(
    parse("codex", '{"type":"turn.failed"}', "", 0).status,
    "failed",
  );
});
