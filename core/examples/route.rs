//! Offline routing preview. Never loads secrets or launches a paid session.
use serde::Deserialize;
use wreck_it_core::routing::{route, Account, AccountUsage, RouteRequest};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    accounts: Vec<Account>,
    usage: Vec<AccountUsage>,
    request: RouteRequest,
    now: u64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: route <input.json>")?;
    let input: Input = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let decision = route(&input.accounts, &input.usage, &input.request, input.now)?;
    println!("{}", serde_json::to_string_pretty(&decision)?);
    Ok(())
}
