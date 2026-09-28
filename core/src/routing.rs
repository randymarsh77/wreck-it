//! Pure admission and routing policy for official CLI harnesses.
//!
//! This produces a proposal, not a reservation. A durable coordinator must
//! atomically recheck capacity and acquire an account lease before dispatch.
//! All timestamps are Unix seconds; usage is remaining basis points (0..10000).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Codex,
    ClaudeCode,
    Copilot,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Native Codex login maintained by the credential owner.
    NativeSubscription,
    ApiKey,
    CopilotToken,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelOption {
    pub model: String,
    /// Operator-defined capability classes, e.g. triage, repair, deep_repair.
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub id: String,
    pub owner: String,
    pub harness: Harness,
    pub auth: AuthMode,
    /// Opaque reference only. Native subscription auth stays in its runner.
    pub credential_ref: String,
    pub enabled: bool,
    pub models: Vec<ModelOption>,
    pub max_concurrent_sessions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UsageWindow {
    pub remaining_basis_points: u16,
    pub resets_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AccountUsage {
    pub account_id: String,
    pub observed_at: u64,
    /// Every applicable provider window must be present, including weekly caps.
    /// API accounts may use an explicit operator-managed spend budget window.
    pub windows: Vec<UsageWindow>,
    pub active_sessions: u32,
    pub blocked_until: Option<u64>,
    pub requires_reauthentication: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteRequest {
    pub owner: String,
    pub capability: String,
    pub minimum_remaining_basis_points: u16,
    pub max_usage_age_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub account_id: String,
    pub harness: Harness,
    pub model: String,
    pub credential_ref: String,
    pub remaining_basis_points: u16,
    pub resets_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    Disabled,
    DifferentOwner,
    UnsupportedAuthentication,
    NoCapableModel,
    MissingOrStaleUsage,
    InvalidUsage,
    Exhausted,
    AtConcurrencyLimit,
    ReauthenticationRequired,
    CoolingDown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Exclusion {
    pub account_id: String,
    pub reason: ExclusionReason,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteDecision {
    /// None means defer; it never means silently spend on another account.
    pub selection: Option<Selection>,
    pub excluded: Vec<Exclusion>,
}

/// Select within an owner's explicitly configured pool. This hosted router
/// deliberately excludes Claude subscription tokens: native, owner-operated
/// Claude Code sessions require a separate integration with no token brokerage.
pub fn route(
    accounts: &[Account],
    usage: &[AccountUsage],
    request: &RouteRequest,
    now: u64,
) -> Result<RouteDecision, String> {
    if request.owner.trim().is_empty()
        || request.capability.trim().is_empty()
        || request.minimum_remaining_basis_points > 10_000
        || request.max_usage_age_seconds == 0
    {
        return Err("invalid routing request".into());
    }
    let mut ids = std::collections::HashSet::new();
    for account in accounts {
        if account.id.trim().is_empty()
            || account.credential_ref.trim().is_empty()
            || !ids.insert(&account.id)
        {
            return Err("empty account/credential reference or duplicate account ID".into());
        }
    }
    ids.clear();
    if usage.iter().any(|u| !ids.insert(&u.account_id)) {
        return Err("duplicate account usage snapshot".into());
    }
    let mut decision = RouteDecision {
        selection: None,
        excluded: vec![],
    };
    for account in accounts {
        let candidate = eligible(account, usage, request, now);
        match candidate {
            Err(reason) => decision.excluded.push(Exclusion {
                account_id: account.id.clone(),
                reason,
            }),
            Ok(candidate) => {
                // Spend expiring headroom first: remaining fraction / seconds
                // until the limiting window resets. Cross-multiply in u128 to
                // avoid floating-point rounding and timestamp overflow.
                let better = decision.selection.as_ref().is_none_or(|current| {
                    let left = u128::from(candidate.remaining_basis_points)
                        * u128::from(current.resets_at - now);
                    let right = u128::from(current.remaining_basis_points)
                        * u128::from(candidate.resets_at - now);
                    left > right || (left == right && candidate.account_id < current.account_id)
                });
                if better {
                    decision.selection = Some(candidate);
                }
            }
        }
    }
    Ok(decision)
}

fn eligible(
    account: &Account,
    snapshots: &[AccountUsage],
    request: &RouteRequest,
    now: u64,
) -> Result<Selection, ExclusionReason> {
    use ExclusionReason::*;
    if !account.enabled {
        return Err(Disabled);
    }
    if account.owner != request.owner {
        return Err(DifferentOwner);
    }
    if !matches!(
        (account.harness, account.auth),
        (
            Harness::Codex,
            AuthMode::NativeSubscription | AuthMode::ApiKey
        ) | (Harness::ClaudeCode, AuthMode::ApiKey)
            | (Harness::Copilot, AuthMode::CopilotToken)
    ) {
        return Err(UnsupportedAuthentication);
    }
    // Configuration order expresses model preference within a capability class.
    let model = account
        .models
        .iter()
        .find(|m| !m.model.trim().is_empty() && m.capabilities.contains(&request.capability))
        .ok_or(NoCapableModel)?;
    let usage = snapshots
        .iter()
        .find(|u| u.account_id == account.id)
        .ok_or(MissingOrStaleUsage)?;
    if usage.requires_reauthentication {
        return Err(ReauthenticationRequired);
    }
    if usage.blocked_until.is_some_and(|until| until > now) {
        return Err(CoolingDown);
    }
    if usage.active_sessions >= account.max_concurrent_sessions {
        return Err(AtConcurrencyLimit);
    }
    if usage.observed_at > now
        || now - usage.observed_at > request.max_usage_age_seconds
        || usage.windows.is_empty()
        || usage.windows.iter().any(|w| w.resets_at <= now)
    {
        // A passed reset is not proof that the provider replenished capacity.
        return Err(MissingOrStaleUsage);
    }
    if usage
        .windows
        .iter()
        .any(|w| w.remaining_basis_points > 10_000)
    {
        return Err(InvalidUsage);
    }
    if usage.windows.iter().any(|w| {
        w.remaining_basis_points == 0
            || w.remaining_basis_points < request.minimum_remaining_basis_points
    }) {
        return Err(Exhausted);
    }
    let limiting = usage
        .windows
        .iter()
        .min_by_key(|w| (w.remaining_basis_points, std::cmp::Reverse(w.resets_at)))
        .unwrap();
    Ok(Selection {
        account_id: account.id.clone(),
        harness: account.harness,
        model: model.model.clone(),
        credential_ref: account.credential_ref.clone(),
        remaining_basis_points: limiting.remaining_basis_points,
        resets_at: limiting.resets_at,
    })
}
