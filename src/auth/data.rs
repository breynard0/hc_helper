use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

use actix_web::HttpRequest;
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{auth::login::get_auth_token_with_handling, client_ip};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct IdentityResponse {
    identity: AuthData,
    scopes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthData {
    pub id: String,
    pub ysws_eligible: bool,
    pub verification_status: String,
    pub first_name: String,
    pub last_name: String,
    pub primary_email: String,
    pub slack_id: String,
    pub phone_number: String,
    pub birthday: String,
    pub legal_first_name: String,
    pub legal_last_name: String,
    pub addresses: Vec<Address>,
}

impl AuthData {
    pub fn primary_address(&self) -> Option<Address> {
        self.addresses.iter().find(|x| x.primary).cloned()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Address {
    pub id: String,
    pub first_name: String,
    pub last_name: String,
    pub line_1: String,
    pub line_2: String,
    pub city: String,
    pub state: String,
    pub postal_code: String,
    pub country: String,
    pub phone_number: String,
    pub primary: bool,
}

struct AuthDataCacheEntry {
    token: String,
    data: AuthData,
    created_time: Instant,
}

static APP_DATA_CACHE: OnceLock<Mutex<Vec<AuthDataCacheEntry>>> = OnceLock::new();

const APP_DATA_EXPIRY_MINUTES: u64 = 10;

pub async fn get_auth_data(req: &HttpRequest) -> Result<AuthData> {
    let caller = client_ip(req);
    log::info!("Getting auth data from {caller}");
    let token = match get_auth_token_with_handling(req) {
        Some(x) => x,
        None => return Err(anyhow::anyhow!("No auth token")),
    };
    {
        let mut cache = APP_DATA_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        *cache = cache
            .drain(..)
            .filter(|e| e.created_time.elapsed().as_secs() < APP_DATA_EXPIRY_MINUTES * 60)
            .collect();

        if let Some(cache_hit) = cache.iter().find(|e| e.token == token) {
            log::info!("Retrieving from HCA cache from {caller}");
            return Ok(cache_hit.data.clone());
        }
    }

    log::info!("No HCA cache hit, fetching from {caller}");
    let client = reqwest::Client::new();
    let response = client
        .get("https://auth.hackclub.com/api/v1/me")
        .bearer_auth(&token)
        .send()
        .await;
    let auth_data_resp = response?.error_for_status()?;
    let parsed: IdentityResponse = auth_data_resp.json().await?;
    log::debug!("granted scopes: {:?} from {caller}", parsed.scopes);
    let auth_data = parsed.identity;

    log::info!("HCA data successfully retrieved from {caller}");

    {
        let mut cache = APP_DATA_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(AuthDataCacheEntry {
            token,
            data: auth_data.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(auth_data)
}

pub enum VerificationStatus {
    NeedsSubmission,
    Pending,
    VerifiedEligible,
    VerifiedButOver18,
    Rejected,
    NotFound,
}

impl VerificationStatus {
    pub fn is_verified(&self) -> bool {
        matches!(self, VerificationStatus::VerifiedEligible)
    }
}

pub async fn check_verified(req: &HttpRequest) -> Result<VerificationStatus> {
    let auth_data = get_auth_data(req).await?;

    match auth_data.verification_status.as_str() {
        "needs_submission" => Ok(VerificationStatus::NeedsSubmission),
        "pending" => Ok(VerificationStatus::Pending),
        "verified_eligible" | "verified" => Ok(VerificationStatus::VerifiedEligible),
        "verified_but_over_18" => Ok(VerificationStatus::VerifiedButOver18),
        "rejected" => Ok(VerificationStatus::Rejected),
        "not_found" => Ok(VerificationStatus::NotFound),
        _ => Err(anyhow::anyhow!(
            "Unknown verification status: {}",
            auth_data.verification_status
        )),
    }
}
