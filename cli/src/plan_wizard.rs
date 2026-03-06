use crossterm::execute;
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use std::io::{self, Write};

/// Options collected by the interactive plan wizard.
#[derive(Debug, Clone)]
pub struct PlanWizardResult {
    /// The natural-language goal entered by the user.
    pub goal: String,
    /// Whether to use a cloud agent instead of the local LLM.
    pub cloud: bool,
    /// Optional ralph context name (empty → auto-derive from goal).
    pub ralph: Option<String>,
}

/// Run an interactive wizard that collects plan options from the user.
///
/// The wizard prompts for:
/// 1. A natural-language goal (required).
/// 2. Plan generation mode: local LLM vs cloud agent.
/// 3. An optional ralph context name.
///
/// Returns `None` if the user provides an empty goal (cancels).
pub fn run_plan_wizard() -> io::Result<Option<PlanWizardResult>> {
    let mut stdout = io::stdout();

    // ── Header ──────────────────────────────────────────────────────────
    execute!(
        stdout,
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        Print("\n  wreck-it plan wizard\n"),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("  ─────────────────────\n\n"),
    )?;

    // ── Step 1: Goal ────────────────────────────────────────────────────
    execute!(
        stdout,
        SetForegroundColor(Color::Green),
        Print("  Describe your goal:\n"),
        ResetColor,
    )?;
    print!("  > ");
    stdout.flush()?;

    let mut goal = String::new();
    io::stdin().read_line(&mut goal)?;
    let goal = goal.trim().to_string();

    if goal.is_empty() {
        println!("  No goal provided — aborting.");
        return Ok(None);
    }

    // ── Step 2: Generation mode ─────────────────────────────────────────
    println!();
    execute!(
        stdout,
        SetForegroundColor(Color::Green),
        Print("  How should the plan be generated?\n"),
        ResetColor,
    )?;
    println!("    [1] Local LLM (default)");
    println!("    [2] Cloud agent (creates GitHub issue)");
    print!("  > ");
    stdout.flush()?;

    let mut mode_input = String::new();
    io::stdin().read_line(&mut mode_input)?;
    let cloud = mode_input.trim() == "2";

    // ── Step 3: Ralph name ──────────────────────────────────────────────
    println!();
    execute!(
        stdout,
        SetForegroundColor(Color::Green),
        Print("  Ralph name (leave blank to auto-generate):\n"),
        ResetColor,
    )?;
    print!("  > ");
    stdout.flush()?;

    let mut ralph_input = String::new();
    io::stdin().read_line(&mut ralph_input)?;
    let ralph = {
        let trimmed = ralph_input.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };

    // ── Summary ─────────────────────────────────────────────────────────
    println!();
    execute!(
        stdout,
        SetForegroundColor(Color::Cyan),
        SetAttribute(Attribute::Bold),
        Print("  Plan summary\n"),
        SetAttribute(Attribute::Reset),
        ResetColor,
        Print("  ─────────────\n"),
    )?;
    println!("  Goal  : {}", goal);
    println!(
        "  Mode  : {}",
        if cloud { "cloud agent" } else { "local LLM" }
    );
    println!("  Ralph : {}", ralph.as_deref().unwrap_or("(auto)"));
    println!();

    Ok(Some(PlanWizardResult { goal, cloud, ralph }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_result_defaults() {
        let result = PlanWizardResult {
            goal: "Build a REST API".to_string(),
            cloud: false,
            ralph: None,
        };
        assert_eq!(result.goal, "Build a REST API");
        assert!(!result.cloud);
        assert!(result.ralph.is_none());
    }

    #[test]
    fn wizard_result_with_cloud_and_ralph() {
        let result = PlanWizardResult {
            goal: "Deploy infrastructure".to_string(),
            cloud: true,
            ralph: Some("infra-deploy".to_string()),
        };
        assert!(result.cloud);
        assert_eq!(result.ralph.as_deref(), Some("infra-deploy"));
    }

    #[test]
    fn wizard_result_clone() {
        let result = PlanWizardResult {
            goal: "Test".to_string(),
            cloud: false,
            ralph: Some("test-ralph".to_string()),
        };
        let cloned = result.clone();
        assert_eq!(cloned.goal, result.goal);
        assert_eq!(cloned.cloud, result.cloud);
        assert_eq!(cloned.ralph, result.ralph);
    }
}
