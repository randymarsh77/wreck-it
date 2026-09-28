use wreck_it_core::routing::*;

fn account(id: &str) -> Account {
    Account {
        id: id.into(),
        owner: "owner".into(),
        harness: Harness::Codex,
        auth: AuthMode::NativeSubscription,
        credential_ref: format!("runner:{id}"),
        enabled: true,
        models: vec![ModelOption {
            model: "configured-model".into(),
            capabilities: vec!["repair".into()],
        }],
        max_concurrent_sessions: 1,
    }
}
fn usage(id: &str, remaining: u16, reset: u64) -> AccountUsage {
    AccountUsage {
        account_id: id.into(),
        observed_at: 100,
        windows: vec![UsageWindow {
            remaining_basis_points: remaining,
            resets_at: reset,
        }],
        active_sessions: 0,
        blocked_until: None,
        requires_reauthentication: false,
    }
}
fn request() -> RouteRequest {
    RouteRequest {
        owner: "owner".into(),
        capability: "repair".into(),
        minimum_remaining_basis_points: 500,
        max_usage_age_seconds: 60,
    }
}
fn excluded(account: Account, usage: AccountUsage, expected: ExclusionReason) {
    let decision = route(&[account], &[usage], &request(), 100).unwrap();
    assert!(decision.selection.is_none());
    assert_eq!(decision.excluded[0].reason, expected);
}

#[test]
fn ranks_capacity_and_reset_together_with_stable_ties() {
    let accounts = [account("a"), account("b")];
    for (a, b, expected) in [
        (usage("a", 9000, 200), usage("b", 6000, 200), "a"),
        (usage("a", 9000, 1100), usage("b", 6000, 200), "b"),
        (usage("a", 6000, 200), usage("b", 6000, 200), "a"),
    ] {
        for ordered in [accounts.to_vec(), accounts.iter().rev().cloned().collect()] {
            assert_eq!(
                route(&ordered, &[a.clone(), b.clone()], &request(), 100)
                    .unwrap()
                    .selection
                    .unwrap()
                    .account_id,
                expected
            );
        }
    }
}

#[test]
fn admission_precedes_capacity_scoring() {
    let mut other = account("other");
    other.owner = "someone-else".into();
    excluded(
        other,
        usage("other", 10000, 101),
        ExclusionReason::DifferentOwner,
    );
    let mut incapable = account("a");
    incapable.models[0].capabilities = vec!["triage".into()];
    excluded(
        incapable,
        usage("a", 10000, 101),
        ExclusionReason::NoCapableModel,
    );
    let mut disabled = account("a");
    disabled.enabled = false;
    excluded(disabled, usage("a", 10000, 101), ExclusionReason::Disabled);
}

#[test]
fn never_brokers_claude_subscription_credentials() {
    let mut claude = account("a");
    claude.harness = Harness::ClaudeCode;
    excluded(
        claude.clone(),
        usage("a", 10000, 101),
        ExclusionReason::UnsupportedAuthentication,
    );
    claude.auth = AuthMode::ApiKey;
    assert!(route(&[claude], &[usage("a", 10000, 101)], &request(), 100)
        .unwrap()
        .selection
        .is_some());
    let mut copilot = account("a");
    copilot.harness = Harness::Copilot;
    copilot.auth = AuthMode::CopilotToken;
    assert!(
        route(&[copilot], &[usage("a", 10000, 101)], &request(), 100)
            .unwrap()
            .selection
            .is_some()
    );
}

#[test]
fn every_window_must_have_capacity_and_a_fresh_reset() {
    let mut snapshot = usage("a", 9000, 200);
    snapshot.windows.push(UsageWindow {
        remaining_basis_points: 0,
        resets_at: 1000,
    });
    excluded(account("a"), snapshot.clone(), ExclusionReason::Exhausted);
    snapshot.windows[1].remaining_basis_points = 499;
    excluded(account("a"), snapshot.clone(), ExclusionReason::Exhausted);
    snapshot.windows[1].remaining_basis_points = 500;
    let chosen = route(&[account("a")], &[snapshot.clone()], &request(), 100)
        .unwrap()
        .selection
        .unwrap();
    assert_eq!(
        (chosen.remaining_basis_points, chosen.resets_at),
        (500, 1000)
    );
    snapshot.windows[1].resets_at = 100;
    excluded(account("a"), snapshot, ExclusionReason::MissingOrStaleUsage);
}

#[test]
fn unknown_stale_future_and_invalid_usage_are_not_unlimited() {
    assert!(route(&[account("a")], &[], &request(), 100)
        .unwrap()
        .selection
        .is_none());
    for observed_at in [0, 101] {
        let mut snapshot = usage("a", 9000, 200);
        snapshot.observed_at = observed_at;
        excluded(account("a"), snapshot, ExclusionReason::MissingOrStaleUsage);
    }
    let mut snapshot = usage("a", 9000, 200);
    snapshot.windows.clear();
    excluded(account("a"), snapshot, ExclusionReason::MissingOrStaleUsage);
    excluded(
        account("a"),
        usage("a", 10001, 200),
        ExclusionReason::InvalidUsage,
    );
}

#[test]
fn leases_reauthentication_and_cooldowns_block_admission() {
    let mut snapshot = usage("a", 9000, 200);
    snapshot.active_sessions = 1;
    excluded(
        account("a"),
        snapshot.clone(),
        ExclusionReason::AtConcurrencyLimit,
    );
    snapshot.active_sessions = 0;
    snapshot.requires_reauthentication = true;
    excluded(
        account("a"),
        snapshot.clone(),
        ExclusionReason::ReauthenticationRequired,
    );
    snapshot.requires_reauthentication = false;
    snapshot.blocked_until = Some(101);
    excluded(account("a"), snapshot.clone(), ExclusionReason::CoolingDown);
    snapshot.blocked_until = Some(100);
    assert!(route(&[account("a")], &[snapshot], &request(), 100)
        .unwrap()
        .selection
        .is_some());
}

#[test]
fn rejects_ambiguous_configuration_and_invalid_requests() {
    assert!(route(&[account("a"), account("a")], &[], &request(), 100).is_err());
    assert!(route(
        &[],
        &[usage("a", 1, 200), usage("a", 1, 200)],
        &request(),
        100
    )
    .is_err());
    let mut invalid = request();
    invalid.minimum_remaining_basis_points = 10001;
    assert!(route(&[], &[], &invalid, 100).is_err());
}

#[test]
fn zero_budget_is_exhausted_even_with_no_reserve() {
    let mut req = request();
    req.minimum_remaining_basis_points = 0;
    assert!(route(&[account("a")], &[usage("a", 0, 200)], &req, 100)
        .unwrap()
        .selection
        .is_none());
}

#[test]
fn committed_preview_fixture_is_valid_and_selects_expiring_capacity() {
    let input: serde_json::Value =
        serde_json::from_str(include_str!("../../examples/routing.json")).unwrap();
    let accounts = serde_json::from_value::<Vec<Account>>(input["accounts"].clone()).unwrap();
    let snapshots = serde_json::from_value::<Vec<AccountUsage>>(input["usage"].clone()).unwrap();
    let request = serde_json::from_value(input["request"].clone()).unwrap();
    let decision = route(
        &accounts,
        &snapshots,
        &request,
        input["now"].as_u64().unwrap(),
    )
    .unwrap();
    assert_eq!(decision.selection.unwrap().account_id, "copilot-personal");
}
