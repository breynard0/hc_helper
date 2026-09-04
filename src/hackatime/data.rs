use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

use actix_web::HttpRequest;
use anyhow::Result;
use log::error;
use serde::{Deserialize, Serialize};

use crate::{client_ip, hackatime::login::get_hackatime_token_with_handling};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HackatimeUser {
    #[serde(rename = "id")]
    pub user_id: u64,
    pub emails: Vec<String>,
    pub slack_id: Option<String>,
    pub github_username: Option<String>,
    pub trust_factor: TrustFactor,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrustFactor {
    pub trust_level: String,
    pub trust_value: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    pub name: String,
    #[serde(rename = "total_seconds")]
    pub total_duration_seconds: f64,
    pub total_heartbeats: u64,
    pub languages: Vec<String>,
    pub repo_url: Option<String>,
    pub first_heartbeat: Option<String>,
    pub last_heartbeat: Option<String>,
    pub most_recent_heartbeat: Option<String>,
    pub archived: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ProjectNamesResponse {
    projects: Vec<ProjectName>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct ProjectName {
    name: String,
}

struct HackatimeUserCacheEntry {
    token: String,
    user: HackatimeUser,
    created_time: Instant,
}

struct ProjectNamesCacheEntry {
    token: String,
    names: Vec<String>,
    created_time: Instant,
}

struct ProjectCacheEntry {
    token: String,
    name: String,
    project: Project,
    created_time: Instant,
}

static HACKATIME_USER_CACHE: OnceLock<Mutex<Vec<HackatimeUserCacheEntry>>> = OnceLock::new();
static HACKATIME_PROJECT_NAMES_CACHE: OnceLock<Mutex<Vec<ProjectNamesCacheEntry>>> =
    OnceLock::new();
static HACKATIME_PROJECT_CACHE: OnceLock<Mutex<Vec<ProjectCacheEntry>>> = OnceLock::new();

const CACHE_EXPIRY_MINUTES: u64 = 30;
/// Without this the API only looks back a year, which truncates `first_heartbeat`.
const PROJECT_STATS_START: &str = "2015-01-01";

fn token(req: &HttpRequest) -> Result<String> {
    match get_hackatime_token_with_handling(req) {
        Some(x) => Ok(x),
        None => Err(anyhow::anyhow!("No Hackatime token")),
    }
}

pub async fn get_hackatime_user(req: &HttpRequest) -> Result<HackatimeUser> {
    let caller = client_ip(req);
    let token = token(req)?;

    {
        let mut cache = HACKATIME_USER_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES) && {
                if e.user.user_id == 0 {
                    error!("Rejecting invalid user ID in cache from {caller}");
                }
                e.user.user_id != 0
            }
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token) {
            log::info!("Retrieving from Hackatime user cache from {caller}");
            return Ok(entry.user.clone());
        }
    }

    log::info!("No Hackatime user cache hit, fetching from {caller}");

    let client = reqwest::Client::new();
    let response = client
        .get("https://hackatime.hackclub.com/api/v1/authenticated/me")
        .bearer_auth(&token)
        .send()
        .await;
    let parsed: HackatimeUser = response?.error_for_status()?.json().await?;

    {
        let mut cache = HACKATIME_USER_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(HackatimeUserCacheEntry {
            token,
            user: parsed.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(parsed)
}

pub async fn get_hackatime_projects(req: &HttpRequest) -> Result<Vec<String>> {
    let caller = client_ip(req);
    let token = token(req)?;

    {
        let mut cache = HACKATIME_PROJECT_NAMES_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES)
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token) {
            log::info!("Retrieving from Hackatime project names cache from {caller}");
            return Ok(entry.names.clone());
        }
    }

    log::info!("No Hackatime project names cache hit, fetching from {caller}");

    let client = reqwest::Client::new();
    let response = client
        .get("https://hackatime.hackclub.com/api/v1/authenticated/projects?include_archived=true")
        .bearer_auth(&token)
        .send()
        .await;
    let parsed: ProjectNamesResponse = response?.error_for_status()?.json().await?;
    let names: Vec<String> = parsed.projects.into_iter().map(|p| p.name).collect();

    {
        let mut cache = HACKATIME_PROJECT_NAMES_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(ProjectNamesCacheEntry {
            token,
            names: names.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(names)
}

pub async fn get_hackatime_project(req: &HttpRequest, name: &str) -> Result<Project> {
    let caller = client_ip(req);
    let token = token(req)?;

    {
        let mut cache = HACKATIME_PROJECT_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES)
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token && e.name == name) {
            log::info!("Retrieving from Hackatime project cache from {caller}");
            return Ok(entry.project.clone());
        }
    }

    log::info!("No Hackatime project cache hit, fetching from {caller}");

    let mut url = reqwest::Url::parse("https://hackatime.hackclub.com/api/v1/users/my/project")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Cannot build Hackatime project URL"))?
        .push(name);
    url.query_pairs_mut()
        .append_pair("start_date", PROJECT_STATS_START);

    let client = reqwest::Client::new();
    let response = client.get(url).bearer_auth(&token).send().await;
    let parsed: Project = response?.error_for_status()?.json().await?;

    {
        let mut cache = HACKATIME_PROJECT_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(ProjectCacheEntry {
            token,
            name: name.to_string(),
            project: parsed.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(parsed)
}
