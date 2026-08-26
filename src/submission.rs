use actix_web::HttpRequest;
use log::error;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    airtable::{AirtableTable, upsert_records},
    auth::data::get_auth_data,
    hackatime::data::get_hackatime_project,
};

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
    screenshot_url: String,
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
    #[serde(rename = "Birthday")]
    birthday: String,
    #[serde(rename = "Justification - Hackatime Project Name(s) + Date Range(s)")]
    hackatime_projects: String,
    #[serde(flatten)]
    extra: T
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
    let auth_data = get_auth_data(req).await?;
    if !auth_data.ysws_eligible {
        return Err(anyhow::anyhow!("User not YSWS-eligible"));
    }

    let mut hackatime_justification = String::new();
    for name in hackatime_project_names {
        let name = name.trim();
        match get_hackatime_project(req, &name).await {
            Ok(project) => {
                let first_hb = match project.first_heartbeat.as_deref() {
                    Some(hb) => match hb.split("T").nth(0) {
                        Some(date) => date,
                        None => {
                            error!("No first heartbeat for project: {}", name);
                            continue;
                        }
                    },
                    None => {
                        error!("No first heartbeat for project: {}", name);
                        continue;
                    }
                };

                let last_hb = match project.last_heartbeat.as_deref() {
                    Some(hb) => match hb.split("T").nth(0) {
                        Some(date) => date,
                        None => {
                            error!("No last heartbeat for project: {}", name);
                            continue;
                        }
                    },
                    None => {
                        error!("No last heartbeat for project: {}", name);
                        continue;
                    }
                };

                if !hackatime_justification.is_empty() {
                    hackatime_justification.push_str(", ");
                }

                hackatime_justification
                    .push_str(format!("{} {}-{}", &project.name, first_hb, last_hb).as_str());
            }
            Err(e) => {
                error!("Failed to get hackatime project: {}", e);
            }
        }
    }

    let address;
    if let Some(addr) = auth_data.primary_address() {
        address = addr;
    } else if !auth_data.addresses.is_empty() {
        address = auth_data.addresses[0].clone();
    } else {
        return Err(anyhow::anyhow!("No primary address for user"));
    }

    let fields = UnifiedFields {
        code_url,
        playable_url,
        first_name: auth_data.first_name,
        last_name: auth_data.last_name,
        email: auth_data.primary_email,
        screenshot_url,
        description,
        address_line_1: address.line_1,
        address_line_2: address.line_2,
        city: address.city,
        state: address.state,
        zip: address.postal_code,
        birthday: auth_data.birthday,
        hackatime_projects: hackatime_justification,
        extra: additional_fields,
    };

    upsert_records(table, vec![fields], vec!["Code URL".to_string()]).await?;

    Ok(())
}
