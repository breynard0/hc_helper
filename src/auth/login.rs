use std::{
    sync::{Mutex, OnceLock},
    time::Instant,
};

use actix_web::{
    HttpRequest, HttpResponse,
    cookie::{
        Cookie, SameSite,
        time::{Duration, OffsetDateTime},
    },
    web::Query,
};
use serde::{Deserialize, Serialize};

use crate::{
    client_ip, get_reqwest_client,
    keys::{self, base_url, hca_client_id, hca_client_secret},
};

#[derive(Clone, Copy)]
struct StateRegistryItem {
    value: u128,
    created_time: Instant,
}
static STATE_REGISTRY: OnceLock<Mutex<Vec<StateRegistryItem>>> = OnceLock::new();
const STATE_EXPIRY_TIME_MINUTES: u64 = 5;

pub const TOKEN_COOKIE: &str = "auth-token";

pub fn get_auth_token_with_handling(req: &HttpRequest) -> Option<String> {
    req.cookie(TOKEN_COOKIE).map(|x| x.value().to_string())
}

#[macro_export]
/// Takes in an HttpRequest reference
macro_rules! get_auth_token {
    ($req:ident) => {
        match $crate::auth::login::get_auth_token_with_handling(&$req) {
            Some(token) => token,
            None => return $crate::auth::login::get_login_redirect_response(),
        }
    };
}


/// Get the redirect to the /login page
pub fn get_login_redirect_response() -> HttpResponse {
    HttpResponse::Found()
        .append_header(("Location", "/login"))
        .finish()
}

pub struct Scopes {
    pub openid: bool,
    pub profile: bool,
    pub email: bool,
    pub name: bool,
    pub slack_id: bool,
    pub verification_status: bool,
    pub basic_info: bool,
    pub addresses: bool,
}

fn scopes_to_string(scopes: Scopes) -> String {
    let mut out = String::new();
    if scopes.openid {
        out.push_str("openid+");
    }
    if scopes.profile {
        out.push_str("profile+");
    }
    if scopes.email {
        out.push_str("email+");
    }
    if scopes.name {
        out.push_str("name+");
    }
    if scopes.slack_id {
        out.push_str("slack_id+");
    }
    if scopes.verification_status {
        out.push_str("verification_status+");
    }
    if scopes.basic_info {
        out.push_str("basic_info+");
    }
    if scopes.addresses {
        out.push_str("address+");
    }
    out.pop();
    out
}

fn redirect_url(req: &HttpRequest) -> String {
    format!(
        "{}://{}/callback",
        req.connection_info().scheme(),
        base_url(),
    )
}

/// should be placed at /login, saves state cookie and redirects to HCA redirect URL
pub async fn handle_login(req: &HttpRequest, scopes: Scopes) -> actix_web::HttpResponse {
    let caller = client_ip(req);
    log::info!("Beginning new login attempt from {caller}");

    let state_value = rand::random::<u128>();

    {
        let mut state_registry = STATE_REGISTRY
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        state_registry.push(StateRegistryItem {
            value: state_value,
            created_time: Instant::now(),
        });
    }

    let auth_url = format!(
        "https://auth.hackclub.com/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        keys::hca_client_id(),
        redirect_url(req),
        scopes_to_string(scopes),
        state_value
    );

    {
        let mut registry = STATE_REGISTRY
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|x| x.into_inner());
        (*registry).retain(|x| x.created_time.elapsed().as_secs() < STATE_EXPIRY_TIME_MINUTES * 60);
    }

    let mut state_cookie = Cookie::new("state", state_value.to_string());
    state_cookie.set_http_only(true);
    state_cookie.set_secure(true);
    state_cookie.set_same_site(SameSite::Lax);
    state_cookie.set_expires(
        OffsetDateTime::now_utc() + Duration::new(STATE_EXPIRY_TIME_MINUTES as i64 * 60, 0),
    );

    actix_web::HttpResponse::Found()
        .cookie(state_cookie)
        .append_header(("Location", auth_url))
        .body("redirecting to Hack Club Auth")
}

#[derive(Deserialize)]
pub struct CallbackArgs {
    pub code: Option<String>,
    pub state: Option<String>,
}

#[derive(Serialize)]
pub struct CodeRequestBody {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub code: String,
    pub grant_type: String,
}

#[derive(Deserialize)]
pub struct CodeRequestResponse {
    pub access_token: Option<String>,
    pub token_type: Option<String>,
    pub expires_in: Option<u32>,
    pub scope: Option<String>,
    pub refresh_token: Option<String>,
}

/// should be placed at /callback, validates response and saves token cookie
pub async fn handle_callback(
    req: &HttpRequest,
    query: Query<CallbackArgs>,
    redirect_url_on_success: String,
) -> actix_web::HttpResponse {
    let caller = client_ip(req);
    log::info!("Callback initiated from {caller}");

    let args = query.into_inner();

    let state_from_cookie = match req.cookie("state") {
        Some(cookie) => match cookie.value().parse::<u128>() {
            Ok(x) => x,
            Err(e) => {
                log::error!("Invalid state cookie: {e} from {caller}");
                return get_login_redirect_response();
            }
        },
        None => {
            log::error!("No state cookie from {caller}");
            return get_login_redirect_response();
        }
    };

    let state_from_url = match args.state {
        Some(x) => match x.parse::<u128>() {
            Ok(x) => x,
            Err(e) => {
                log::error!("Invalid state in URL: {e} from {caller}");
                return get_login_redirect_response();
            }
        },
        None => {
            log::error!("No state in URL from {caller}");
            return get_login_redirect_response();
        }
    };

    let state_found;
    {
        let mut registry = STATE_REGISTRY
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap_or_else(|x| x.into_inner());

        (*registry).retain(|x| x.created_time.elapsed().as_secs() < STATE_EXPIRY_TIME_MINUTES * 60);

        state_found = registry.iter().any(|x| x.value == state_from_cookie);

        (*registry).retain(|x| x.value != state_from_cookie);

        if !state_found || state_from_cookie != state_from_url {
            log::error!("State mismatch from {caller}");
            return get_login_redirect_response();
        }
    }

    log::info!("State matches from {caller}");

    let code = match args.code {
        Some(x) => x,
        None => {
            log::error!("No code variable from {caller}");
            return get_login_redirect_response();
        }
    };

    let body = CodeRequestBody {
        client_id: hca_client_id(),
        client_secret: hca_client_secret(),
        redirect_uri: redirect_url(req),
        code,
        grant_type: "authorization_code".to_string(),
    };

    let client = get_reqwest_client();

    log::info!("Sending token request from {caller}");

    let response = match client
        .post("https://auth.hackclub.com/oauth/token")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            log::error!("error: {e} from {caller}");
        }) {
        Ok(x) => x,
        Err(_) => return get_login_redirect_response(),
    };

    let parsed: CodeRequestResponse = match {
        match response.error_for_status() {
            Ok(x) => x,
            Err(e) => {
                log::error!("error: {e} from {caller}");
                return get_login_redirect_response();
            }
        }
    }
    .json()
    .await
    {
        Ok(x) => x,
        Err(e) => {
            log::error!("error: {e} from {caller}");
            return get_login_redirect_response();
        }
    };

    let token = match parsed.access_token {
        Some(x) => x,
        None => {
            log::error!("No access token from {caller}");
            return get_login_redirect_response();
        }
    };

    log::info!("Token fetch successful from {caller}");

    let mut token_cookie = Cookie::new(TOKEN_COOKIE, token);
    token_cookie.set_secure(true);
    token_cookie.set_http_only(true);
    token_cookie.set_same_site(SameSite::Lax);
    token_cookie.set_expires(OffsetDateTime::now_utc() + Duration::new(60 * 60 * 24 * 5, 0));

    HttpResponse::Found()
        .cookie(token_cookie)
        .append_header(("Location", redirect_url_on_success.as_str()))
        .body("Authentication successful, redirecting...")
}
