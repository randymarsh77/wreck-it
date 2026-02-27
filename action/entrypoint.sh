#!/bin/sh
set -e

cd "${GITHUB_WORKSPACE:-.}"

echo "[wreck-it] running headless iteration in $(pwd)"

wreck-it run --headless \
  --work-dir "." \
  ${INPUT_TASK_FILE:+--task-file "$INPUT_TASK_FILE"} \
  ${INPUT_MAX_ITERATIONS:+--max-iterations "$INPUT_MAX_ITERATIONS"} \
  ${INPUT_VERIFY_COMMAND:+--verify-command "$INPUT_VERIFY_COMMAND"}

STATE_BRANCH="${INPUT_STATE_BRANCH:-wreck-it-state}"

# Push state branch (managed by wreck-it via git plumbing).
if git rev-parse --verify "refs/heads/${STATE_BRANCH}" >/dev/null 2>&1; then
  echo "[wreck-it] pushing state branch '${STATE_BRANCH}'"
  git push origin "${STATE_BRANCH}"
fi

# Commit any remaining working-tree changes (e.g. task file updates) back to
# the current branch.
if git diff --quiet && git diff --cached --quiet; then
  echo "[wreck-it] no working-tree changes to commit"
else
  git config user.name "wreck-it[bot]"
  git config user.email "wreck-it[bot]@users.noreply.github.com"
  git add -A
  git commit -m "wreck-it: update task state"
  git push
fi
