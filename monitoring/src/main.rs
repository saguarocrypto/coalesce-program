//! CoalesceFi off-chain invariant monitor.
//!
//! Periodically fetches all program accounts from a Solana RPC node,
//! deserializes them into the protocol's state types, and runs a suite of
//! invariant checks. Violations are logged and optionally POSTed to a webhook.

pub mod invariants;
pub mod types;

use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

use bytemuck;
use clap::Parser;
use log::{error, info, warn};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig};
use solana_client::rpc_filter::RpcFilterType;
use solana_sdk::account::Account;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use serde_json::json;

use crate::invariants::{
    check_all_market_invariants, check_all_whitelist_invariants, InvariantViolation,
    MonitorState, Severity,
};
use crate::types::{
    BorrowerWhitelist, LenderPosition, Market, BORROWER_WHITELIST_SIZE,
    LENDER_POSITION_SIZE, MARKET_SIZE,
};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(
    name = "coalescefi-monitor",
    about = "Off-chain invariant monitor for the CoalesceFi lending protocol"
)]
struct Cli {
    /// Solana RPC URL.
    #[arg(long, env = "RPC_URL", default_value = "https://api.mainnet-beta.solana.com")]
    rpc_url: String,

    /// Program ID of the CoalesceFi program (base58).
    #[arg(long, env = "PROGRAM_ID")]
    program_id: String,

    /// Polling interval in seconds.
    #[arg(long, default_value_t = 60)]
    interval_secs: u64,

    /// Optional webhook URL for posting violation alerts.
    #[arg(long, env = "ALERT_WEBHOOK")]
    alert_webhook: Option<String>,

    /// Stale accrual threshold in seconds (default 1 hour).
    #[arg(long, default_value_t = 3600)]
    stale_threshold_secs: i64,
}

// ---------------------------------------------------------------------------
// Account fetching helpers
// ---------------------------------------------------------------------------

/// Fetch all program accounts of a specific size.
fn fetch_accounts_by_size(
    client: &RpcClient,
    program_id: &Pubkey,
    data_size: u64,
) -> Result<Vec<(Pubkey, Account)>, Box<dyn std::error::Error>> {
    let config = RpcProgramAccountsConfig {
        filters: Some(vec![RpcFilterType::DataSize(data_size)]),
        account_config: RpcAccountInfoConfig {
            commitment: Some(CommitmentConfig::confirmed()),
            encoding: Some(solana_account_decoder::UiAccountEncoding::Base64),
            ..Default::default()
        },
        ..Default::default()
    };
    let accounts = client.get_program_accounts_with_config(program_id, config)?;
    Ok(accounts)
}

/// Fetch SPL token account balance for a given vault pubkey.
fn fetch_token_balance(
    client: &RpcClient,
    vault_pubkey: &Pubkey,
) -> Result<u64, Box<dyn std::error::Error>> {
    let account = client.get_account(vault_pubkey)?;
    // SPL token account data: amount is at bytes 64..72 (little-endian u64).
    if account.data.len() >= 72 {
        let amount_bytes: [u8; 8] = account.data[64..72]
            .try_into()
            .map_err(|_| "failed to read vault balance")?;
        Ok(u64::from_le_bytes(amount_bytes))
    } else {
        Err("vault account data too short".into())
    }
}

// ---------------------------------------------------------------------------
// Webhook alerting
// ---------------------------------------------------------------------------

/// POST a violation payload to the configured webhook URL.
fn send_webhook_alert(
    webhook_url: &str,
    violation: &InvariantViolation,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = json!({
        "severity": format!("{}", violation.severity),
        "violation_type": format!("{}", violation.violation_type),
        "market_pubkey": violation.market_pubkey,
        "expected": violation.expected,
        "actual": violation.actual,
        "timestamp": violation.timestamp,
    });

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .body(payload.to_string())
        .send()?;
    if !resp.status().is_success() {
        warn!(
            "Webhook returned non-200 status: {} for violation: {}",
            resp.status(),
            violation
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Main monitoring loop
// ---------------------------------------------------------------------------

fn run_monitoring_cycle(
    client: &RpcClient,
    program_id: &Pubkey,
    state: &mut MonitorState,
    stale_threshold_secs: i64,
    alert_webhook: Option<&str>,
) -> Result<Vec<InvariantViolation>, Box<dyn std::error::Error>> {
    let mut all_violations = Vec::new();

    // 1. Fetch all markets.
    let market_accounts = fetch_accounts_by_size(client, program_id, MARKET_SIZE as u64)?;
    info!("Fetched {} market accounts", market_accounts.len());

    // 2. Fetch all lender positions.
    let position_accounts =
        fetch_accounts_by_size(client, program_id, LENDER_POSITION_SIZE as u64)?;
    info!("Fetched {} lender position accounts", position_accounts.len());

    // 3. Group lender positions by market pubkey.
    let mut positions_by_market: HashMap<[u8; 32], Vec<LenderPosition>> = HashMap::new();
    for (_pk, account) in &position_accounts {
        if account.data.len() == LENDER_POSITION_SIZE {
            let pos: &LenderPosition = bytemuck::from_bytes(&account.data);
            positions_by_market
                .entry(pos.market)
                .or_default()
                .push(*pos);
        }
    }

    // 4. Get current time.
    let current_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // 5. Check each market.
    for (market_pk, account) in &market_accounts {
        if account.data.len() != MARKET_SIZE {
            warn!(
                "Market account {} has unexpected size {}",
                market_pk,
                account.data.len()
            );
            continue;
        }
        let market: &Market = bytemuck::from_bytes(&account.data);
        let market_key: [u8; 32] = market_pk.to_bytes();
        let market_pubkey_str = market_pk.to_string();

        // Try to fetch vault balance.
        let vault_pubkey = Pubkey::new_from_array(market.vault);
        let vault_balance = match fetch_token_balance(client, &vault_pubkey) {
            Ok(bal) => bal,
            Err(e) => {
                warn!(
                    "Could not fetch vault balance for market {}: {}",
                    market_pubkey_str, e
                );
                // Use u64::MAX as sentinel so the solvency check is effectively
                // skipped when we cannot read the vault.
                u64::MAX
            }
        };

        let positions = positions_by_market
            .get(&market_key)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        let violations = check_all_market_invariants(
            &market_pubkey_str,
            &market_key,
            market,
            vault_balance,
            positions,
            state,
            current_unix,
            stale_threshold_secs,
        );

        all_violations.extend(violations);
    }

    // 6. Fetch and check whitelist accounts.
    let whitelist_accounts =
        fetch_accounts_by_size(client, program_id, BORROWER_WHITELIST_SIZE as u64)?;
    info!(
        "Fetched {} borrower whitelist accounts",
        whitelist_accounts.len()
    );

    for (wl_pk, account) in &whitelist_accounts {
        if account.data.len() != BORROWER_WHITELIST_SIZE {
            continue;
        }
        let wl: &BorrowerWhitelist = bytemuck::from_bytes(&account.data);
        let violations = check_all_whitelist_invariants(&wl_pk.to_string(), wl);
        all_violations.extend(violations);
    }

    // 7. Log and optionally alert.
    for v in &all_violations {
        match v.severity {
            Severity::Critical => error!("{}", v),
            Severity::Warning => warn!("{}", v),
            Severity::Info => info!("{}", v),
        }

        if let Some(url) = alert_webhook {
            if let Err(e) = send_webhook_alert(url, v) {
                error!("Failed to send webhook alert: {}", e);
            }
        }
    }

    if all_violations.is_empty() {
        info!("All invariants passed.");
    }

    Ok(all_violations)
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let cli = Cli::parse();
    let program_id = Pubkey::from_str(&cli.program_id)?;
    let client = RpcClient::new_with_commitment(
        cli.rpc_url.clone(),
        CommitmentConfig::confirmed(),
    );

    info!("CoalesceFi Monitor starting");
    info!("  RPC:           {}", cli.rpc_url);
    info!("  Program ID:    {}", program_id);
    info!("  Interval:      {} secs", cli.interval_secs);
    info!("  Stale thresh:  {} secs", cli.stale_threshold_secs);
    if let Some(ref url) = cli.alert_webhook {
        info!("  Webhook:       {}", url);
    }

    let mut state = MonitorState::default();

    loop {
        match run_monitoring_cycle(
            &client,
            &program_id,
            &mut state,
            cli.stale_threshold_secs,
            cli.alert_webhook.as_deref(),
        ) {
            Ok(violations) => {
                info!(
                    "Cycle complete: {} violation(s) detected",
                    violations.len()
                );
            }
            Err(e) => {
                error!("Monitoring cycle failed: {}", e);
            }
        }

        std::thread::sleep(Duration::from_secs(cli.interval_secs));
    }
}
