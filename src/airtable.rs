use std::{
    str::FromStr,
    sync::OnceLock,
    time::{Duration, Instant},
};

use actix_web::http::header::HttpDate;
use anyhow::{Result, anyhow};
use log::{error, info};
use reqwest::{
    Body, Method, Request, Response, StatusCode, Url,
    header::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    sync::{
        mpsc::{self, Sender},
        oneshot,
    },
    time::sleep,
};

use crate::keys::airtable_token;

#[derive(Clone)]
pub struct AirtableTable {
    pub base_id: String,
    pub table_id_or_name: String,
}

impl AirtableTable {
    fn url(&self) -> Result<Url> {
        let mut url = Url::parse(BASE_URL)?;
        url.path_segments_mut()
            .map_err(|_| anyhow!("{BASE_URL} cannot be a base"))?
            .push("v0")
            .push(&self.base_id)
            .push(&self.table_id_or_name);
        Ok(url)
    }
}

fn escape_formula_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn escape_formula_field(field: &str) -> String {
    field.replace('\\', "\\\\").replace('}', "\\}")
}

static BASE_URL: &str = "https://api.airtable.com";

const MAX_REQUESTS_PER_SECOND: usize = 4;

struct AirtableRequest {
    table: AirtableTable,
    req: Request,
    res: oneshot::Sender<Result<Response>>,
    rate_limited_count: u32,
}

static QUEUE_HANDLER_TX: OnceLock<Sender<AirtableRequest>> = OnceLock::new();

pub fn spawn_airtable_queue_handler() -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<AirtableRequest>(1024);

    QUEUE_HANDLER_TX
        .set(tx)
        .map_err(|_| anyhow!("error setting tx"))?;

    tokio::spawn(async move {
        struct RequestInstance {
            airtable_base_id: String,
            instant: Instant,
        }
        let mut request_instances: Vec<RequestInstance> = Vec::new();

        while let Some(req) = rx.recv().await {
            info!(
                "Sending out request to <{}>",
                req.req.url().domain().unwrap_or("")
            );

            request_instances.retain(|x| x.instant.elapsed() < Duration::from_secs(1));

            let mut filtered = request_instances
                .iter()
                .filter(|x| x.airtable_base_id == req.table.base_id)
                .collect::<Vec<_>>();
            if filtered.len() >= MAX_REQUESTS_PER_SECOND {
                filtered.sort_by_key(|k| k.instant.elapsed());
                let oldest = filtered.last().unwrap();

                let sleep_amount = 1.0 - oldest.instant.elapsed().as_secs_f64();
                if sleep_amount > 0.0 {
                    sleep(Duration::from_secs_f64(sleep_amount)).await;
                } else {
                    sleep(Duration::from_secs_f64(1.0)).await;
                }
            }

            let duplicated_request_for_retry = req.req.try_clone();
            let duplicated_table_for_retry = req.table.clone();
            let num_rate_limited = req.rate_limited_count;
            if num_rate_limited > 10 {
                error!(
                    "A very patient HTTP request has just hit ten rate limit retries. It will rest now."
                );
                req.res
                    .send(Err(anyhow::anyhow!("Rate limit retries exceeded")))
                    .ok();
                continue;
            }

            request_instances.push(RequestInstance {
                airtable_base_id: req.table.base_id,
                instant: Instant::now(),
            });

            tokio::spawn(async move {
                let client = reqwest::Client::new();
                let mut response_result: Result<Response, anyhow::Error> =
                    client.execute(req.req).await.map_err(|e| e.into());
                if let Ok(resp) = response_result {
                    response_result = match resp.status() == StatusCode::TOO_MANY_REQUESTS {
                        true => {
                            if let Some(req_handle) = duplicated_request_for_retry {
                                let backoff_time_default = 2_u64.pow(num_rate_limited).min(5 * 60);
                                let sleep_time = match resp
                                    .headers()
                                    .get(HeaderName::from_static("retry-after"))
                                {
                                    Some(x) => match x.to_str() {
                                        Ok(x) => match x.parse::<u64>() {
                                            Ok(x) => x,
                                            Err(_) => match HttpDate::from_str(x) {
                                                Ok(date) => {
                                                    let sys_time: std::time::SystemTime =
                                                        date.into();
                                                    let duration =
                                                        sys_time
                                                            .duration_since(
                                                                std::time::SystemTime::now(),
                                                            )
                                                            .unwrap_or_default();
                                                    match duration.as_secs() {
                                                        0 => backoff_time_default,
                                                        _ => duration.as_secs(),
                                                    }
                                                }
                                                Err(_) => backoff_time_default,
                                            },
                                        },
                                        Err(_) => backoff_time_default,
                                    },
                                    None => backoff_time_default,
                                };
                                sleep(Duration::from_secs(sleep_time)).await;
                                enqueue_airtable_request(
                                    req_handle,
                                    duplicated_table_for_retry,
                                    num_rate_limited + 1,
                                )
                                .await
                            } else {
                                Err(anyhow::anyhow!(
                                    "Failed to clone request after Airtable returned a 429"
                                ))
                            }
                        }
                        false => match resp.error_for_status_ref() {
                            Ok(_) => Ok(resp),
                            Err(e) => {
                                let body = resp.text().await.unwrap_or_default();
                                Err(anyhow::Error::new(e)
                                    .context(format!("Airtable responded: {body}")))
                            }
                        },
                    }
                }

                match req.res.send(response_result) {
                    Ok(_) => {}
                    Err(e) => error!("{:?}", e),
                }
            });
        }
    });

    Ok(())
}

async fn enqueue_airtable_request(
    request: Request,
    table: AirtableTable,
    rate_limited_count: u32,
) -> Result<Response> {
    match QUEUE_HANDLER_TX.get() {
        Some(handle) => {
            let (sender, receiver) = oneshot::channel();
            handle
                .send(AirtableRequest {
                    table,
                    req: request,
                    res: sender,
                    rate_limited_count,
                })
                .await?;
            match receiver.await {
                Ok(out) => out,
                Err(e) => Err(e.into()),
            }
        }
        None => Err(anyhow!("Queue handler uninitialized")),
    }
}

#[derive(Serialize)]
struct UpsertSettings {
    #[serde(rename = "fieldsToMergeOn")]
    fields_to_merge_on: Vec<String>,
}

#[derive(Serialize)]
struct RecordEntry<T> {
    fields: T,
}

#[derive(Serialize)]
struct UpsertTopLevel<'a, T> {
    #[serde(rename = "performUpsert")]
    settings: &'a UpsertSettings,
    records: &'a [RecordEntry<T>],
    typecast: bool,
}

const MAX_RECORDS_PER_UPSERT: usize = 10;

pub async fn upsert_records<T>(
    table: AirtableTable,
    records: Vec<T>,
    fields_to_merge_on_names: Vec<String>,
) -> Result<()>
where
    T: Serialize,
{
    let settings = UpsertSettings {
        fields_to_merge_on: fields_to_merge_on_names,
    };
    let entries = records
        .into_iter()
        .map(|r| RecordEntry { fields: r })
        .collect::<Vec<_>>();

    info!(
        "Upserting {} record(s) to {}",
        entries.len(),
        table.table_id_or_name
    );

    let url = table.url()?;

    for chunk in entries.chunks(MAX_RECORDS_PER_UPSERT) {
        let body_parsed = serde_json::to_string(&UpsertTopLevel {
            settings: &settings,
            records: chunk,
            typecast: true,
        })
        .unwrap();

        let mut request = Request::new(Method::PATCH, url.clone());

        request
            .headers_mut()
            .append("Content-Type", HeaderValue::from_static("application/json"));

        let mut auth_header_value = HeaderValue::from_str(&format!("Bearer {}", airtable_token()))
            .expect("bad airtable token");
        auth_header_value.set_sensitive(true);
        request
            .headers_mut()
            .append("Authorization", auth_header_value);

        let body_mut = request.body_mut();
        *body_mut = Some(Body::from(body_parsed));

        enqueue_airtable_request(request, table.clone(), 0).await?;
    }

    Ok(())
}

#[derive(Deserialize)]
struct FindRecordsTopLevel<T> {
    records: Vec<FindRecord<T>>,
    offset: Option<String>,
}

#[derive(Deserialize)]
struct FindRecord<T> {
    #[serde(rename = "id")]
    _id: String,
    #[serde(rename = "createdTime")]
    _created_time: String,
    fields: T,
}

pub async fn find_records<T>(table: AirtableTable, field: String, value: String) -> Result<Vec<T>>
where
    T: DeserializeOwned + Default,
{
    let formula = format!(
        "{{{}}}='{}'",
        escape_formula_field(&field),
        escape_formula_string(&value)
    );

    info!(
        "Finding records in {} where {} is {}",
        table.table_id_or_name, field, value
    );

    let url = table.url()?;
    let mut records = Vec::new();
    let mut offset: Option<String> = None;

    loop {
        let mut page_url = url.clone();
        {
            let mut query = page_url.query_pairs_mut();
            query.append_pair("filterByFormula", &formula);
            if let Some(offset) = &offset {
                query.append_pair("offset", offset);
            }
        }

        let mut request = Request::new(Method::GET, page_url);
        request.headers_mut().append(
            "Content-Type",
            HeaderValue::from_str("application/json").expect("bad content type"),
        );

        let mut auth_header_value = HeaderValue::from_str(&format!("Bearer {}", airtable_token()))
            .expect("bad airtable token");
        auth_header_value.set_sensitive(true);
        request
            .headers_mut()
            .append("Authorization", auth_header_value);

        let response = enqueue_airtable_request(request, table.clone(), 0).await?;

        let parsed: FindRecordsTopLevel<T> = response.json().await?;

        records.extend(parsed.records.into_iter().map(|r| r.fields));

        match parsed.offset {
            Some(next) => offset = Some(next),
            None => break,
        }
    }

    Ok(records)
}
