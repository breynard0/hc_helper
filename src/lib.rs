use std::sync::OnceLock;

use actix_web::HttpRequest;

pub mod airtable;
pub mod auth;
pub mod hackatime;
pub mod keys;
pub mod submission;

static REQWEST_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub fn get_reqwest_client() -> &'static reqwest::Client {
    REQWEST_CLIENT.get_or_init(reqwest::Client::new)
}

/// The client's address, for logging. Trusts `Forwarded`/`X-Forwarded-For`, so
/// only meaningful behind a proxy that overwrites them.
pub fn client_ip(req: &HttpRequest) -> String {
    let info = req.connection_info();
    info.realip_remote_addr().unwrap_or("unknown").to_string()
}
