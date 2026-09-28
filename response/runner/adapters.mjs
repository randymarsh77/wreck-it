// Pinned official CLI contracts. All commands are executed as argv, never shells.
export function command(input) {
  const { account, model, prompt, session_id } = input;
  if (account.harness === "codex")
    return {
      file: "codex",
      args: session_id
        ? ["exec", "resume", session_id, "--json", "--model", model, prompt]
        : [
            "exec",
            "--json",
            "--sandbox",
            "workspace-write",
            "--model",
            model,
            prompt,
          ],
    };
  if (account.harness === "claude_code")
    return {
      file: "claude",
      args: [
        "-p",
        prompt,
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        "--permission-mode",
        "acceptEdits",
        "--allowedTools",
        "Read,Edit,Write,Glob,Grep,Bash",
        ...(session_id ? ["--resume", session_id] : []),
      ],
    };
  if (account.harness === "copilot")
    return {
      file: "copilot",
      args: [
        "-p",
        prompt,
        "--model",
        model,
        "--output-format",
        "json",
        "--allow-all-tools",
        "--no-ask-user",
        ...(session_id ? ["--resume", session_id] : []),
      ],
    };
  throw new Error("Unsupported harness");
}
export function parse(harness, stdout, stderr, exitCode) {
  let session_id,
    summary = "",
    assessment,
    failed = exitCode !== 0;
  for (const line of stdout.split("\n")) {
    let e;
    try {
      e = JSON.parse(line);
    } catch {
      continue;
    }
    if (e.type === "thread.started") session_id = e.thread_id;
    if (e.session_id) session_id = e.session_id;
    if (e.type === "session.start" && e.data?.sessionId)
      session_id = e.data.sessionId;
    if (e.type === "assistant.message" && e.data?.content)
      summary = e.data.content;
    if (e.type === "item.completed" && e.item?.type === "agent_message")
      summary = e.item.text;
    if (e.type === "result") {
      summary = e.result ?? "";
      if (e.is_error) failed = true;
    }
    if (
      e.type === "turn.failed" ||
      e.type === "error" ||
      e.type === "session.error"
    )
      failed = true;
  }
  try {
    const data = JSON.parse(summary.replace(/^```json\s*|\s*```$/g, ""));
    if (["repair", "deep_repair", "no_change"].includes(data.assessment))
      assessment = data.assessment;
  } catch {}
  if (!summary && !session_id) failed = true;
  const errors = stdout + "\n" + stderr;
  const error = /rate.?limit|quota|429|usage limit/i.test(errors)
    ? "quota"
    : /unauthori[sz]ed|authentication|login required|401|token expired/i.test(
          errors,
        )
      ? "auth"
      : "execution";
  return {
    status: failed ? "failed" : "succeeded",
    session_id,
    summary: summary.slice(-8000),
    assessment,
    ...(failed ? { error } : {}),
    tests_passed: false,
    files: [],
  };
}
