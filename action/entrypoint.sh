#!/bin/sh
set -e

cd "${GITHUB_WORKSPACE:-.}"

echo "[wreck-it] running headless iteration in $(pwd)"

wreck-it run --headless \
  --work-dir "." \
  ${INPUT_TASK_FILE:+--task-file "$INPUT_TASK_FILE"} \
  ${INPUT_MAX_ITERATIONS:+--max-iterations "$INPUT_MAX_ITERATIONS"} \
  ${INPUT_VERIFY_COMMAND:+--verify-command "$INPUT_VERIFY_COMMAND"}

# Commit state changes back to the repo so the next cron run can pick up.
if git diff --quiet && git diff --cached --quiet; then
  echo "[wreck-it] no state changes to commit"
else
  git config user.name "wreck-it[bot]"
  git config user.email "wreck-it[bot]@users.noreply.github.com"
  git add -A
  git commit -m "wreck-it: update headless state"
  git push
fi
