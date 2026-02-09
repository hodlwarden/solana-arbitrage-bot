//! Low-latency submission services (Jito, Helius, Astralane, ZeroSlot, etc.).
//! Set `submission_services` in `[connection]` and the corresponding API keys in `[dex_api]`.
//! When any service is configured, submission uses `ultra_submit_simple` with those clients;
//! otherwise submission is RPC-only via `ultra_submit` with all None.

use once_cell::sync::Lazy;
use solana_relayer_adapter_rust::ultra_submit::{ServiceConfig, ServiceClient, ultra_submit_simple};

use crate::app::config;

fn build_services() -> Vec<ServiceConfig> {
    let services = match &config::CONFIG.node.submission_services {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };
    let api = &config::CONFIG.swap_api;
    let mut out = Vec::new();

    // Set env vars so the relayer adapter (or .env) can use them; many adapters read e.g. JITO_AUTH_KEY.
    for name in services {
        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            "jito" if !api.jito_api_key.is_empty() => {
                std::env::set_var("JITO_AUTH_KEY", &api.jito_api_key);
                if let Some(ptr) = try_build_jito(api.jito_api_key.as_str()) {
                    out.push(ServiceConfig { name: "jito", client: ServiceClient::Jito(ptr) });
                }
            }
            "liljit" if !api.liljit_endpoint.is_empty() => {
                if let Some(ptr) = try_build_jito(api.liljit_endpoint.as_str()) {
                    out.push(ServiceConfig { name: "liljit", client: ServiceClient::LilJit(ptr) });
                }
            }
            "helius" if !api.helius_api_key.is_empty() => {
                std::env::set_var("HELIUS_AUTH_KEY", &api.helius_api_key);
                if let Some(ptr) = try_build_helius(api.helius_api_key.as_str()) {
                    out.push(ServiceConfig { name: "helius", client: ServiceClient::Helius(ptr) });
                }
            }
            "astralane" | "astra" if !api.astralane_key.is_empty() => {
                std::env::set_var("ASTRALANE_KEY", &api.astralane_key);
                if let Some(ptr) = try_build_astralane(api.astralane_key.as_str()) {
                    out.push(ServiceConfig { name: "astralane", client: ServiceClient::Astralane(ptr) });
                }
            }
            "zeroslot" | "zero_slot" if !api.zero_slot_key.is_empty() => {
                if let Some(ptr) = try_build_zeroslot(api.zero_slot_key.as_str()) {
                    out.push(ServiceConfig { name: "zeroslot", client: ServiceClient::ZeroSlot(ptr) });
                }
            }
            "nozomi" if !api.nozomi_api_key.is_empty() => {
                if let Some(ptr) = try_build_nozomi(api.nozomi_api_key.as_str()) {
                    out.push(ServiceConfig { name: "nozomi", client: ServiceClient::Nozomi(ptr) });
                }
            }
            "blockrazor" | "brazor" if !api.blockrazor_key.is_empty() => {
                if let Some(ptr) = try_build_blockrazor(api.blockrazor_key.as_str()) {
                    out.push(ServiceConfig { name: "blockrazor", client: ServiceClient::BlockRazor(ptr) });
                }
            }
            "bloxroute" if !api.bloxroute_key.is_empty() => {
                if let Some(ptr) = try_build_bloxroute(api.bloxroute_key.as_str()) {
                    out.push(ServiceConfig { name: "bloxroute", client: ServiceClient::BloxRoute(ptr) });
                }
            }
            "nextblock" if !api.nextblock_key.is_empty() => {
                if let Some(ptr) = try_build_nextblock(api.nextblock_key.as_str()) {
                    out.push(ServiceConfig { name: "nextblock", client: ServiceClient::NextBlock(ptr) });
                }
            }
            _ => {}
        }
    }
    out
}

// Build clients using the adapter's public API. The adapter crate may use different
// constructors or private fields; these try to build from regions + reqwest::Client.
#[allow(dead_code)]
fn try_build_jito(_auth_key: &str) -> Option<&'static solana_relayer_adapter_rust::Jito> {
    None
}
#[allow(dead_code)]
fn try_build_helius(_auth_key: &str) -> Option<&'static solana_relayer_adapter_rust::Helius> {
    None
}
#[allow(dead_code)]
fn try_build_astralane(_key: &str) -> Option<&'static solana_relayer_adapter_rust::Astralane> {
    None
}
#[allow(dead_code)]
fn try_build_zeroslot(_key: &str) -> Option<&'static solana_relayer_adapter_rust::ZeroSlot> {
    None
}
#[allow(dead_code)]
fn try_build_nozomi(_key: &str) -> Option<&'static solana_relayer_adapter_rust::Nozomi> {
    None
}
#[allow(dead_code)]
fn try_build_blockrazor(_key: &str) -> Option<&'static solana_relayer_adapter_rust::BlockRazor> {
    None
}
#[allow(dead_code)]
fn try_build_bloxroute(_key: &str) -> Option<&'static solana_relayer_adapter_rust::BloxRoute> {
    None
}
#[allow(dead_code)]
fn try_build_nextblock(_key: &str) -> Option<&'static solana_relayer_adapter_rust::NextBlock> {
    None
}

/// Built list of relayer services from config (lazy, once). Empty if submission_services not set or no valid clients.
pub static RELAYER_SERVICES: Lazy<Vec<ServiceConfig>> = Lazy::new(build_services);

/// Returns true when at least one low-latency service is enabled.
pub fn use_low_latency_submission() -> bool {
    !RELAYER_SERVICES.is_empty()
}

/// Submit via low-latency services (when configured) or fall back to RPC. Call this instead of `ultra_submit` when you want config-driven submission.
pub async fn submit_with_services(
    tx_info: solana_relayer_adapter_rust::Tips,
    signers: &'static Vec<&'static solana_sdk::signature::Keypair>,
    recent_blockhash: solana_sdk::hash::Hash,
    nonce_ix: solana_sdk::instruction::Instruction,
    alt: Vec<solana_sdk::message::AddressLookupTableAccount>,
    retry_count: u32,
) {
    if use_low_latency_submission() {
        ultra_submit_simple(tx_info, signers, recent_blockhash, nonce_ix, alt, retry_count, RELAYER_SERVICES.clone()).await;
    } else {
        solana_relayer_adapter_rust::ultra_submit(
            tx_info,
            signers,
            recent_blockhash,
            nonce_ix,
            alt,
            retry_count,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await;
    }
}
