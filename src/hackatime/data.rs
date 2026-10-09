use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

use actix_web::HttpRequest;
use anyhow::Result;
use log::error;
use serde::{Deserialize, Serialize};

use crate::{client_ip, hackatime::login::get_hackatime_token_with_handling, http::CLIENT};

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
struct ProjectsResponse {
    projects: Vec<Project>,
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

struct ProjectSummariesCacheEntry {
    token: String,
    projects: Vec<Project>,
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
static HACKATIME_PROJECT_SUMMARIES_CACHE: OnceLock<Mutex<Vec<ProjectSummariesCacheEntry>>> =
    OnceLock::new();
static HACKATIME_PROJECT_CACHE: OnceLock<Mutex<Vec<ProjectCacheEntry>>> = OnceLock::new();

const CACHE_EXPIRY_MINUTES: u64 = 30;
const SUMMARIES_CACHE_EXPIRY_SECONDS: u64 = 60;
/// Without this the API only looks back a year, which truncates `first_heartbeat`.
const PROJECT_STATS_START: &str = "2015-01-01";

fn forget<T>(cache: &OnceLock<Mutex<Vec<T>>>, matches: impl Fn(&T) -> bool) {
    if let Some(cache) = cache.get() {
        cache
            .lock()
            .unwrap_or_else(|x| x.into_inner())
            .retain(|e| !matches(e));
    }
}

/// Drops every cached response for this token, so the next lookups hit Hackatime.
pub fn forget_hackatime_token(token: &str) {
    forget(&HACKATIME_USER_CACHE, |e| e.token == token);
    forget(&HACKATIME_PROJECT_NAMES_CACHE, |e| e.token == token);
    forget(&HACKATIME_PROJECT_SUMMARIES_CACHE, |e| e.token == token);
    forget(&HACKATIME_PROJECT_CACHE, |e| e.token == token);
}

fn token(req: &HttpRequest) -> Result<String> {
    match get_hackatime_token_with_handling(req) {
        Some(x) => Ok(x),
        None => Err(anyhow::anyhow!("No Hackatime token")),
    }
}

pub async fn get_hackatime_user(req: &HttpRequest) -> Result<HackatimeUser> {
    log::info!("Getting Hackatime user from {}", client_ip(req));
    get_hackatime_user_with_token(&token(req)?).await
}

pub async fn get_hackatime_user_with_token(token: &str) -> Result<HackatimeUser> {
    {
        let mut cache = HACKATIME_USER_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES) && {
                if e.user.user_id == 0 {
                    error!("Rejecting invalid user ID in cache");
                }
                e.user.user_id != 0
            }
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token) {
            log::info!("Retrieving from Hackatime user cache");
            return Ok(entry.user.clone());
        }
    }

    log::info!("No Hackatime user cache hit, fetching");

    let response = CLIENT
        .get("https://hackatime.hackclub.com/api/v1/authenticated/me")
        .bearer_auth(token)
        .send()
        .await;
    let parsed: HackatimeUser = response?.error_for_status()?.json().await?;
    if parsed.user_id == 0 {
        return Err(anyhow::anyhow!("Hackatime user has no id"));
    }

    {
        let mut cache = HACKATIME_USER_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(HackatimeUserCacheEntry {
            token: token.to_string(),
            user: parsed.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(parsed)
}

async fn fetch_project_summaries(token: &str) -> Result<Vec<Project>> {
    let response = CLIENT
        .get("https://hackatime.hackclub.com/api/v1/authenticated/projects?include_archived=true")
        .bearer_auth(token)
        .send()
        .await;
    let parsed: ProjectsResponse = response?.error_for_status()?.json().await?;
    Ok(parsed.projects)
}

pub async fn get_hackatime_projects(req: &HttpRequest) -> Result<Vec<String>> {
    log::info!("Getting Hackatime project names from {}", client_ip(req));
    get_hackatime_projects_with_token(&token(req)?).await
}

pub async fn get_hackatime_projects_with_token(token: &str) -> Result<Vec<String>> {
    {
        let mut cache = HACKATIME_PROJECT_NAMES_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES)
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token) {
            log::info!("Retrieving from Hackatime project names cache");
            return Ok(entry.names.clone());
        }
    }

    log::info!("No Hackatime project names cache hit, fetching");

    let names: Vec<String> = fetch_project_summaries(token)
        .await?
        .into_iter()
        .map(|p| p.name)
        .collect();

    {
        let mut cache = HACKATIME_PROJECT_NAMES_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(ProjectNamesCacheEntry {
            token: token.to_string(),
            names: names.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(names)
}

/// Every project with `name`, `total_seconds`, `most_recent_heartbeat` and `languages` filled.
pub async fn get_hackatime_project_summaries_with_token(token: &str) -> Result<Vec<Project>> {
    {
        let mut cache = HACKATIME_PROJECT_SUMMARIES_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_secs(SUMMARIES_CACHE_EXPIRY_SECONDS)
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token) {
            log::info!("Retrieving from Hackatime project summaries cache");
            return Ok(entry.projects.clone());
        }
    }

    log::info!("No Hackatime project summaries cache hit, fetching");

    let projects = fetch_project_summaries(token).await?;

    {
        let mut cache = HACKATIME_PROJECT_SUMMARIES_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(ProjectSummariesCacheEntry {
            token: token.to_string(),
            projects: projects.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(projects)
}

pub async fn get_hackatime_project(req: &HttpRequest, name: &str) -> Result<Project> {
    log::info!("Getting Hackatime project from {}", client_ip(req));
    get_hackatime_project_with_token(&token(req)?, name).await
}

pub async fn get_hackatime_project_with_token(token: &str, name: &str) -> Result<Project> {
    {
        let mut cache = HACKATIME_PROJECT_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*cache).retain(|e| {
            e.created_time.elapsed() < std::time::Duration::from_mins(CACHE_EXPIRY_MINUTES)
        });

        if let Some(entry) = cache.iter().find(|e| e.token == token && e.name == name) {
            log::info!("Retrieving from Hackatime project cache");
            return Ok(entry.project.clone());
        }
    }

    log::info!("No Hackatime project cache hit, fetching");

    let mut url = reqwest::Url::parse("https://hackatime.hackclub.com/api/v1/users/my/project")?;
    url.path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Cannot build Hackatime project URL"))?
        .push(name);
    url.query_pairs_mut()
        .append_pair("start_date", PROJECT_STATS_START);

    let response = CLIENT.get(url).bearer_auth(token).send().await;
    let parsed: Project = response?.error_for_status()?.json().await?;

    {
        let mut cache = HACKATIME_PROJECT_CACHE
            .get_or_init(|| Mutex::new(vec![]))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        cache.push(ProjectCacheEntry {
            token: token.to_string(),
            name: name.to_string(),
            project: parsed.clone(),
            created_time: Instant::now(),
        });
    }

    Ok(parsed)
}
