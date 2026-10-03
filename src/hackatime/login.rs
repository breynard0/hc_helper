use actix_web::{
    HttpRequest, HttpResponse,
    cookie::{
        Cookie, SameSite,
        time::{Duration, OffsetDateTime},
    },
    http::{StatusCode, header::ContentType},
    web::Query,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::{
    client_ip,
    http::CLIENT,
    keys::{self, hackatime_client_id, hackatime_client_secret},
};

const STATE_EXPIRY_TIME_MINUTES: u64 = 5;

pub const TOKEN_COOKIE: &str = "hackatime-token";
pub const STATE_COOKIE: &str = "hackatime-state";

pub fn get_hackatime_token_with_handling(req: &HttpRequest) -> Option<String> {
    req.cookie(TOKEN_COOKIE).map(|x| x.value().to_string())
}

#[macro_export]
/// Takes in an HttpRequest reference
macro_rules! get_hackatime_token {
    ($req:ident) => {
        match $crate::hackatime::login::get_hackatime_token_with_handling(&$req) {
            Some(token) => token,
            None => return $crate::hackatime::login::get_login_redirect_response(),
        }
    };
}

pub fn get_login_redirect_response() -> HttpResponse {
    HttpResponse::Found()
        .append_header(("Location", "/hackatime/login"))
        .finish()
}

fn sign_in_failed(status: StatusCode) -> HttpResponse {
    HttpResponse::build(status)
        .content_type(ContentType::plaintext())
        .body("Sign-in failed. Please go back and try again.")
}

fn new_state_cookie(value: String, http_only: bool) -> Cookie<'static> {
    let mut state_cookie = Cookie::new(STATE_COOKIE, value);
    state_cookie.set_http_only(http_only);
    state_cookie.set_secure(true);
    state_cookie.set_same_site(SameSite::Lax);
    state_cookie
}

pub struct Scopes {
    pub profile: bool,
    pub read: bool,
    pub admin: bool,
}

fn scopes_to_string(scopes: Scopes) -> String {
    let mut out = String::new();
    if scopes.profile {
        out.push_str("profile ");
    }
    if scopes.read {
        out.push_str("read ");
    }
    if scopes.admin {
        out.push_str("admin ");
    }
    out.pop();
    out
}

pub async fn handle_login(
    req: &HttpRequest,
    scopes: Scopes,
    callback_redirect_url: String,
    http_only: bool,
) -> actix_web::HttpResponse {
    let caller = client_ip(req);
    log::info!("Beginning new Hackatime login attempt from {caller}");

    let state_value = rand::random::<u128>();

    let mut auth_url = match Url::parse("https://hackatime.hackclub.com/oauth/authorize") {
        Ok(x) => x,
        Err(e) => {
            log::error!("Invalid authorize URL: {e}");
            return HttpResponse::InternalServerError().finish();
        }
    };
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &keys::hackatime_client_id())
        .append_pair("redirect_uri", &callback_redirect_url)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes_to_string(scopes))
        .append_pair("state", &state_value.to_string());

    let mut state_cookie = new_state_cookie(state_value.to_string(), http_only);
    state_cookie.set_expires(
        OffsetDateTime::now_utc() + Duration::new(STATE_EXPIRY_TIME_MINUTES as i64 * 60, 0),
    );

    actix_web::HttpResponse::Found()
        .cookie(state_cookie)
        .append_header(("Location", auth_url.as_str()))
        .body("redirecting to Hackatime")
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

pub async fn handle_callback<F>(
    req: &HttpRequest,
    query: Query<CallbackArgs>,
    callback_redirect_url: String,
    redirect_url_on_success: String,
    push_token_db: Option<F>,
    should_store_auth_token_cookie: bool,
) -> actix_web::HttpResponse
where
    F: AsyncFnOnce(String),
{
    let mut response = process_callback(
        req,
        query,
        callback_redirect_url,
        redirect_url_on_success,
        push_token_db,
        should_store_auth_token_cookie,
    )
    .await;

    let _ = response.add_removal_cookie(&new_state_cookie(String::new(), true));

    response
}

async fn process_callback<F>(
    req: &HttpRequest,
    query: Query<CallbackArgs>,
    callback_redirect_url: String,
    redirect_url_on_success: String,
    push_token_db: Option<F>,
    should_store_auth_token_cookie: bool,
) -> actix_web::HttpResponse
where
    F: AsyncFnOnce(String),
{
    let caller = client_ip(req);
    log::info!("Hackatime callback initiated from {caller}");

    let args = query.into_inner();

    let state_from_cookie = match req.cookie(STATE_COOKIE) {
        Some(cookie) => match cookie.value().parse::<u128>() {
            Ok(x) => x,
            Err(e) => {
                log::error!("Invalid state cookie: {e} from {caller}");
                return sign_in_failed(StatusCode::BAD_REQUEST);
            }
        },
        None => {
            log::error!("No state cookie from {caller}");
            return sign_in_failed(StatusCode::BAD_REQUEST);
        }
    };

    let state_from_url = match args.state {
        Some(x) => match x.parse::<u128>() {
            Ok(x) => x,
            Err(e) => {
                log::error!("Invalid state in URL: {e} from {caller}");
                return sign_in_failed(StatusCode::BAD_REQUEST);
            }
        },
        None => {
            log::error!("No state in URL from {caller}");
            return sign_in_failed(StatusCode::BAD_REQUEST);
        }
    };

    if state_from_cookie != state_from_url {
        log::error!("State mismatch from {caller}");
        return sign_in_failed(StatusCode::BAD_REQUEST);
    }

    log::info!("State matches from {caller}");

    let code = match args.code {
        Some(x) => x,
        None => {
            log::error!("No code variable from {caller}");
            return sign_in_failed(StatusCode::BAD_REQUEST);
        }
    };

    let body = CodeRequestBody {
        client_id: hackatime_client_id(),
        client_secret: hackatime_client_secret(),
        redirect_uri: callback_redirect_url,
        code,
        grant_type: "authorization_code".to_string(),
    };

    log::info!("Sending Hackatime token request from {caller}");

    let response = match CLIENT
        .post("https://hackatime.hackclub.com/oauth/token")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            log::error!("error: {e} from {caller}");
        }) {
        Ok(x) => x,
        Err(_) => return sign_in_failed(StatusCode::BAD_GATEWAY),
    };

    let parsed: CodeRequestResponse = match {
        match response.error_for_status() {
            Ok(x) => x,
            Err(e) => {
                log::error!("error: {e} from {caller}");
                return sign_in_failed(StatusCode::BAD_GATEWAY);
            }
        }
    }
    .json()
    .await
    {
        Ok(x) => x,
        Err(e) => {
            log::error!("error: {e} from {caller}");
            return sign_in_failed(StatusCode::BAD_GATEWAY);
        }
    };

    let token = match parsed.access_token {
        Some(x) => x,
        None => {
            log::error!("No access token from {caller}");
            return sign_in_failed(StatusCode::BAD_GATEWAY);
        }
    };

    log::info!("Hackatime token fetch successful from {caller}");

    if let Some(f) = push_token_db {
        f(token.clone()).await;
    }

    let mut token_cookie = Cookie::new(TOKEN_COOKIE, token);
    token_cookie.set_secure(true);
    token_cookie.set_http_only(true);
    token_cookie.set_same_site(SameSite::Lax);
    token_cookie.set_path("/");
    token_cookie.set_expires(OffsetDateTime::now_utc() + Duration::new(60 * 60 * 24 * 5, 0));

    let mut response = HttpResponse::Found()
        .append_header(("Location", redirect_url_on_success.as_str()))
        .body("Hackatime authentication successful, redirecting...");

    if should_store_auth_token_cookie {
        let _ = response.add_cookie(&token_cookie);
    }

    response
}
