pub fn airtable_token() -> String {
    std::env::var("AIRTABLE_TOKEN").unwrap_or_default().to_string()
}

pub fn hca_client_id() -> String {
    std::env::var("HCA_CLIENT_ID").unwrap_or_default().to_string()
}

pub fn hca_client_secret() -> String {
    std::env::var("HCA_CLIENT_SECRET").unwrap_or_default().to_string()
}

pub fn hackatime_token() -> String {
    std::env::var("HACKATIME_TOKEN").unwrap_or_default().to_string()
}
