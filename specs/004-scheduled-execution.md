# Spec 004: Scheduled Task Execution via Agent Scheduling

## Summary

Use Cloudflare Agents' built-in scheduling primitives (`this.schedule()`,
`this.scheduleEvery()`, cron expressions) to replace the current GitHub
Actions cron + `pulse` system for recurring ralph execution.

## Motivation

Today, recurring ralph execution relies on:

1. A GitHub Actions workflow running on a cron schedule.
2. The worker's `pulse` system checking cooldown timers.
3. External triggers (webhook events) to initiate iterations.

This has several limitations:

- GitHub Actions cron has ~5-minute granularity and can be delayed.
- The pulse system requires the worker to be invoked externally.
- No fine-grained per-task scheduling control.

Cloudflare Agents support:

- **Delayed execution**: `this.schedule(seconds, taskName, data)`
- **Recurring intervals**: `this.scheduleEvery(seconds, taskName, data)`
- **Cron expressions**: `this.schedule('0 9 * * *', taskName, data)`
- **Durable scheduling**: schedules survive agent hibernation.

## Proposed Design

### Scheduling Integration

```typescript
export class RalphAgent extends Agent<Env, RalphState> {
  @callable()
  async configureSchedule(config: ScheduleConfig) {
    // Cancel existing schedule if any
    this.cancelAll()

    if (config.type === 'interval') {
      this.scheduleEvery(config.seconds, 'runIteration', {})
    } else if (config.type === 'cron') {
      this.schedule(config.expression, 'runIteration', {})
    }

    this.setState({
      ...this.state,
      schedule: config
    })
  }

  async onTask(name: string, data: unknown) {
    if (name === 'runIteration') {
      await this.executeIteration()
    }
  }

  private async executeIteration() {
    // Check cooldowns, pick next task, call LLM, update state
    // Broadcast progress to connected WebSocket clients
  }
}
```

### Schedule Configuration

```typescript
type ScheduleConfig =
  | { type: 'interval'; seconds: number }
  | { type: 'cron'; expression: string }
  | { type: 'manual' }  // no auto-scheduling
```

### Portal UI Integration

The RepoConfig page gains a "Schedule" section per ralph:

- Toggle between Manual / Interval / Cron
- Interval input (minutes/hours)
- Cron expression input with preview
- Next scheduled run display

### Cooldown Respect

The `onTask` handler checks `cooldown_seconds` and `last_attempt_at` on each
task before executing.  If a task is still in cooldown, it is skipped and the
agent re-schedules for the remaining cooldown period.

## Migration Path

1. Deploy agent scheduling alongside existing GitHub Actions cron.
2. Add portal UI to configure schedules per ralph.
3. Once agents handle scheduling reliably, remove the Actions cron workflows
   and the worker `pulse` system.

## Open Questions

- Should each recurring task have its own schedule, or should scheduling be
  per-ralph (the agent picks the next eligible task each time)?
- How do we handle schedule configuration in `.wreck-it/config.toml` vs
  storing it purely in the agent's Durable Object state?
