#!/bin/sh
set -e

cd "${GITHUB_WORKSPACE:-.}"

git config --global --add safe.directory "$(pwd)"
git config --global user.name "wreck-it[bot]"
git config --global user.email "wreck-it[bot]@users.noreply.github.com"

# Authenticate the Copilot CLI when a token is provided.
if [ -n "${INPUT_COPILOT_TOKEN}" ]; then
  export GITHUB_TOKEN="${INPUT_COPILOT_TOKEN}"
  echo "[wreck-it] Copilot token configured"
fi

echo "[wreck-it] running headless iteration in $(pwd)"

# wreck-it automatically creates a worktree at .wreck-it/state for the state
# branch.  All config, task, and state file I/O happens there; agent work
# uses this (the default branch) checkout.
wreck-it run --headless \
  --work-dir "." \
  ${INPUT_MODEL_PROVIDER:+--model-provider "$INPUT_MODEL_PROVIDER"} \
  ${INPUT_MAX_ITERATIONS:+--max-iterations "$INPUT_MAX_ITERATIONS"} \
  ${INPUT_VERIFY_COMMAND:+--verify-command "$INPUT_VERIFY_COMMAND"}

STATE_BRANCH="${INPUT_STATE_BRANCH:-wreck-it-state}"

# Push the state branch (commits were made by wreck-it inside the worktree).
if git rev-parse --verify "refs/heads/${STATE_BRANCH}" >/dev/null 2>&1; then
  echo "[wreck-it] pushing state branch '${STATE_BRANCH}'"
  git push origin "${STATE_BRANCH}"
fi
