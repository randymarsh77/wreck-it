mod agent;
mod agent_memory;
mod artefact_store;
mod cli;
mod cloud_agent;
mod config_manager;
mod cost_tracker;
mod gastown_client;
mod github_auth;
mod github_client;
mod graph;
mod headless;
mod headless_config;
mod headless_state;
mod install;
#[cfg(test)]
mod integration_eval;

mod merge;
mod notifier;
mod openclaw;
mod plan_migration;
mod plan_wizard;
mod planner;
mod prompt_loader;
mod provenance;
mod ralph_loop;
mod replanner;
mod repo_config;
mod state_worktree;
mod task_cli;
mod task_manager;
mod templates;
mod tui;
mod types;
mod unstuck;

use anyhow::{Context, Result};
use clap::Parser;
use cli::{Cli, Commands, TasksAction};
use config_manager::{load_user_config, save_user_config};
use ralph_loop::RalphLoop;
use repo_config::{
    is_interactive, is_state_uninitialized, load_repo_config, prompt_with_default,
    repo_config_path, save_repo_config, RepoConfig,
};
use std::env;
use tui::TuiApp;
use types::{
    ModelProvider, Task, TaskStatus, DEFAULT_COPILOT_ENDPOINT, DEFAULT_GITHUB_MODELS_ENDPOINT,
    DEFAULT_LLAMA_ENDPOINT,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            task_file,
            max_iterations,
            work_dir,
            api_endpoint,
            api_token,
            model_provider,
            verify_command,
            evaluation_mode,
            completeness_prompt,
            completion_marker_file,
            headless,
            ralph,
            goal,
            reflection_rounds,
            replan_threshold,
            max_autopilot_continues,
            notify_webhooks,
            github_issues,
            github_repo,
            github_token,
            max_cost_usd,
            work_dir_map,
            prompt_dir,
        } => {
            // Determine work directory early so we can look for the repo config.
            let resolved_work_dir = work_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

            // Check for repo-level config.
            let repo_cfg = match load_repo_config(&resolved_work_dir)? {
                Some(cfg) => cfg,
                None => {
                    println!("wreck-it config not found; run wreck-it init");
                    return Ok(());
                }
            };

            // Resolve the list of ralphs to run.
            let run_all = ralph.as_deref() == Some("all");
            let ralph_list: Vec<repo_config::RalphConfig> = if run_all {
                if !headless {
                    anyhow::bail!("--ralph all is only supported in headless mode");
                }
                if repo_cfg.ralphs.is_empty() {
                    println!("No [[ralphs]] entries in config – nothing to do");
                    return Ok(());
                }
                repo_cfg.ralphs.clone()
            } else if let Some(ref name) = ralph {
                match repo_config::find_ralph(&repo_cfg, name) {
                    Some(rc) => vec![rc.clone()],
                    None => {
                        let available: Vec<&str> =
                            repo_cfg.ralphs.iter().map(|r| r.name.as_str()).collect();
                        println!(
                            "ralph '{}' not found in repo config. available: {:?}",
                            name, available,
                        );
                        return Ok(());
                    }
                }
            } else if headless && !repo_cfg.ralphs.is_empty() {
                // Headless mode with [[ralphs]] configured – run all of them.
                repo_cfg.ralphs.clone()
            } else {
                // No ralph specified – single anonymous run.
                vec![]
            };

            // Set up the state worktree.
            let state_dir =
                state_worktree::ensure_state_worktree(&resolved_work_dir, &repo_cfg.state_branch)?;

            // If state is empty, nothing to do.
            if is_state_uninitialized(&state_dir) {
                println!("wreck-it state is empty; nothing to do");
                return Ok(());
            }

            // Build the base config from user config + CLI overrides.
            let build_config = |ralph_override: Option<&repo_config::RalphConfig>| {
                let mut config = load_user_config().unwrap_or_default();

                if let Some(rc) = ralph_override {
                    config.task_file = rc.task_file.clone().into();
                }

                if let Some(ref task_file) = task_file {
                    config.task_file = task_file.clone();
                }
                if let Some(max_iterations) = max_iterations {
                    config.max_iterations = max_iterations;
                }
                if let Some(ref work_dir) = work_dir {
                    config.work_dir = work_dir.clone();
                }
                if let Some(ref api_endpoint) = api_endpoint {
                    config.api_endpoint = api_endpoint.clone();
                }
                if let Some(ref model_provider) = model_provider {
                    config.model_provider = model_provider.clone();
                }
                if let Some(ref verify_command) = verify_command {
                    config.verification_command = Some(verify_command.clone());
                }
                if let Some(ref evaluation_mode) = evaluation_mode {
                    config.evaluation_mode = *evaluation_mode;
                }
                if let Some(ref completeness_prompt) = completeness_prompt {
                    config.completeness_prompt = Some(completeness_prompt.clone());
                }
                if let Some(ref completion_marker_file) = completion_marker_file {
                    config.completion_marker_file = completion_marker_file.clone();
                }
                if let Some(reflection_rounds) = reflection_rounds {
                    config.reflection_rounds = reflection_rounds;
                }
                if let Some(replan_threshold) = replan_threshold {
                    config.replan_threshold = replan_threshold;
                }
                if let Some(max_autopilot_continues) = max_autopilot_continues {
                    config.max_autopilot_continues = Some(max_autopilot_continues);
                }
                if config.model_provider == ModelProvider::Llama
                    && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
                {
                    config.api_endpoint = DEFAULT_LLAMA_ENDPOINT.to_string();
                }
                if config.model_provider == ModelProvider::GithubModels
                    && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
                {
                    config.api_endpoint = DEFAULT_GITHUB_MODELS_ENDPOINT.to_string();
                }
                config.api_token = api_token
                    .clone()
                    .or(config.api_token)
                    .or_else(|| env::var("COPILOT_API_TOKEN").ok())
                    .or_else(|| env::var("GITHUB_TOKEN").ok());

                config
                    .notify_webhooks
                    .extend(notify_webhooks.iter().cloned());

                if github_issues {
                    config.github_issues_enabled = true;
                }
                if let Some(repo) = &github_repo {
                    config.github_repo = Some(repo.clone());
                }
                if let Some(token) = &github_token {
                    config.github_token = Some(token.clone());
                }
                if let Some(cost) = max_cost_usd {
                    config.max_cost_usd = Some(cost);
                }

                // Parse `KEY=PATH` pairs from --work-dir-map into the config map.
                for entry in &work_dir_map {
                    if let Some((key, path)) = entry.split_once('=') {
                        config.work_dirs.insert(key.to_string(), path.to_string());
                    } else {
                        eprintln!(
                            "Warning: ignoring malformed --work-dir-map entry '{}' \
                             (expected ROLE_OR_ID=PATH)",
                            entry
                        );
                    }
                }

                // CLI --prompt-dir overrides config and ralph-level prompt_dir.
                if let Some(ref pd) = prompt_dir {
                    config.prompt_dir = Some(pd.clone());
                } else if let Some(rc) = ralph_override {
                    if let Some(ref pd) = rc.prompt_dir {
                        config.prompt_dir = Some(pd.clone());
                    }
                }

                config
            };

            if ralph_list.is_empty() {
                // Single anonymous run (no --ralph).
                let config = build_config(None);
                save_user_config(&config)?;

                // Optional pre-loop planning phase.
                if let Some(ref goal_str) = goal {
                    println!("Generating task plan for goal: {}", goal_str);
                    let task_path = state_dir.join(&config.task_file);
                    if task_path.exists() {
                        println!(
                            "Warning: existing task file '{}' will be overwritten with the generated plan",
                            task_path.display()
                        );
                    }
                    let task_planner = planner::TaskPlanner::new(
                        config.model_provider.clone(),
                        config.api_endpoint.clone(),
                        config.api_token.clone(),
                    );
                    let planned_tasks = task_planner.generate_task_plan(goal_str).await?;
                    println!("Generated {} task(s)", planned_tasks.len());
                    task_manager::save_tasks(&task_path, &planned_tasks)?;
                    println!("Task plan written to {}", task_path.display());
                }

                if headless {
                    headless::run_headless(config, None).await?;
                } else {
                    let ralph_loop = RalphLoop::new(config);
                    let mut app = TuiApp::new(ralph_loop);
                    app.run().await?;
                }
            } else {
                // Run one or more named ralphs sequentially.
                for rc in &ralph_list {
                    println!("\n═══ Running ralph '{}' ═══", rc.name);
                    let config = build_config(Some(rc));
                    save_user_config(&config)?;

                    if rc.command.as_deref() == Some("unstuck") {
                        if let Err(e) = unstuck::run_unstuck(&config).await {
                            println!(
                                "[wreck-it] ralph '{}' (unstuck) failed: {}. Continuing…",
                                rc.name, e
                            );
                        }
                    } else if rc.command.as_deref() == Some("merge") {
                        if let Err(e) = merge::run_merge(&config, rc.backend.as_deref()).await {
                            println!(
                                "[wreck-it] ralph '{}' (merge) failed: {}. Continuing…",
                                rc.name, e
                            );
                        }
                    } else if headless {
                        if let Err(e) = headless::run_headless(config, Some(rc)).await {
                            println!("[wreck-it] ralph '{}' failed: {}. Continuing…", rc.name, e);
                        }
                    } else {
                        let ralph_loop = RalphLoop::new(config);
                        let mut app = TuiApp::new(ralph_loop);
                        app.run().await?;
                    }
                }
            }

            // Commit any pending state changes and push.
            let _ = state_worktree::commit_and_push_state(
                &resolved_work_dir,
                &repo_cfg.state_branch,
                "wreck-it: update state",
            );
        }

        Commands::Plan {
            goal,
            goal_file,
            ralph,
            output,
            api_endpoint,
            api_token,
            model_provider,
            cloud,
        } => {
            // Resolve goal: read from file, use raw string, or run wizard.
            let (goal, cloud, ralph) = match (goal, goal_file) {
                (Some(g), None) => (g, cloud, ralph),
                (None, Some(path)) => {
                    let g = std::fs::read_to_string(&path).with_context(|| {
                        format!("Failed to read goal file '{}'", path.display())
                    })?;
                    (g, cloud, ralph)
                }
                (None, None) => {
                    if is_interactive() {
                        match plan_wizard::run_plan_wizard()? {
                            Some(w) => (w.goal, w.cloud, w.ralph),
                            None => return Ok(()),
                        }
                    } else {
                        anyhow::bail!("Either --goal or --goal-file must be provided");
                    }
                }
                (Some(_), Some(_)) => {
                    // clap `conflicts_with` prevents this, but be defensive.
                    anyhow::bail!("--goal and --goal-file are mutually exclusive");
                }
            };
            let goal = goal.trim().to_string();

            let work_dir = std::env::current_dir()?;

            // Load (or fail) the repo config — plan needs a repo.
            let mut repo_cfg = load_repo_config(&work_dir)?
                .context("No wreck-it config found. Run `wreck-it init` first.")?;

            if cloud {
                // ── Cloud plan path ─────────────────────────────────────
                // Resolve a GitHub token (env → config → OAuth device flow).
                let mut config = load_user_config().unwrap_or_default();
                let github_token =
                    github_auth::resolve_github_token(config.github_token.as_deref()).await?;

                // Persist the token for future runs.
                if config.github_token.as_deref() != Some(&github_token) {
                    config.github_token = Some(github_token.clone());
                    save_user_config(&config)?;
                }

                // Derive ralph name from flag or slugify the goal.
                let ralph_name = ralph.unwrap_or_else(|| slugify_for_ralph(&goal));

                // Derive the target task filename.
                let task_filename = output
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("{}-tasks.json", ralph_name));

                // Plan file in .wreck-it/plans/ uses the targeted routing
                // convention so the headless runner merges it into the right
                // task file.
                let plan_filename = format!("{}--cloud-plan.json", task_filename);

                // Resolve repo owner/name from the git remote.
                let (repo_owner, repo_name) =
                    cloud_agent::resolve_repo_info(None, None, &work_dir)?;

                // Build the issue.
                let issue_title = format!("[wreck-it] plan: {}", truncate_title(&goal, 60));
                let issue_body = github_auth::build_plan_issue_body(&goal, &plan_filename);

                // Create the issue and assign a cloud agent.
                let client =
                    cloud_agent::CloudAgentClient::new(github_token, repo_owner, repo_name);
                let result = client.create_plan_issue(&issue_title, &issue_body).await?;

                println!(
                    "Created issue #{}: {}",
                    result.issue_number, result.issue_url
                );

                // Upsert the ralph entry in repo config so the headless
                // runner knows where to route the migrated plan.
                let state_filename = format!(".{}-state.json", ralph_name);
                if let Some(existing) = repo_cfg.ralphs.iter_mut().find(|r| r.name == ralph_name) {
                    existing.task_file = task_filename.clone();
                    existing.state_file = state_filename.clone();
                    println!("Updated ralph '{}' in config", ralph_name);
                } else {
                    repo_cfg.ralphs.push(repo_config::RalphConfig {
                        name: ralph_name.clone(),
                        task_file: task_filename.clone(),
                        state_file: state_filename.clone(),
                        branch: None,
                        agent: None,
                        reviewers: None,
                        command: None,
                        brute_mode: None,
                        backend: None,

                        prompt_dir: None,
                    });
                    println!("Added ralph '{}' to config", ralph_name);
                }
                save_repo_config(&work_dir, &repo_cfg)?;

                println!(
                    "Cloud agent will generate the plan and write it to .wreck-it/plans/{}",
                    plan_filename,
                );
                println!(
                    "Run `wreck-it run --headless --ralph {}` to migrate and execute the plan.",
                    ralph_name,
                );
            } else {
                // ── Local LLM plan path (existing behaviour) ────────────
                // Ensure the state worktree exists.
                let state_dir =
                    state_worktree::ensure_state_worktree(&work_dir, &repo_cfg.state_branch)?;

                // Build planner config.
                let mut config = load_user_config().unwrap_or_default();

                if let Some(api_endpoint) = api_endpoint {
                    config.api_endpoint = api_endpoint;
                }
                if let Some(model_provider) = model_provider {
                    config.model_provider = model_provider;
                }
                if config.model_provider == ModelProvider::Llama
                    && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
                {
                    config.api_endpoint = DEFAULT_LLAMA_ENDPOINT.to_string();
                }
                if config.model_provider == ModelProvider::GithubModels
                    && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
                {
                    config.api_endpoint = DEFAULT_GITHUB_MODELS_ENDPOINT.to_string();
                }
                config.api_token = api_token
                    .or(config.api_token)
                    .or_else(|| env::var("COPILOT_API_TOKEN").ok())
                    .or_else(|| env::var("GITHUB_TOKEN").ok());

                let task_planner = planner::TaskPlanner::new(
                    config.model_provider.clone(),
                    config.api_endpoint.clone(),
                    config.api_token.clone(),
                );

                // Derive a ralph name from the explicit flag, or ask the LLM,
                // falling back to a simple slug of the goal.
                let ralph_name = if let Some(r) = ralph {
                    r
                } else {
                    match task_planner.generate_plan_name(&goal).await {
                        Ok(name) => {
                            println!("LLM suggested plan name: {}", name);
                            name
                        }
                        Err(e) => {
                            tracing::warn!("LLM naming failed, falling back to slug: {}", e);
                            slugify_for_ralph(&goal)
                        }
                    }
                };

                // Derive the task filename (explicit --output wins, else
                // `<ralph>-tasks.json`).
                let task_filename = output
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("{}-tasks.json", ralph_name));

                let task_path = state_dir.join(&task_filename);

                println!("Generating task plan for goal: {}", goal);
                let tasks = task_planner.generate_task_plan(&goal).await?;
                println!("Generated {} task(s)", tasks.len());

                task_manager::save_tasks(&task_path, &tasks)?;
                println!("Task plan written to {}", task_path.display());

                // Upsert the ralph entry in the repo config.
                let state_filename = format!(".{}-state.json", ralph_name);
                if let Some(existing) = repo_cfg.ralphs.iter_mut().find(|r| r.name == ralph_name) {
                    existing.task_file = task_filename.clone();
                    existing.state_file = state_filename.clone();
                    println!("Updated ralph '{}' in config", ralph_name);
                } else {
                    repo_cfg.ralphs.push(repo_config::RalphConfig {
                        name: ralph_name.clone(),
                        task_file: task_filename.clone(),
                        state_file: state_filename.clone(),
                        branch: None,
                        agent: None,
                        reviewers: None,
                        command: None,
                        brute_mode: None,
                        backend: None,

                        prompt_dir: None,
                    });
                    println!("Added ralph '{}' to config", ralph_name);
                }
                save_repo_config(&work_dir, &repo_cfg)?;

                // Commit changes to the state branch.
                if let Ok(true) = state_worktree::commit_state_worktree(
                    &work_dir,
                    &format!("wreck-it: plan '{}' → ralph '{}'", goal, ralph_name),
                ) {
                    println!("Committed plan to state branch '{}'", repo_cfg.state_branch,);
                }
            }
        }

        Commands::Init { output } => {
            let work_dir = std::env::current_dir()?;
            let interactive = is_interactive();

            // ── Phase 1: Repo-level config ──────────────────────────────
            let repo_cfg = match load_repo_config(&work_dir)? {
                Some(cfg) => {
                    println!("Found existing wreck-it configuration");
                    cfg
                }
                None => {
                    let cfg = if interactive {
                        let branch = prompt_with_default(
                            "State branch",
                            state_worktree::DEFAULT_STATE_BRANCH,
                        );
                        let root =
                            prompt_with_default("State root directory", repo_config::CONFIG_DIR);
                        RepoConfig {
                            state_branch: branch,
                            task_branch: None,
                            state_root: root,
                            ralphs: vec![],
                        }
                    } else {
                        RepoConfig::default()
                    };

                    save_repo_config(&work_dir, &cfg)?;

                    // Commit the config to the current branch.
                    let cfg_path = repo_config_path(&work_dir);
                    let cfg_path_str = cfg_path
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("config path contains invalid UTF-8"))?;
                    state_worktree::git_cmd(&work_dir, &["add", cfg_path_str])?;
                    state_worktree::git_cmd(
                        &work_dir,
                        &["commit", "-m", "Initialize wreck-it configuration"],
                    )?;

                    println!(
                        "Initialized wreck-it configuration (branch='{}', root='{}')",
                        cfg.state_branch, cfg.state_root,
                    );
                    cfg
                }
            };

            // ── Phase 2: State worktree ─────────────────────────────────
            let state_dir =
                state_worktree::ensure_state_worktree(&work_dir, &repo_cfg.state_branch)?;

            println!(
                "State worktree at {} (branch '{}')",
                state_dir.display(),
                repo_cfg.state_branch,
            );

            // ── Phase 3: Task creation ──────────────────────────────────
            let task_path = state_dir.join(&output);

            // If tasks already exist, we are already initialized.
            if task_path.exists() {
                let tasks = task_manager::load_tasks(&task_path)?;
                println!("wreck-it is initialized");
                println!(
                    "  config: state_branch={}, state_root={}",
                    repo_cfg.state_branch, repo_cfg.state_root,
                );
                println!("  tasks: {} task(s) in {}", tasks.len(), output.display());
                return Ok(());
            }

            // Non-interactive: don't create tasks (empty state is fine).
            if !interactive {
                return Ok(());
            }

            // Interactive: create sample tasks.
            let sample_tasks = vec![
                Task {
                    id: "1".to_string(),
                    description: "First task - implement feature X".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    timeout_seconds: None,
                    max_retries: None,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                    precondition_prompt: None,
                    parent_id: None,
                    labels: vec![],
                    system_prompt_override: None,
                },
                Task {
                    id: "2".to_string(),
                    description: "Second task - add tests for feature X".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    timeout_seconds: None,
                    max_retries: None,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                    precondition_prompt: None,
                    parent_id: None,
                    labels: vec![],
                    system_prompt_override: None,
                },
                Task {
                    id: "3".to_string(),
                    description: "Third task - update documentation".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase: 2,
                    depends_on: vec!["1".to_string(), "2".to_string()],
                    priority: 0,
                    complexity: 1,
                    timeout_seconds: None,
                    max_retries: None,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                    precondition_prompt: None,
                    parent_id: None,
                    labels: vec![],
                    system_prompt_override: None,
                },
            ];

            task_manager::save_tasks(&task_path, &sample_tasks)?;
            println!("Created sample task file at: {}", task_path.display());

            // Write a default .wreck-it.toml config into the state worktree.
            let config_path = state_dir.join(".wreck-it.toml");
            if !config_path.exists() {
                let default_toml = format!(
                    "# wreck-it headless configuration\n\
                     # This file lives on the state branch ({}).\n\
                     \n\
                     task_file = \"{}\"\n\
                     state_file = \".wreck-it-state.json\"\n\
                     max_iterations = 100\n",
                    repo_cfg.state_branch,
                    output.display(),
                );
                std::fs::write(&config_path, default_toml)?;
                println!("Created default config at: {}", config_path.display());
            }

            // Commit everything into the state branch.
            if let Ok(true) =
                state_worktree::commit_state_worktree(&work_dir, "wreck-it: init state")
            {
                println!(
                    "Committed initial state to branch '{}'",
                    repo_cfg.state_branch,
                );
            }
        }

        Commands::Provenance { task, work_dir } => {
            let resolved_work_dir =
                work_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let records = provenance::load_provenance_records(&task, &resolved_work_dir)?;
            if records.is_empty() {
                println!("No provenance records found for task '{}'.", task);
            } else {
                println!("Provenance records for task '{}':", task);
                for record in &records {
                    println!("  timestamp : {}", record.timestamp);
                    println!("  outcome   : {}", record.outcome);
                    println!("  model     : {}", record.model);
                    println!("  agent_role: {:?}", record.agent_role);
                    println!("  prompt_hash: {}", record.prompt_hash);
                    println!("  diff_hash : {}", record.git_diff_hash);
                    if !record.tool_calls.is_empty() {
                        println!("  tool_calls: {}", record.tool_calls.join(", "));
                    }
                    println!();
                }
            }
        }

        Commands::Template { action } => match action {
            cli::TemplateAction::List => {
                let templates = templates::builtin_templates();
                if templates.is_empty() {
                    println!("No built-in templates available.");
                } else {
                    println!("Available templates:\n");
                    for tmpl in &templates {
                        println!("  {}", tmpl.manifest.name);
                        println!("    {}", tmpl.manifest.description);
                        if !tmpl.manifest.ralphs.is_empty() {
                            let names: Vec<&str> = tmpl
                                .manifest
                                .ralphs
                                .iter()
                                .map(|r| r.name.as_str())
                                .collect();
                            println!("    ralphs: {}", names.join(", "));
                        }
                        println!();
                    }
                }
            }
            cli::TemplateAction::Apply { name } => {
                let tmpl = templates::find_template(&name)
                    .ok_or_else(|| anyhow::anyhow!("Unknown template: '{}'", name))?;

                let work_dir = std::env::current_dir()?;

                // Ensure we have a repo config (create default if missing).
                let mut repo_cfg = load_repo_config(&work_dir)?.unwrap_or_default();

                // Ensure the state worktree exists.
                let state_dir =
                    state_worktree::ensure_state_worktree(&work_dir, &repo_cfg.state_branch)?;

                // Apply the template.
                let result = templates::apply_template(&tmpl, &state_dir, &mut repo_cfg)?;

                // Persist the updated config.
                save_repo_config(&work_dir, &repo_cfg)?;

                // Report what happened.
                if !result.written.is_empty() {
                    println!("Wrote task files:");
                    for f in &result.written {
                        println!("  {}", f);
                    }
                }
                if !result.skipped.is_empty() {
                    println!("Skipped (already exist):");
                    for f in &result.skipped {
                        println!("  {}", f);
                    }
                }
                if !result.ralphs_added.is_empty() {
                    println!("Added ralph contexts:");
                    for r in &result.ralphs_added {
                        println!("  {}", r);
                    }
                }
                println!("\nTemplate '{}' applied successfully.", tmpl.manifest.name,);

                // Commit state worktree changes.
                if let Ok(true) = state_worktree::commit_state_worktree(
                    &work_dir,
                    &format!("wreck-it: apply template '{}'", name),
                ) {
                    println!(
                        "Committed template state to branch '{}'",
                        repo_cfg.state_branch,
                    );
                }
            }
        },

        Commands::ExportOpenclaw {
            task_file,
            work_dir,
            workflow_name,
            output,
        } => {
            let resolved_work_dir =
                work_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let doc = openclaw::build_document(&task_file, &resolved_work_dir, &workflow_name)?;
            let json = openclaw::serialise_document(&doc)?;
            match output {
                Some(path) => {
                    std::fs::write(&path, &json).with_context(|| {
                        format!("Failed to write openclaw export to {}", path.display())
                    })?;
                    println!("Openclaw export written to {}", path.display());
                }
                None => println!("{}", json),
            }
        }

        Commands::Install { work_dir } => {
            let target = work_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let result = install::install(&target)?;

            if !result.written.is_empty() {
                println!("Created:");
                for f in &result.written {
                    println!("  {}", f);
                }
            }
            if !result.skipped.is_empty() {
                println!("Skipped (already exist):");
                for f in &result.skipped {
                    println!("  {}", f);
                }
            }
            if !result.ralphs_added.is_empty() {
                println!("Added ralph contexts:");
                for r in &result.ralphs_added {
                    println!("  {}", r);
                }
            }
            if result.written.is_empty() && result.ralphs_added.is_empty() {
                println!("wreck-it is already installed.");
            } else {
                println!(
                    "\nInstallation complete! Configure PAT_TOKEN secret and enable workflows."
                );
            }
        }

        Commands::Unstuck { work_dir } => {
            let resolved_work_dir =
                work_dir.unwrap_or_else(|| env::current_dir().unwrap_or_default());

            let mut config = load_user_config().unwrap_or_default();
            config.work_dir = resolved_work_dir;
            config.api_token = config.api_token.or_else(|| env::var("GITHUB_TOKEN").ok());

            unstuck::run_unstuck(&config).await?;
        }

        Commands::Merge { work_dir, backend } => {
            let resolved_work_dir =
                work_dir.unwrap_or_else(|| env::current_dir().unwrap_or_default());

            let mut config = load_user_config().unwrap_or_default();
            config.work_dir = resolved_work_dir;
            config.api_token = config.api_token.or_else(|| env::var("GITHUB_TOKEN").ok());

            merge::run_merge(&config, Some(&backend)).await?;
        }

        Commands::Graph {
            task_file,
            format,
            output,
        } => {
            let tasks = task_manager::load_tasks(&task_file)
                .with_context(|| format!("Failed to load task file: {}", task_file.display()))?;
            // Warn about circular dependencies before rendering.
            let cycles = graph::detect_cycles(&tasks);
            for cycle in &cycles {
                tracing::warn!("Circular dependency detected: {}", cycle.join(" -> "));
            }
            let content = match format {
                graph::GraphFormat::Mermaid => graph::generate_mermaid(&tasks),
                graph::GraphFormat::Dot => graph::generate_dot(&tasks),
            };
            match output {
                Some(path) => {
                    std::fs::write(&path, &content)
                        .with_context(|| format!("Failed to write graph to {}", path.display()))?;
                    println!("Graph written to {}", path.display());
                }
                None => print!("{content}"),
            }
        }

        Commands::Tasks { action } => match action {
            // ── tasks list ───────────────────────────────────────────────
            TasksAction::List { task_file, status } => {
                let tasks = task_manager::load_tasks(&task_file).with_context(|| {
                    format!("Failed to load task file: {}", task_file.display())
                })?;

                let filtered = task_cli::filter_tasks_by_status(&tasks, status);

                if filtered.is_empty() {
                    println!("No tasks found.");
                } else {
                    // Column widths (minimum header width, grow to content).
                    let id_w = filtered
                        .iter()
                        .map(|t| t.id.len())
                        .max()
                        .unwrap_or(2)
                        .max(2);
                    let status_w = 11; // "in-progress"
                    let role_w = 11; // "implementer"

                    println!(
                        "{:<id_w$}  {:<status_w$}  {:<role_w$}  {:>5}  {:>8}  DEPENDS_ON",
                        "ID",
                        "STATUS",
                        "ROLE",
                        "PHASE",
                        "PRIORITY",
                        id_w = id_w,
                        status_w = status_w,
                        role_w = role_w,
                    );
                    println!("{}", "-".repeat(id_w + status_w + role_w + 30));
                    for t in &filtered {
                        println!("{}", task_cli::format_task_row(t, id_w, status_w, role_w));
                    }
                    println!("\n{} task(s) listed.", filtered.len());
                }
            }

            // ── tasks add ────────────────────────────────────────────────
            TasksAction::Add {
                task_file,
                id,
                description,
                role,
                phase,
                priority,
                depends_on,
            } => {
                let new_task = Task {
                    id: id.clone(),
                    description,
                    status: types::TaskStatus::Pending,
                    role,
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase,
                    depends_on,
                    priority,
                    complexity: 1,
                    timeout_seconds: None,
                    max_retries: None,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                    precondition_prompt: None,
                    parent_id: None,
                    labels: vec![],
                    system_prompt_override: None,
                };
                task_manager::append_task(&task_file, new_task)?;
                println!("Task '{}' added to {}.", id, task_file.display());
            }

            // ── tasks set-status ─────────────────────────────────────────
            TasksAction::SetStatus {
                task_file,
                id,
                status,
            } => {
                task_manager::set_task_status(&task_file, &id, status)?;
                println!("Task '{}' status updated.", id);
            }

            // ── tasks validate ───────────────────────────────────────────
            TasksAction::Validate { task_file } => {
                let tasks = task_manager::load_tasks(&task_file).with_context(|| {
                    format!("Failed to load task file: {}", task_file.display())
                })?;

                let issues = task_manager::validate_tasks(&tasks);

                if issues.is_empty() {
                    println!(
                        "Task file '{}' is valid ({} task(s)).",
                        task_file.display(),
                        tasks.len()
                    );
                } else {
                    for issue in &issues {
                        eprintln!("error: {}", issue);
                    }
                    anyhow::bail!(
                        "{} validation error(s) found in '{}'",
                        issues.len(),
                        task_file.display()
                    );
                }
            }
        },
    }

    Ok(())
}

/// Convert an arbitrary goal string into a short, filesystem-safe ralph name.
///
/// Takes the first few words, lowercases, replaces non-alphanumeric chars with
/// hyphens, collapses runs of hyphens, and truncates to a reasonable length.
fn slugify_for_ralph(goal: &str) -> String {
    let slug: String = goal
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens, trim leading/trailing hyphens.
    let collapsed: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6) // first ~6 words
        .collect::<Vec<_>>()
        .join("-");

    // Cap length.
    let max_len = 40;
    if collapsed.len() > max_len {
        collapsed[..max_len].trim_end_matches('-').to_string()
    } else if collapsed.is_empty() {
        "plan".to_string()
    } else {
        collapsed
    }
}

/// Truncate a string to at most `max_len` characters, appending an ellipsis
/// when truncation occurs.  Breaks at the last whitespace before `max_len` to
/// avoid cutting words in half.
fn truncate_title(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        return s.to_string();
    }
    match s[..max_len].rfind(char::is_whitespace) {
        Some(pos) => format!("{}…", &s[..pos]),
        None => format!("{}…", &s[..max_len]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- slugify_for_ralph tests ----

    #[test]
    fn slugify_basic_sentence() {
        assert_eq!(slugify_for_ralph("Build a REST API"), "build-a-rest-api");
    }

    #[test]
    fn slugify_empty_string() {
        assert_eq!(slugify_for_ralph(""), "plan");
    }

    #[test]
    fn slugify_special_chars() {
        assert_eq!(slugify_for_ralph("Hello, World!"), "hello-world");
    }

    #[test]
    fn slugify_truncates_long_goal() {
        let goal = "word1 word2 word3 word4 word5 word6 word7 word8";
        let result = slugify_for_ralph(goal);
        // Takes first 6 words
        assert_eq!(result, "word1-word2-word3-word4-word5-word6");
    }

    // ---- goal-file resolution tests ----

    #[test]
    fn goal_file_contents_are_read_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("goal.txt");
        std::fs::write(&path, "  Build a pipeline  \n").unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.trim(), "Build a pipeline");
    }

    // ---- truncate_title tests ----

    #[test]
    fn truncate_short_title_unchanged() {
        assert_eq!(truncate_title("short goal", 60), "short goal");
    }

    #[test]
    fn truncate_long_title_at_word_boundary() {
        let long = "Build a comprehensive REST API for user management and authentication";
        let result = truncate_title(long, 40);
        assert!(result.len() <= 42); // 40 + ellipsis
        assert!(result.ends_with('…'));
        // Should break at a word boundary
        assert!(!result.contains("auth"));
    }

    #[test]
    fn truncate_exact_length_unchanged() {
        let s = "exactly sixty characters long string that is exactly sixty!!";
        let result = truncate_title(s, 60);
        assert_eq!(result, s);
    }
}
