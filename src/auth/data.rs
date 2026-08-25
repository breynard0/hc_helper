use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

use actix_web::HttpRequest;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::auth::login::get_auth_token_with_handling;

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
        self.addresses.iter().find(|x| x.primary).map(|x| x.clone())
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

pub async fn get_auth_data(req: &HttpRequest) -> Option<AuthData> {
    let host = req.connection_info().host().to_string();
    log::info!("Getting auth data from {host}");
    let token = match get_auth_token_with_handling(&req) {
        Some(x) => x,
        None => return None,
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
            log::info!("Retrieving from HCA cache from {host}");
            return Some(cache_hit.data.clone());
        }
    }

    log::info!("No HCA cache hit, fetching from {host}");
    let client = Client::new();
    let response = client
        .get("https://auth.hackclub.com/api/v1/me")
        .bearer_auth(&token)
        .send()
        .await;
    let auth_data_raw = response.ok()?.error_for_status().ok()?.text().await.ok()?;
    log::info!("{}", auth_data_raw);
    let parsed: IdentityResponse = serde_json::from_str(&auth_data_raw).ok()?;
    log::debug!("granted scopes: {:?} from {host}", parsed.scopes);
    let auth_data = parsed.identity;

    log::info!("HCA data successfully retrieved from {host}");

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

    Some(auth_data)
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

pub async fn check_verified(req: &HttpRequest) -> Option<VerificationStatus> {
    let auth_data = get_auth_data(req).await?;

    match auth_data.verification_status.as_str() {
        "needs_submission" => Some(VerificationStatus::NeedsSubmission),
        "pending" => Some(VerificationStatus::Pending),
        "verified_eligible" | "verified" => Some(VerificationStatus::VerifiedEligible),
        "verified_but_over_18" => Some(VerificationStatus::VerifiedButOver18),
        "rejected" => Some(VerificationStatus::Rejected),
        "not_found" => Some(VerificationStatus::NotFound),
        _ => None,
    }
}
