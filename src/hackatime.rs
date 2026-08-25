use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

use crate::{get_reqwest_client, keys::hackatime_token};
use anyhow::Result;
use log::{error, info};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
struct HackatimeEmailCacheEntry {
    email: String,
    id: u64,
    created_time: Instant,
}

#[derive(Clone)]
struct HackatimeUserCacheEntry {
    user: HackatimeUser,
    created_time: Instant,
}

const CACHE_EXPIRY_MINUTES: u64 = 30;

static HACKATIME_EMAIL_CACHE: OnceLock<Mutex<Vec<HackatimeEmailCacheEntry>>> = OnceLock::new();
static HACKATIME_USER_CACHE: OnceLock<Mutex<Vec<HackatimeUserCacheEntry>>> = OnceLock::new();

#[derive(Deserialize)]
struct IdResponse {
    user_id: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HackatimeUser {
    pub user_id: u64,
    pub username: String,
    pub projects: Vec<Project>,
    pub total_projects: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub name: String,
    pub total_heartbeats: u64,
    #[serde(rename = "total_duration")]
    pub total_duration_seconds: u64,
    pub languages: Vec<String>,
    pub repo: Option<String>,
    pub repo_mapping_id: Option<u64>,
    pub archived: bool,
}

pub async fn get_hackatime_id_from_email(email: String) -> Result<u64> {
    {
        let mut cache = HACKATIME_EMAIL_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES) && {
                if e.id == 0 {
                    error!("Rejecting invalid user ID in cache for email <{email}>");
                }
                e.id != 0
            }
        });
        if let Some(entry) = cache.iter().find(|e| e.email == email) {
            info!("Cache hit for email {email}");
            return Ok(entry.id);
        }
    }

    info!("Fetching Hackatime ID for email <{email}>");

    let client = get_reqwest_client();
    let request = client
        .post("https://hackatime.hackclub.com/api/admin/v1/user/get_user_by_email")
        .header("Content-Type", "application/json")
        .bearer_auth(hackatime_token())
        .json(&serde_json::json!({
            "email": email
        }));

    let response = request.send().await?.error_for_status()?;

    let parsed: IdResponse = response.json().await?;

    {
        let mut cache = HACKATIME_EMAIL_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        cache.push(HackatimeEmailCacheEntry {
            email,
            id: parsed.user_id,
            created_time: Instant::now(),
        });
    }

    Ok(parsed.user_id)
}

/// Do not expose this directly on an endpoint! Very bad!
pub async fn get_hackatime_user_data(hackatime_id: u64) -> Result<HackatimeUser> {
    {
        let mut cache = HACKATIME_USER_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES) && {
                if e.user.user_id == 0 {
                    error!("Rejecting invalid user ID in cache for ID <{hackatime_id}>");
                }
                e.user.user_id != 0
            }
        });
        if let Some(entry) = cache.iter().find(|e| e.user.user_id == hackatime_id) {
            return Ok(entry.user.clone());
        }
    }

    info!("Fetching Hackatime user data for ID <{hackatime_id}>");

    let client = get_reqwest_client();
    let request = client
        .get(format!(
            "https://hackatime.hackclub.com/api/admin/v1/user/projects?user_id={hackatime_id}"
        ))
        .bearer_auth(hackatime_token());

    let response = request.send().await?.error_for_status()?;
    let parsed: HackatimeUser = response.json().await?;

    {
        let mut cache = HACKATIME_USER_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(HackatimeUserCacheEntry {
            user: parsed.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(parsed)
}
