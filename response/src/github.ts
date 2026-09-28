import type { Incident, Policy } from "./schema";
import type { Deployment, PullState } from "./engine";
export interface Control {
  fetch(input: Request | string, init?: RequestInit): Promise<Response>;
}
export async function control<T>(
  binding: Control,
  secret: string,
  path: string,
  body: unknown,
): Promise<T> {
  const res = await binding.fetch(`https://control/internal/response/${path}`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${secret}`,
      "content-type": "application/json",
    },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`Control unavailable (${res.status})`);
  return res.json() as Promise<T>;
}
export class GitHub {
  constructor(
    private binding: Control,
    private secret: string,
    private fetcher: typeof fetch = (input, init) => fetch(input, init),
  ) {}
  async token(repo: string, read_only = false): Promise<string> {
    return (
      await control<{ token: string }>(this.binding, this.secret, "token", {
        repo,
        read_only,
      })
    ).token;
  }
  async call<T = any>(
    repo: string,
    path: string,
    method = "GET",
    body?: unknown,
  ): Promise<T> {
    const token = await this.token(repo);
    const res = await this.fetcher(
      `https://api.github.com/repos/${repo}${path}`,
      {
        method,
        headers: {
          authorization: `Bearer ${token}`,
          accept: "application/vnd.github+json",
          "user-agent": "wreck-it-response",
          "x-github-api-version": "2022-11-28",
          "content-type": "application/json",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
        signal: AbortSignal.timeout(15000),
      },
    );
    if (!res.ok) throw new Error(`GitHub ${method} failed (${res.status})`);
    return res.json() as Promise<T>;
  }
  async head(repo: string, branch: string): Promise<string> {
    return (await this.call(repo, `/commits/${encodeURIComponent(branch)}`))
      .sha;
  }
  async publish(
    repo: string,
    base: string,
    i: Incident,
    p: Policy,
  ): Promise<NonNullable<Incident["pr"]>> {
    const branch = `wreck-it/response-${i.attempt_id}`;
    const prs = await this.call<any[]>(
      repo,
      `/pulls?state=all&head=${encodeURIComponent(repo.split("/")[0] + ":" + branch)}&base=${encodeURIComponent(p.base_branch)}`,
    );
    if (prs.length) {
      const pr = prs[0];
      return {
        number: pr.number,
        url: pr.html_url,
        head: pr.head.sha,
        node_id: pr.node_id,
      };
    }
    const parent = await this.call(repo, `/git/commits/${base}`);
    const tree = [];
    for (const file of i.result!.files) {
      const blob =
        file.content === null
          ? null
          : await this.call(repo, "/git/blobs", "POST", {
              content: file.content,
              encoding: "base64",
            });
      tree.push({
        path: file.path,
        mode: file.mode,
        type: "blob",
        sha: blob?.sha ?? null,
      });
    }
    const createdTree = await this.call(repo, "/git/trees", "POST", {
      base_tree: parent.tree.sha,
      tree,
    });
    const identity = {
      name: "wreck-it",
      email: "wreck-it@users.noreply.github.com",
      date: new Date(i.created_at * 1000).toISOString(),
    };
    const commit = await this.call(repo, "/git/commits", "POST", {
      message: `Repair incident ${i.id}`,
      tree: createdTree.sha,
      parents: [base],
      author: identity,
      committer: identity,
    });
    // A lost successful create response is reconciled before any write.
    let ref: any;
    try {
      ref = await this.call(repo, `/git/ref/heads/${branch}`);
    } catch {
      ref = undefined;
    }
    if (ref && ref.object.sha !== commit.sha)
      throw new Error("Response branch was modified externally");
    if (!ref)
      await this.call(repo, "/git/refs", "POST", {
        ref: `refs/heads/${branch}`,
        sha: commit.sha,
      });
    const pr = await this.call(repo, "/pulls", "POST", {
      title: `[wreck-it] ${i.signal.title.slice(0, 200)}`,
      head: branch,
      base: p.base_branch,
      body: `Autonomous response to incident ${i.id}.\n\n${i.result!.summary}\n\nConfigured verification commands passed.\n\n<!-- wreck-it-response:${i.attempt_id} -->`,
    });
    return {
      number: pr.number,
      url: pr.html_url,
      head: pr.head.sha,
      node_id: pr.node_id,
    };
  }
  async pull(repo: string, number: number): Promise<PullState> {
    const p = await this.call(repo, `/pulls/${number}`);
    const [runs, status, reviews, files] = await Promise.all([
      this.call(
        repo,
        `/commits/${p.head.sha}/check-runs?per_page=100&filter=latest`,
      ),
      this.call(repo, `/commits/${p.head.sha}/status?per_page=100`),
      this.call<any[]>(repo, `/pulls/${number}/reviews?per_page=100`),
      this.call<any[]>(repo, `/pulls/${number}/files?per_page=100`),
    ]);
    const latest = new Map<string, any>();
    for (const r of reviews) latest.set(r.user.login, r);
    const checks = new Map<
      string,
      { name: string; sha: string; success: boolean }
    >();
    for (const c of runs.check_runs)
      if (!checks.has(c.name))
        checks.set(c.name, {
          name: c.name,
          sha: c.head_sha,
          success: c.status === "completed" && c.conclusion === "success",
        });
    for (const c of status.statuses)
      if (!checks.has(c.context))
        checks.set(c.context, {
          name: c.context,
          sha: status.sha,
          success: c.state === "success",
        });
    return {
      number,
      url: p.html_url,
      head: p.head.sha,
      node_id: p.node_id,
      merged: p.merged,
      merge_sha: p.merge_commit_sha,
      closed: p.state === "closed" && !p.merged,
      checks: [...checks.values()],
      approved: [...latest.values()].some(
        (r) => r.state === "APPROVED" && r.commit_id === p.head.sha,
      ),
      blocked:
        p.draft ||
        p.mergeable !== true ||
        ["blocked", "dirty", "unknown", "behind"].includes(p.mergeable_state) ||
        [...latest.values()].some((r) => r.state === "CHANGES_REQUESTED") ||
        runs.total_count > 100 ||
        status.total_count > 100 ||
        reviews.length >= 100 ||
        files.length >= 100,
      files: files.map((f) => f.filename),
    };
  }
  async merge(repo: string, pr: PullState) {
    // Atomic head compare-and-merge. GitHub branch protections remain in force;
    // this avoids a changed head racing a queued auto-merge request.
    const result = await this.call(repo, `/pulls/${pr.number}/merge`, "PUT", {
      sha: pr.head,
      merge_method: "squash",
    });
    if (!result.merged) throw new Error("Merge not accepted");
  }
  async deployment(
    repo: string,
    sha: string,
    p: Policy,
  ): Promise<Deployment | undefined> {
    const workflows = await this.call(
      repo,
      `/actions/workflows/${encodeURIComponent(p.deployment_workflow)}/runs?head_sha=${sha}&event=push&per_page=100`,
    );
    const run = workflows.workflow_runs.find((r: any) => r.head_sha === sha);
    if (!run) return;
    if (run.status === "completed" && run.conclusion !== "success")
      return {
        id: String(run.id),
        sha,
        environment: p.environment,
        url: run.html_url,
        at: Date.parse(run.updated_at) / 1000,
        status: "failure",
      };
    if (run.conclusion !== "success") return;
    const deployments = await this.call<any[]>(
      repo,
      `/deployments?sha=${sha}&environment=${encodeURIComponent(p.environment)}&per_page=100`,
    );
    for (const d of deployments) {
      if (d.sha !== sha || d.environment !== p.environment) continue;
      const statuses = await this.call<any[]>(
        repo,
        `/deployments/${d.id}/statuses?per_page=1`,
      );
      const s = statuses[0];
      if (!s?.log_url) continue;
      let log: URL;
      try {
        log = new URL(s.log_url);
      } catch {
        continue;
      }
      const expectedPath = `/${repo}/actions/runs/${run.id}`;
      if (
        log.protocol !== "https:" ||
        log.hostname !== "github.com" ||
        !(
          log.pathname === expectedPath ||
          log.pathname.startsWith(expectedPath + "/")
        )
      )
        continue;
      return {
        id: String(d.id),
        sha,
        environment: d.environment,
        url: s.environment_url || run.html_url,
        at: Date.parse(s.created_at) / 1000,
        status:
          s.state === "success"
            ? "success"
            : ["failure", "error"].includes(s.state)
              ? "failure"
              : "pending",
      };
    }
  }
}
