use actix_web::HttpRequest;

pub mod airtable;
pub mod auth;
pub mod hackatime;
mod http;
pub mod keys;
pub mod submission;

/// The client's address, for logging. Trusts `Forwarded`/`X-Forwarded-For`, so
/// only meaningful behind a proxy that overwrites them.
pub fn client_ip(req: &HttpRequest) -> String {
    let info = req.connection_info();
    info.realip_remote_addr().unwrap_or("unknown").to_string()
}
