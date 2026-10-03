use actix_web::HttpRequest;
use anyhow::Context;
use serde::Serialize;

use crate::{
    airtable::{AirtableTable, create_records, upsert_records},
    auth::{data::get_auth_data_with_token, login::get_auth_token_with_handling},
    hackatime::{data::get_hackatime_project_with_token, login::get_hackatime_token_with_handling},
};

pub const UNIFIED_FIELDS: &[&str] = &[
    "Code URL",
    "Playable URL",
    "First Name",
    "Last Name",
    "Email",
    "Screenshot",
    "Description",
    "Address (Line 1)",
    "Address (Line 2)",
    "City",
    "State / Province",
    "ZIP / Postal Code",
    "Country",
    "Birthday",
    "Justification - Hackatime Project Name(s) + Date Range(s)",
];

#[derive(Serialize)]
struct Attachment {
    url: String,
}

#[derive(Serialize)]
pub struct UnifiedFields<T>
where
    T: Serialize,
{
    #[serde(rename = "Code URL")]
    code_url: String,
    #[serde(rename = "Playable URL")]
    playable_url: String,
    #[serde(rename = "First Name")]
    first_name: String,
    #[serde(rename = "Last Name")]
    last_name: String,
    #[serde(rename = "Email")]
    email: String,
    #[serde(rename = "Screenshot")]
    screenshot: Vec<Attachment>,
    #[serde(rename = "Description")]
    description: String,
    #[serde(rename = "Address (Line 1)")]
    address_line_1: String,
    #[serde(rename = "Address (Line 2)")]
    address_line_2: String,
    #[serde(rename = "City")]
    city: String,
    #[serde(rename = "State / Province")]
    state: String,
    #[serde(rename = "ZIP / Postal Code")]
    zip: String,
    #[serde(rename = "Country")]
    country: String,
    #[serde(rename = "Birthday")]
    birthday: String,
    #[serde(rename = "Justification - Hackatime Project Name(s) + Date Range(s)")]
    hackatime_projects: String,
    #[serde(flatten)]
    extra: T,
}

pub async fn push_unified<T>(
    req: &HttpRequest,
    table: AirtableTable,
    code_url: String,
    playable_url: String,
    screenshot_url: String,
    description: String,
    hackatime_project_names: Vec<String>,
    additional_fields: T,
) -> anyhow::Result<()>
where
    T: Serialize,
{
    let auth_token =
        get_auth_token_with_handling(req).ok_or_else(|| anyhow::anyhow!("No auth token"))?;
    let hackatime_token = get_hackatime_token_with_handling(req)
        .ok_or_else(|| anyhow::anyhow!("No Hackatime token"))?;
    push_unified_with_token(
        &auth_token,
        &hackatime_token,
        table,
        code_url,
        playable_url,
        screenshot_url,
        description,
        hackatime_project_names,
        additional_fields,
    )
    .await
}

pub async fn push_unified_with_token<T>(
    auth_token: &str,
    hackatime_token: &str,
    table: AirtableTable,
    code_url: String,
    playable_url: String,
    screenshot_url: String,
    description: String,
    hackatime_project_names: Vec<String>,
    additional_fields: T,
) -> anyhow::Result<()>
where
    T: Serialize,
{
    let fields = unified_fields(
        auth_token,
        hackatime_token,
        code_url,
        playable_url,
        screenshot_url,
        description,
        hackatime_project_names,
        additional_fields,
    )
    .await?;

    upsert_records(
        table,
        vec![fields],
        vec!["Code URL".to_string(), "Email".to_string()],
    )
    .await?;

    Ok(())
}

/// Creates a new submission record every time instead of upserting on Code URL and Email.
/// Returns the new record's id.
pub async fn create_unified_with_token<T>(
    auth_token: &str,
    hackatime_token: &str,
    table: AirtableTable,
    code_url: String,
    playable_url: String,
    screenshot_url: String,
    description: String,
    hackatime_project_names: Vec<String>,
    additional_fields: T,
) -> anyhow::Result<String>
where
    T: Serialize,
{
    let fields = unified_fields(
        auth_token,
        hackatime_token,
        code_url,
        playable_url,
        screenshot_url,
        description,
        hackatime_project_names,
        additional_fields,
    )
    .await?;

    create_records(table, vec![fields])
        .await?
        .pop()
        .ok_or_else(|| anyhow::anyhow!("Airtable returned no record"))
}

async fn unified_fields<T>(
    auth_token: &str,
    hackatime_token: &str,
    code_url: String,
    playable_url: String,
    screenshot_url: String,
    description: String,
    hackatime_project_names: Vec<String>,
    additional_fields: T,
) -> anyhow::Result<UnifiedFields<T>>
where
    T: Serialize,
{
    let auth_data = get_auth_data_with_token(auth_token).await?;
    if !auth_data.ysws_eligible {
        return Err(anyhow::anyhow!("User not YSWS-eligible"));
    }

    let mut hackatime_justification = String::new();
    for name in hackatime_project_names {
        let name = name.trim();
        let project = get_hackatime_project_with_token(hackatime_token, name)
            .await
            .with_context(|| format!("Failed to get hackatime project: {name}"))?;

        let first_hb = project
            .first_heartbeat
            .as_deref()
            .and_then(|hb| hb.split("T").nth(0))
            .filter(|date| !date.is_empty())
            .ok_or_else(|| anyhow::anyhow!("No first heartbeat for project: {name}"))?;

        let last_hb = project
            .last_heartbeat
            .as_deref()
            .and_then(|hb| hb.split("T").nth(0))
            .filter(|date| !date.is_empty())
            .ok_or_else(|| anyhow::anyhow!("No last heartbeat for project: {name}"))?;

        if !hackatime_justification.is_empty() {
            hackatime_justification.push_str(", ");
        }

        hackatime_justification
            .push_str(format!("{} {}-{}", &project.name, first_hb, last_hb).as_str());
    }

    let address;
    if let Some(addr) = auth_data.primary_address() {
        address = addr;
    } else if !auth_data.addresses.is_empty() {
        address = auth_data.addresses[0].clone();
    } else {
        return Err(anyhow::anyhow!("No primary address for user"));
    }

    Ok(UnifiedFields {
        code_url,
        playable_url,
        first_name: auth_data.first_name,
        last_name: auth_data.last_name,
        email: auth_data.primary_email,
        screenshot: match screenshot_url.is_empty() {
            true => vec![],
            false => vec![Attachment {
                url: screenshot_url,
            }],
        },
        description,
        address_line_1: address.line_1,
        address_line_2: address.line_2,
        city: address.city,
        state: address.state,
        zip: address.postal_code,
        country: address.country,
        birthday: auth_data.birthday,
        hackatime_projects: hackatime_justification,
        extra: additional_fields,
    })
}
