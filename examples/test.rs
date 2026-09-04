use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web, web::Query};
use hc_helper::airtable::{self, AirtableTable};
use hc_helper::auth::data::{self, Address, AuthData, VerificationStatus};
use hc_helper::auth::login::{self, CallbackArgs, Scopes};
use hc_helper::get_reqwest_client;
use hc_helper::hackatime::data::{self as hackatime, HackatimeUser, Project};
use hc_helper::hackatime::login::{
    self as hackatime_login, CallbackArgs as HackatimeCallbackArgs, Scopes as HackatimeScopes,
};
use hc_helper::keys::airtable_token;
use hc_helper::submission;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const PORT: u16 = 8080;
const AIRTABLE_API: &str = "https://api.airtable.com";
const AIRTABLE_BASE_ID: &str = "appJiSAd1LqJ4aFWL";
const AIRTABLE_TABLE: &str = "YSWS Project Submission";
const OK_ROWS_SHOWN: usize = 5;
const LOGIN_BUTTON: &str = "<p><a class=\"button\" href=\"/login\">Log in with Hack Club</a></p>";
const HACKATIME_LOGIN_BUTTON: &str =
    "<p><a class=\"button\" href=\"/hackatime/login\">Log in with Hackatime</a></p>";
/// Replaces the requests tables in place while anything is in flight, so scrolling and form
/// inputs survive a spam.
const POLL_SCRIPT: &str = "<script>(function(){\
var el=document.getElementById('requests');\
function busy(){var w=el.firstElementChild;return w&&w.dataset.busy==='1'}\
function tick(){fetch('/airtable/requests').then(function(r){return r.text()})\
.then(function(html){el.innerHTML=html;if(busy())setTimeout(tick,1000)})}\
if(busy())setTimeout(tick,1000)})()</script>";
const STYLE: &str = ":root{--nord0:#2E3440;--nord1:#3B4252;--nord2:#434C5E;--nord3:#4C566A;--nord4:#D8DEE9;--nord6:#ECEFF4;--nord8:#88C0D0;--nord9:#81A1C1;--nord10:#5E81AC;--muted:#616E88}html{background:var(--nord0)}body{font:15px/1.5 system-ui,sans-serif;max-width:44rem;margin:3rem auto;padding:0 1rem;background:var(--nord0);color:var(--nord4)}h1,h2,h3{color:var(--nord6);font-weight:600}h1{border-bottom:1px solid var(--nord3);padding-bottom:.5rem}table{border-collapse:collapse;width:100%;margin-bottom:1.5rem;border:1px solid var(--nord3);border-radius:.3rem;overflow:hidden}th,td{border:1px solid var(--nord3);padding:.4rem .6rem;text-align:left;vertical-align:top}th{width:14rem;background:var(--nord1);color:var(--nord9);font-family:ui-monospace,monospace;font-weight:600}td{background:var(--nord0);color:var(--nord4);font-family:ui-monospace,monospace;word-break:break-word}tr:nth-child(even) td{background:#333A47}em{color:var(--muted);font-style:italic}code{color:var(--nord8);font-family:ui-monospace,monospace}a.button{display:inline-block;background:var(--nord10);color:var(--nord6);text-decoration:none;padding:.7rem 1.4rem;border-radius:.4rem;font-weight:600}a.button:hover{background:var(--nord9)}nav{display:flex;gap:.5rem;margin-bottom:1.5rem}a.tab{padding:.5rem 1rem;border-radius:.4rem;text-decoration:none;background:var(--nord1);color:var(--nord9);font-weight:600}a.tab.active{background:var(--nord10);color:var(--nord6)}form{display:flex;gap:.5rem;flex-wrap:wrap;margin-bottom:1.5rem}input{font:15px/1.5 ui-monospace,monospace;padding:.6rem;min-width:18rem;border:1px solid var(--nord3);border-radius:.4rem;background:var(--nord1);color:var(--nord4)}button{font:15px/1.5 system-ui,sans-serif;font-weight:600;padding:.6rem 1.2rem;border:0;border-radius:.4rem;background:var(--nord10);color:var(--nord6);cursor:pointer}button:hover{background:var(--nord9)}ul{padding-left:1.2rem}li{font-family:ui-monospace,monospace;margin-bottom:.2rem}li a{color:var(--nord8)}";

fn escape(v: &str) -> String {
    v.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn attr(v: &str) -> String {
    escape(v).replace('"', "&quot;")
}

fn row(label: &str, value: &str) -> String {
    let value = if value.is_empty() {
        "<em>empty</em>".to_string()
    } else {
        escape(value)
    };
    format!("<tr><th>{label}</th><td>{value}</td></tr>")
}

fn table(rows: &[(&str, &str)]) -> String {
    let rows = rows.iter().map(|(l, v)| row(l, v)).collect::<String>();
    format!("<table>{rows}</table>")
}

fn section(heading: &str, rows: &[(&str, &str)]) -> String {
    format!("<h3>{heading}</h3>{}", table(rows))
}

/// Concatenates `f` over an indexed list, e.g. `records[0]`, `records[1]`, ...
fn indexed<T>(name: &str, items: &[T], f: impl Fn(&str, &T) -> String) -> String {
    items
        .iter()
        .enumerate()
        .map(|(i, x)| f(&format!("{name}[{i}]"), x))
        .collect()
}

fn page(body: &str, active: &str) -> HttpResponse {
    let nav = [
        ("/", "auth"),
        ("/hackatime", "hackatime"),
        ("/airtable", "airtable"),
        ("/submission", "submission"),
    ]
    .iter()
    .map(|(href, label)| {
        let class = if *href == active { "tab active" } else { "tab" };
        format!("<a class=\"{class}\" href=\"{href}\">{label}</a>")
    })
    .collect::<String>();

    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(format!(
            "<!doctype html><html><head><meta charset=\"utf-8\">\
         <title>hc_helper auth test</title><style>{STYLE}</style></head>\
         <body><h1>hc_helper test</h1>{nav}{body}</body></html>"
        ))
}

fn all_scopes() -> Scopes {
    Scopes {
        openid: true,
        profile: true,
        email: true,
        name: true,
        slack_id: true,
        verification_status: true,
        basic_info: true,
        addresses: true,
    }
}

fn all_hackatime_scopes() -> HackatimeScopes {
    HackatimeScopes {
        profile: true,
        read: true,
        admin: false,
    }
}

fn verification_label(status: &VerificationStatus) -> &'static str {
    match status {
        VerificationStatus::NeedsSubmission => "NeedsSubmission",
        VerificationStatus::Pending => "Pending",
        VerificationStatus::VerifiedEligible => "VerifiedEligible",
        VerificationStatus::VerifiedButOver18 => "VerifiedButOver18",
        VerificationStatus::Rejected => "Rejected",
        VerificationStatus::NotFound => "NotFound",
    }
}

fn address_table(heading: &str, a: &Address) -> String {
    section(
        heading,
        &[
            ("id", &a.id),
            ("first_name", &a.first_name),
            ("last_name", &a.last_name),
            ("line_1", &a.line_1),
            ("line_2", &a.line_2),
            ("city", &a.city),
            ("state", &a.state),
            ("postal_code", &a.postal_code),
            ("country", &a.country),
            ("phone_number", &a.phone_number),
            ("primary", &a.primary.to_string()),
        ],
    )
}

fn data_page(data: &AuthData, verified: Option<&VerificationStatus>) -> String {
    let rows = table(&[
        ("id", &data.id),
        ("ysws_eligible", &data.ysws_eligible.to_string()),
        ("verification_status", &data.verification_status),
        ("first_name", &data.first_name),
        ("last_name", &data.last_name),
        ("primary_email", &data.primary_email),
        ("slack_id", &data.slack_id),
        ("phone_number", &data.phone_number),
        ("birthday", &data.birthday),
        ("legal_first_name", &data.legal_first_name),
        ("legal_last_name", &data.legal_last_name),
    ]);

    let addresses = if data.addresses.is_empty() {
        "<h3>addresses</h3><p><em>none returned</em></p>".to_string()
    } else {
        indexed("addresses", &data.addresses, address_table)
    };

    let primary = match data.primary_address() {
        Some(a) => address_table("primary_address()", &a),
        None => "<h3>primary_address()</h3><p><em>none</em></p>".to_string(),
    };

    let check = match verified {
        Some(s) => table(&[
            ("check_verified()", verification_label(s)),
            ("is_verified()", &s.is_verified().to_string()),
        ]),
        None => table(&[("check_verified()", "None")]),
    };

    format!("<h2>AuthData</h2>{rows}<h3>checks</h3>{check}{primary}{addresses}")
}

async fn index(req: HttpRequest) -> HttpResponse {
    if login::get_auth_token_with_handling(&req).is_none() {
        return page(LOGIN_BUTTON, "/");
    }

    let data = match data::get_auth_data(&req).await {
        Ok(data) => data,
        Err(e) => {
            let failed = escape(&e.to_string());
            let body = format!(
                "<p><code>get_auth_data</code> failed: <code>{failed}</code></p>{LOGIN_BUTTON}"
            );
            return page(&body, "/");
        }
    };

    let verified = data::check_verified(&req).await;
    page(&data_page(&data, verified.as_ref().ok()), "/")
}

#[derive(Deserialize)]
struct ProjectQuery {
    project: Option<String>,
}

fn project_table(heading: &str, p: &Project) -> String {
    section(
        heading,
        &[
            ("name", &p.name),
            (
                "total_duration_seconds",
                &p.total_duration_seconds.to_string(),
            ),
            ("total_heartbeats", &p.total_heartbeats.to_string()),
            ("languages", &p.languages.join(", ")),
            ("repo_url", p.repo_url.as_deref().unwrap_or_default()),
            (
                "first_heartbeat",
                p.first_heartbeat.as_deref().unwrap_or_default(),
            ),
            (
                "last_heartbeat",
                p.last_heartbeat.as_deref().unwrap_or_default(),
            ),
            (
                "most_recent_heartbeat",
                p.most_recent_heartbeat.as_deref().unwrap_or_default(),
            ),
            ("archived", &p.archived.to_string()),
        ],
    )
}

/// Percent-encoded, so projects with a space or an `&` in the name still link correctly.
fn project_href(name: &str) -> String {
    let mut url = Url::parse("http://localhost/hackatime").expect("static url");
    url.query_pairs_mut().append_pair("project", name);
    format!("/hackatime?{}", url.query().unwrap_or_default())
}

fn user_page(
    user: &HackatimeUser,
    names: &[String],
    selected: Option<&str>,
    details: &str,
) -> String {
    let rows = table(&[
        ("user_id", &user.user_id.to_string()),
        ("emails", &user.emails.join(", ")),
        ("slack_id", user.slack_id.as_deref().unwrap_or_default()),
        (
            "github_username",
            user.github_username.as_deref().unwrap_or_default(),
        ),
        ("trust_factor.trust_level", &user.trust_factor.trust_level),
        (
            "trust_factor.trust_value",
            &user.trust_factor.trust_value.to_string(),
        ),
    ]);

    let projects = if names.is_empty() {
        "<h3>projects</h3><p><em>none returned</em></p>".to_string()
    } else {
        let items = names
            .iter()
            .map(|n| {
                let label = escape(n);
                let label = if Some(n.as_str()) == selected {
                    format!("<strong>{label}</strong>")
                } else {
                    label
                };
                format!("<li><a href=\"{}\">{label}</a></li>", project_href(n))
            })
            .collect::<String>();
        format!("<h3>projects</h3><ul>{items}</ul>")
    };

    format!("<h2>HackatimeUser</h2>{rows}{projects}{details}")
}

/// Details are fetched for the clicked project only — fetching every project on load meant one
/// request per project every time the tab was opened.
async fn hackatime_page(req: HttpRequest, query: Query<ProjectQuery>) -> HttpResponse {
    if hackatime_login::get_hackatime_token_with_handling(&req).is_none() {
        return page(HACKATIME_LOGIN_BUTTON, "/hackatime");
    }

    let user = match hackatime::get_hackatime_user(&req).await {
        Ok(user) => user,
        Err(e) => {
            let failed = escape(&e.to_string());
            let body = format!(
                "<p><code>get_hackatime_user</code> failed: <code>{failed}</code></p>\
                 {HACKATIME_LOGIN_BUTTON}"
            );
            return page(&body, "/hackatime");
        }
    };

    let names = match hackatime::get_hackatime_projects(&req).await {
        Ok(names) => names,
        Err(e) => {
            let body = format!(
                "{}{}",
                user_page(&user, &[], None, ""),
                table(&[("get_hackatime_projects error", &e.to_string())])
            );
            return page(&body, "/hackatime");
        }
    };

    let selected = query.into_inner().project;
    let details = match &selected {
        Some(name) => match hackatime::get_hackatime_project(&req, name).await {
            Ok(project) => project_table("project", &project),
            Err(e) => table(&[("get_hackatime_project error", &e.to_string())]),
        },
        None => String::new(),
    };

    page(
        &user_page(&user, &names, selected.as_deref(), &details),
        "/hackatime",
    )
}

#[derive(Serialize, Deserialize, Default)]
struct Submission {
    #[serde(rename = "Code URL", default)]
    code_url: String,
    #[serde(rename = "Playable URL", default)]
    playable_url: String,
    #[serde(rename = "How did you hear about this?", default)]
    how_did_you_hear: String,
    #[serde(rename = "What are we doing well?", default)]
    doing_well: String,
    #[serde(rename = "How can we improve?", default)]
    how_can_we_improve: String,
    #[serde(rename = "First Name", default)]
    first_name: String,
    #[serde(rename = "Last Name", default)]
    last_name: String,
    #[serde(rename = "Email", default)]
    email: String,
}

/// Fields missing from a submitted form fall back to `Default`, which is the John Hack Club
/// dummy submission — so either form can be submitted on its own.
#[derive(Deserialize, Clone)]
#[serde(default)]
struct AirtableArgs {
    action: String,
    base: String,
    table: String,
    merge_on: String,
    field: String,
    value: String,
    count: usize,
    seconds: u64,
    code_url: String,
    playable_url: String,
    how_did_you_hear: String,
    doing_well: String,
    how_can_we_improve: String,
    first_name: String,
    last_name: String,
    email: String,
}

impl Default for AirtableArgs {
    fn default() -> Self {
        Self {
            action: String::new(),
            base: AIRTABLE_BASE_ID.to_string(),
            table: AIRTABLE_TABLE.to_string(),
            merge_on: "Email".to_string(),
            field: "Email".to_string(),
            value: "john@example.com".to_string(),
            count: 200,
            seconds: 3,
            code_url: "https://example.com/john-hack-club/tale".to_string(),
            playable_url: "https://example.com/john-hack-club/tale/releases/v1.0.0".to_string(),
            how_did_you_hear:
                "John Hack Club heard about it from a sticker on a stranger's laptop, \
                               somewhere between gate B12 and a very long layover."
                    .to_string(),
            doing_well:
                "John Hack Club shipped on the first try, which had never happened before, \
                         and the ship channel cheered anyway."
                    .to_string(),
            how_can_we_improve: "John Hack Club would like the build step to stop lying about \
                                 how long it will take."
                .to_string(),
            first_name: "John".to_string(),
            last_name: "Hack Club".to_string(),
            email: "john@example.com".to_string(),
        }
    }
}

impl AirtableArgs {
    fn submission(&self) -> Submission {
        Submission {
            code_url: self.code_url.clone(),
            playable_url: self.playable_url.clone(),
            how_did_you_hear: self.how_did_you_hear.clone(),
            doing_well: self.doing_well.clone(),
            how_can_we_improve: self.how_can_we_improve.clone(),
            first_name: self.first_name.clone(),
            last_name: self.last_name.clone(),
            email: self.email.clone(),
        }
    }

    fn target(&self) -> Result<AirtableTable, String> {
        if self.base.is_empty() || self.table.is_empty() {
            return Err(table(&[("error", "base id and table are required")]));
        }
        Ok(AirtableTable {
            base_id: self.base.clone(),
            table_id_or_name: self.table.clone(),
        })
    }
}

fn submission_table(heading: &str, s: &Submission) -> String {
    section(
        heading,
        &[
            ("Code URL", &s.code_url),
            ("Playable URL", &s.playable_url),
            ("How did you hear about this?", &s.how_did_you_hear),
            ("What are we doing well?", &s.doing_well),
            ("How can we improve?", &s.how_can_we_improve),
            ("First Name", &s.first_name),
            ("Last Name", &s.last_name),
            ("Email", &s.email),
        ],
    )
}

fn input(name: &str, placeholder: &str, value: &str) -> String {
    format!(
        "<input name=\"{name}\" placeholder=\"{placeholder}\" value=\"{}\">",
        attr(value)
    )
}

fn button(action: &str, label: &str) -> String {
    format!("<button name=\"action\" value=\"{action}\" type=\"submit\">{label}</button>")
}

fn form(action: &str, fields: &[(&str, &str, &str)], buttons: &str) -> String {
    let inputs = fields
        .iter()
        .map(|(n, p, v)| input(n, p, v))
        .collect::<String>();
    format!("<form method=\"post\" action=\"{action}\">{inputs}{buttons}</form>")
}

async fn find_result(a: &AirtableArgs) -> String {
    let target = match a.target() {
        Ok(target) => target,
        Err(e) => return e,
    };

    // Tracked like a spam request, so single fetches show up in the requests table too.
    let id = start_attempt("queue");
    let found =
        airtable::find_records::<Submission>(target, a.field.clone(), a.value.clone()).await;

    match found {
        Ok(records) => {
            finish_attempt(
                id,
                AttemptStatus::Done(records.len(), snippet(&records, &a.value)),
            );
            if records.is_empty() {
                "<p><em>none returned</em></p>".to_string()
            } else {
                indexed("records", &records, submission_table)
            }
        }
        Err(e) => {
            finish_attempt(id, AttemptStatus::Failed(format!("{e:#}")));
            table(&[("error", &e.to_string())])
        }
    }
}

async fn upsert_result(a: &AirtableArgs) -> String {
    let target = match a.target() {
        Ok(target) => target,
        Err(e) => return e,
    };

    let fields = a
        .merge_on
        .split(',')
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
        .collect::<Vec<_>>();
    let sent = submission_table("sent record", &a.submission());

    match airtable::upsert_records(target, vec![a.submission()], fields).await {
        Ok(()) => format!("{}{sent}", table(&[("upsert_records", "ok")])),
        Err(e) => format!("{}{sent}", table(&[("error", &e.to_string())])),
    }
}

enum AttemptStatus {
    InFlight,
    Done(usize, String),
    /// A 429 read off the wire, so the status is not in doubt.
    Limited {
        retry_after: Option<String>,
        body: String,
    },
    Failed(String),
}

struct Attempt {
    id: usize,
    /// `queue` went through `find_records`; `direct` bypassed the queue handler.
    via: &'static str,
    started: Instant,
    elapsed: Option<Duration>,
    status: AttemptStatus,
}

static ATTEMPTS: LazyLock<Mutex<Vec<Attempt>>> = LazyLock::new(|| Mutex::new(Vec::new()));
static NEXT_ATTEMPT_ID: AtomicUsize = AtomicUsize::new(1);
/// Spam loops that have not finished launching their requests yet.
static SPAMS_RUNNING: AtomicUsize = AtomicUsize::new(0);

fn start_attempt(via: &'static str) -> usize {
    let id = NEXT_ATTEMPT_ID.fetch_add(1, Ordering::Relaxed);
    ATTEMPTS.lock().unwrap().push(Attempt {
        id,
        via,
        started: Instant::now(),
        elapsed: None,
        status: AttemptStatus::InFlight,
    });
    id
}

fn finish_attempt(id: usize, status: AttemptStatus) {
    let mut attempts = ATTEMPTS.lock().unwrap();
    if let Some(attempt) = attempts.iter_mut().find(|a| a.id == id) {
        attempt.elapsed = Some(attempt.started.elapsed());
        attempt.status = status;
    }
}

fn launch_find(a: &AirtableArgs) {
    let id = start_attempt("queue");

    let target = AirtableTable {
        base_id: a.base.clone(),
        table_id_or_name: a.table.clone(),
    };
    // Unique value per request so no layer can serve a cached response.
    let (field, value) = (a.field.clone(), format!("{}-{id}", a.value));

    let call = actix_web::rt::spawn(async move {
        let query = value.clone();
        airtable::find_records::<Submission>(target, field, value)
            .await
            .map(|records| (records.len(), snippet(&records, &query)))
            .map_err(|e| format!("{e:#}"))
    });

    actix_web::rt::spawn(async move {
        finish_attempt(
            id,
            match call.await {
                Ok(Ok((found, snippet))) => AttemptStatus::Done(found, snippet),
                Ok(Err(e)) => AttemptStatus::Failed(e),
                Err(_) => AttemptStatus::Failed("request task panicked".to_string()),
            },
        );
    });
}

/// The same GET `find_records` builds, sent straight through reqwest. This skips the airtable
/// queue handler, so it is subject to neither the 4/s per-base gate nor the automatic 429 retry:
/// a rate limit is reported as it arrives instead of being absorbed and retried.
fn launch_probe(a: &AirtableArgs) {
    let id = start_attempt("direct");

    let url = format!(
        "{AIRTABLE_API}/v0/{}/{}",
        a.base,
        a.table.replace(' ', "%20")
    );
    // Unique value per request so no layer can serve a cached response.
    let filter = format!("{{{}}}='{}-{id}'", a.field, a.value);

    actix_web::rt::spawn(async move {
        let sent = get_reqwest_client()
            .get(&url)
            .query(&[("filterByFormula", &filter)])
            .bearer_auth(airtable_token())
            .send()
            .await;

        let status = match sent {
            Ok(response) => {
                let code = response.status();
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                let body = response.text().await.unwrap_or_default();

                if code == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    AttemptStatus::Limited {
                        retry_after,
                        body: clip(&body, 120),
                    }
                } else if code.is_success() {
                    let found = serde_json::from_str::<FindRecordsBody>(&body)
                        .map(|parsed| parsed.records.len())
                        .unwrap_or_default();
                    AttemptStatus::Done(found, format!("HTTP {}", code.as_u16()))
                } else {
                    AttemptStatus::Failed(format!("HTTP {}: {}", code.as_u16(), clip(&body, 120)))
                }
            }
            Err(e) => AttemptStatus::Failed(format!("{e:#}")),
        };

        finish_attempt(id, status);
    });
}

#[derive(Deserialize)]
struct FindRecordsBody {
    records: Vec<serde_json::Value>,
}

/// Fires `count` requests spread evenly over `seconds`, without blocking the response.
fn spam_find(a: AirtableArgs) {
    spam(a, launch_find);
}

/// `spam_find` through the queue, but bypassing it — this one actually reaches the requested rate.
fn spam_probe(a: AirtableArgs) {
    spam(a, launch_probe);
}

fn spam(a: AirtableArgs, launch: fn(&AirtableArgs)) {
    let interval = Duration::from_micros(match a.count {
        0 => return,
        count => a.seconds * 1_000_000 / count as u64,
    });

    SPAMS_RUNNING.fetch_add(1, Ordering::Relaxed);
    actix_web::rt::spawn(async move {
        for _ in 0..a.count {
            launch(&a);
            actix_web::rt::time::sleep(interval).await;
        }
        SPAMS_RUNNING.fetch_sub(1, Ordering::Relaxed);
    });
}

/// Spam requests query a unique value, so an empty result is normal — say what was asked for
/// instead of rendering an empty detail cell.
fn snippet(records: &[Submission], query: &str) -> String {
    let Some(record) = records.first() else {
        return format!("no match for {query}");
    };
    clip(&serde_json::to_string(record).unwrap_or_default(), 60)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        text.chars().take(max).collect::<String>() + "…"
    } else {
        text.to_string()
    }
}

fn is_rate_limited(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "429",
        "rate limit",
        "rate_limit",
        "ratelimit",
        "too many requests",
        "quota",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// Events over the span they happened in. `n` events between the first and the last are `n - 1`
/// intervals, so dividing by `n` reports a rate a burst never actually sustained.
#[derive(Default)]
struct Window {
    count: usize,
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Window {
    fn add(&mut self, at: Instant) {
        self.count += 1;
        self.first = Some(self.first.map_or(at, |f: Instant| f.min(at)));
        self.last = Some(self.last.map_or(at, |l: Instant| l.max(at)));
    }

    fn per_second(&self) -> String {
        match (self.first, self.last) {
            (Some(first), Some(last)) if self.count > 1 && last > first => {
                let rate = (self.count - 1) as f64 / (last - first).as_secs_f64();
                format!("{rate:.2}")
            }
            _ => "n/a".to_string(),
        }
    }
}

/// Returns the rendered summary and how many requests are still in flight. Ongoing requests get
/// their own table; the finished one lists every error but cuts successes off after
/// `OK_ROWS_SHOWN`.
fn attempts_section() -> (String, usize) {
    let attempts = ATTEMPTS.lock().unwrap();
    if attempts.is_empty() {
        return ("<p><em>no requests yet</em></p>".to_string(), 0);
    }

    let (mut ongoing, mut ok, mut errors, mut limited) = (0, 0, 0, 0);
    // Sends and arrivals are separate timelines: mixing them is what makes a serialized queue
    // look like it beat a rate limit it never reached.
    let (mut sent, mut arrived, mut accepted, mut rejected) = (
        Window::default(),
        Window::default(),
        Window::default(),
        Window::default(),
    );
    let mut by_error: BTreeMap<String, usize> = BTreeMap::new();
    let (mut ongoing_rows, mut finished_rows) = (String::new(), String::new());

    for attempt in attempts.iter().rev() {
        sent.add(attempt.started);
        let done_at = attempt.elapsed.map(|taken| attempt.started + taken);
        if let Some(at) = done_at {
            arrived.add(at);
        }
        let elapsed = attempt
            .elapsed
            .unwrap_or_else(|| attempt.started.elapsed())
            .as_millis();

        let (state, detail) = match &attempt.status {
            AttemptStatus::InFlight => {
                ongoing += 1;
                ongoing_rows.push_str(&format!(
                    "<tr><td>{}</td><td>{}</td><td>{elapsed}</td></tr>",
                    attempt.id, attempt.via
                ));
                continue;
            }
            AttemptStatus::Done(found, snippet) => {
                ok += 1;
                if let Some(at) = done_at {
                    accepted.add(at);
                }
                if ok > OK_ROWS_SHOWN {
                    continue;
                }
                ("ok", format!("{found}: {snippet}"))
            }
            AttemptStatus::Limited { retry_after, body } => {
                errors += 1;
                limited += 1;
                if let Some(at) = done_at {
                    rejected.add(at);
                }
                let detail = match retry_after {
                    Some(after) => format!("HTTP 429 (retry-after: {after}): {body}"),
                    None => format!("HTTP 429: {body}"),
                };
                *by_error.entry(detail.clone()).or_default() += 1;
                ("<strong>RATE LIMITED</strong>", detail)
            }
            AttemptStatus::Failed(e) => {
                errors += 1;
                *by_error.entry(e.clone()).or_default() += 1;
                if is_rate_limited(e) {
                    limited += 1;
                    if let Some(at) = done_at {
                        rejected.add(at);
                    }
                    ("<strong>RATE LIMITED</strong>", e.clone())
                } else {
                    ("error", e.clone())
                }
            }
        };

        finished_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{elapsed}</td><td>{state}</td><td>{}</td></tr>",
            attempt.id,
            attempt.via,
            escape(&detail)
        ));
    }

    if ok > OK_ROWS_SHOWN {
        let hidden = ok - OK_ROWS_SHOWN;
        finished_rows.push_str(&format!(
            "<tr><td colspan=\"5\"><em>{hidden} more ok hidden</em></td></tr>"
        ));
    }
    if ongoing_rows.is_empty() {
        ongoing_rows = "<tr><td colspan=\"3\"><em>none</em></td></tr>".to_string();
    }
    if finished_rows.is_empty() {
        finished_rows = "<tr><td colspan=\"5\"><em>none</em></td></tr>".to_string();
    }

    let loud = |hot: &'static str, cold: &'static str, n: usize| if n > 0 { hot } else { cold };
    let summary = table(&[
        ("requests", &attempts.len().to_string()),
        ("ongoing", &ongoing.to_string()),
        ("sent/second", &sent.per_second()),
        ("responses/second (any status)", &arrived.per_second()),
        ("accepted/second (2xx)", &accepted.per_second()),
        ("turned away/second", &rejected.per_second()),
        ("completed ok", &ok.to_string()),
        ("errors", &errors.to_string()),
        (
            loud(
                "RATE LIMITED (confirmed)",
                "rate limited (confirmed)",
                limited,
            ),
            &limited.to_string(),
        ),
    ]);

    let breakdown = if by_error.is_empty() {
        String::new()
    } else {
        let rows = by_error
            .iter()
            .map(|(message, count)| {
                let tag = if is_rate_limited(message) {
                    "<strong>RATE LIMITED</strong>"
                } else {
                    "error"
                };
                format!(
                    "<tr><td>{count}</td><td>{tag}</td><td>{}</td></tr>",
                    escape(message)
                )
            })
            .collect::<String>();
        format!("<table><tr><th>count</th><th>kind</th><th>message</th></tr>{rows}</table>")
    };

    let queue_note = "<p><em><code>spam find_records</code> is capped by the client, not by \
         Airtable: the queue handler dispatches each request concurrently but admits at most \
         4/second per base, so the offered rate is held at that gate however high \
         <code>count</code> and <code>seconds</code> go. A 429 that slips past it is retried \
         inside the handler, so it shows up here as a slower request rather than an error. \
         <strong>probe rate limit</strong> bypasses the queue, so <em>sent/second</em> is the \
         rate offered and <em>accepted/second</em> is what Airtable let through.</em></p>";

    let plot = attempts_plot(&attempts);
    let body = format!(
        "{summary}{plot}{breakdown}{queue_note}\
         <div style=\"display:flex;gap:1rem;align-items:flex-start\">\
         <div style=\"flex:1;min-width:0\"><h3>ongoing</h3>\
         <table><tr><th>#</th><th>via</th><th>ms</th></tr>{ongoing_rows}</table></div>\
         <div style=\"flex:2;min-width:0\"><h3>finished</h3>\
         <table><tr><th>#</th><th>via</th><th>ms</th><th>state</th><th>detail</th></tr>\
         {finished_rows}</table></div></div>"
    );
    (body, ongoing)
}
#[derive(Default)]
struct PageState {
    args: AirtableArgs,
    find_out: String,
    upsert_out: String,
}

static PAGE: LazyLock<Mutex<PageState>> = LazyLock::new(|| Mutex::new(PageState::default()));

async fn airtable_get() -> HttpResponse {
    render_airtable()
}

/// The requests tables on their own, so a spam in progress can be polled without navigating —
/// reloading the whole page threw away the scroll position every second.
async fn airtable_requests() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(requests_fragment())
}

/// Request number against time, as cumulative step lines. Slope is the rate, which is the whole
/// point: a serialized queue draws a straight ramp, and a rate limit draws the accepted line
/// flattening onto the 5/s guide while `sent` keeps climbing away from it.
///
/// Two identity colors only. Accepted-vs-turned-away is a pass/fail pair, so it wants status
/// tokens, but status green vs status critical measures OKLab dE 4.1 under deuteranopia — a pair
/// dichromats cannot separate. The accepted series therefore takes a blue snapped to Nord's own
/// blue hue (254 deg) at dE 22.7, and the alarm keeps the reserved critical red. Both are direct
/// labelled, so identity never rests on hue alone.
fn attempts_plot(attempts: &[Attempt]) -> String {
    const W: f64 = 760.0;
    const H: f64 = 300.0;
    const PAD_L: f64 = 52.0;
    const PAD_R: f64 = 150.0;
    const PAD_T: f64 = 14.0;
    const PAD_B: f64 = 36.0;
    /// Airtable's documented per-base ceiling, drawn as a guide — not measured here.
    const LIMIT_RPS: f64 = 5.0;
    const ACCEPTED: &str = "#4e8dd8";
    const TURNED_AWAY: &str = "#d03b3b";
    const REFERENCE: &str = "#616E88";
    const GRID: &str = "#3B4252";
    const INK: &str = "#D8DEE9";
    const SURFACE: &str = "#2E3440";

    let Some(t0) = attempts.iter().map(|a| a.started).min() else {
        return String::new();
    };

    let (mut sent, mut accepted, mut turned_away) = (Vec::new(), Vec::new(), Vec::new());
    for attempt in attempts {
        sent.push((attempt.started - t0).as_secs_f64());
        let Some(taken) = attempt.elapsed else {
            continue;
        };
        let done = (attempt.started + taken - t0).as_secs_f64();
        match &attempt.status {
            AttemptStatus::Done(..) => accepted.push(done),
            AttemptStatus::Limited { .. } | AttemptStatus::Failed(_) => turned_away.push(done),
            AttemptStatus::InFlight => {}
        }
    }
    sent.sort_by(f64::total_cmp);
    accepted.sort_by(f64::total_cmp);
    turned_away.sort_by(f64::total_cmp);

    let plot_w = W - PAD_L - PAD_R;
    let plot_h = H - PAD_T - PAD_B;
    // Completions outlive the last send by the whole drain time, so the window has to cover
    // every series — sizing it on `sent` alone runs the outcome lines off the right edge.
    let t_max = [
        sent.last().copied(),
        accepted.last().copied(),
        turned_away.last().copied(),
    ]
    .into_iter()
    .flatten()
    .fold(0.25f64, f64::max);
    let n_max = sent.len().max(1) as f64;
    let x = |t: f64| PAD_L + (t / t_max) * plot_w;
    let y = |n: f64| PAD_T + plot_h - (n / n_max) * plot_h;

    let step_path = |times: &[f64]| {
        let mut d = format!("M{:.1},{:.1}", x(0.0), y(0.0));
        for (i, t) in times.iter().enumerate() {
            d.push_str(&format!(" H{:.1} V{:.1}", x(*t), y(i as f64 + 1.0)));
        }
        // Hold the level to the end of the window — a cumulative count does not stop existing
        // when its events do, and `sent` going flat early is exactly what the drain tail is.
        d.push_str(&format!(" H{:.1}", x(t_max)));
        d
    };

    let mut svg = String::new();

    // Grid and ticks stay recessive: one ink, 1px, no fill.
    for i in 0..=5 {
        let t = t_max * i as f64 / 5.0;
        svg.push_str(&format!(
            "<line x1=\"{0:.1}\" y1=\"{PAD_T}\" x2=\"{0:.1}\" y2=\"{1:.1}\" stroke=\"{GRID}\"/>\
             <text x=\"{0:.1}\" y=\"{2:.1}\" fill=\"{REFERENCE}\" text-anchor=\"middle\">{3:.1}s</text>",
            x(t),
            PAD_T + plot_h,
            PAD_T + plot_h + 14.0,
            t
        ));
    }
    for i in 0..=4 {
        let n = n_max * i as f64 / 4.0;
        svg.push_str(&format!(
            "<line x1=\"{PAD_L}\" y1=\"{0:.1}\" x2=\"{1:.1}\" y2=\"{0:.1}\" stroke=\"{GRID}\"/>\
             <text x=\"{2:.1}\" y=\"{3:.1}\" fill=\"{REFERENCE}\" text-anchor=\"end\">{4}</text>",
            y(n),
            PAD_L + plot_w,
            PAD_L - 8.0,
            y(n) + 3.5,
            n.round() as usize
        ));
    }

    // The documented ceiling, clipped to whichever axis it leaves first. Drawn last, over the
    // series: a limiter working perfectly puts the accepted line exactly on this guide, and a
    // guide hidden under the data is useless in the one case the test exists to show.
    let guide_t = (n_max / LIMIT_RPS).min(t_max);
    let guide = format!(
        "<path d=\"M{:.1},{:.1} L{:.1},{:.1}\" fill=\"none\" stroke=\"{REFERENCE}\" \
         stroke-width=\"1.5\" stroke-dasharray=\"5 4\"><title>5/s — Airtable's documented \
         per-base limit</title></path>",
        x(0.0),
        y(0.0),
        x(guide_t),
        y(LIMIT_RPS * guide_t)
    );

    // Offered load is a reference, not an identity slot, so it wears the recessive ink.
    svg.push_str(&format!(
        "<path d=\"{}\" fill=\"none\" stroke=\"{REFERENCE}\" stroke-width=\"1.5\" \
         stroke-dasharray=\"1 3\" stroke-linecap=\"round\"><title>sent: {} requests \
         offered</title></path>",
        step_path(&sent),
        sent.len()
    ));

    // Each identity series is drawn on a surface-colored ring so crossings stay readable.
    for (times, color, dash, label) in [
        (&turned_away, TURNED_AWAY, "6 3", "turned away"),
        (&accepted, ACCEPTED, "none", "accepted (2xx)"),
    ] {
        if times.is_empty() {
            continue;
        }
        let d = step_path(times);
        svg.push_str(&format!(
            "<path d=\"{d}\" fill=\"none\" stroke=\"{SURFACE}\" stroke-width=\"5\" \
             stroke-linejoin=\"round\"/>\
             <path d=\"{d}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"2\" \
             stroke-dasharray=\"{dash}\" stroke-linecap=\"round\" stroke-linejoin=\"round\">\
             <title>{label}: {} requests</title></path>",
            times.len()
        ));
    }
    svg.push_str(&guide);

    // Direct labels, pushed apart so a flat run cannot stack them on top of each other.
    let mut labels: Vec<(f64, &str, &str, &str, usize)> = vec![
        (y(sent.len() as f64), REFERENCE, "1 3", "sent", sent.len()),
        (
            y(accepted.len() as f64),
            ACCEPTED,
            "none",
            "accepted",
            accepted.len(),
        ),
        (
            y(turned_away.len() as f64),
            TURNED_AWAY,
            "6 3",
            "turned away",
            turned_away.len(),
        ),
    ];
    labels.sort_by(|a, b| a.0.total_cmp(&b.0));
    for i in 1..labels.len() {
        let floor = labels[i - 1].0 + 13.0;
        if labels[i].0 < floor {
            labels[i].0 = floor;
        }
    }
    for (at, color, dash, label, count) in labels {
        let swatch = PAD_L + plot_w + 8.0;
        svg.push_str(&format!(
            "<line x1=\"{swatch:.1}\" y1=\"{at:.1}\" x2=\"{:.1}\" y2=\"{at:.1}\" \
             stroke=\"{color}\" stroke-width=\"2\" stroke-dasharray=\"{dash}\" \
             stroke-linecap=\"round\"/>\
             <text x=\"{:.1}\" y=\"{:.1}\" fill=\"{INK}\">{label} {count}</text>",
            swatch + 16.0,
            swatch + 22.0,
            at + 3.5
        ));
    }

    svg.push_str(&format!(
        "<text x=\"{PAD_L}\" y=\"{:.1}\" fill=\"{REFERENCE}\">cumulative requests vs seconds \
         since the first request \u{2192} slope is the rate</text>",
        H - 4.0
    ));

    // A legend as well as the direct labels: it is the only place the two reference lines are
    // explained, and the dashes are what carry identity where hue cannot.
    let legend = [
        (ACCEPTED, "none", "2", "accepted (2xx)"),
        (TURNED_AWAY, "6 3", "2", "turned away (429/error)"),
        (REFERENCE, "1 3", "1.5", "sent"),
        (REFERENCE, "5 4", "1.5", "5/s documented limit"),
    ]
    .iter()
    .map(|(color, dash, width, label)| {
        format!(
            "<span style=\"display:inline-flex;align-items:center;gap:.4rem\">\
             <svg width=\"20\" height=\"8\" aria-hidden=\"true\"><line x1=\"0\" y1=\"4\" \
             x2=\"20\" y2=\"4\" stroke=\"{color}\" stroke-width=\"{width}\" \
             stroke-dasharray=\"{dash}\" stroke-linecap=\"round\"/></svg>{label}</span>"
        )
    })
    .collect::<String>();

    format!(
        "<h3>request number vs time</h3>\
         <div style=\"display:flex;gap:1.2rem;flex-wrap:wrap;color:{REFERENCE};\
         font-size:13px;margin-bottom:.5rem\">{legend}</div>\
         <svg viewBox=\"0 0 {W} {H}\" width=\"100%\" role=\"img\" \
         aria-label=\"cumulative requests sent, accepted and turned away, against time\" \
         style=\"font:10px ui-monospace,monospace;background:{SURFACE};border:1px solid {GRID};\
         border-radius:.3rem;margin-bottom:1.5rem\">{svg}</svg>"
    )
}

/// Carries a stop signal on the wrapper so the poller does not have to parse the tables. A spam
/// between two launches has nothing in flight, so `ongoing` alone would stop the poller early —
/// or never start it, if the page renders before the first request goes out.
fn requests_fragment() -> String {
    let (attempts, ongoing) = attempts_section();
    let busy = ongoing > 0 || SPAMS_RUNNING.load(Ordering::Relaxed) > 0;
    format!("<div data-busy=\"{}\">{attempts}</div>", u8::from(busy))
}

/// Runs the action, then redirects so the rendered page is always a GET — reloading or going
/// back never asks the browser to resend the form.
async fn airtable_post(form: web::Form<AirtableArgs>) -> HttpResponse {
    let args = form.into_inner();
    let (find_out, upsert_out) = match args.action.as_str() {
        "find" => (find_result(&args).await, String::new()),
        "upsert" => (String::new(), upsert_result(&args).await),
        "spam" => {
            spam_find(args.clone());
            (String::new(), String::new())
        }
        "probe" => {
            spam_probe(args.clone());
            (String::new(), String::new())
        }
        "clear" => {
            ATTEMPTS.lock().unwrap().clear();
            (String::new(), String::new())
        }
        _ => (String::new(), String::new()),
    };

    *PAGE.lock().unwrap() = PageState {
        args,
        find_out,
        upsert_out,
    };

    HttpResponse::SeeOther()
        .insert_header(("Location", "/airtable"))
        .finish()
}

fn render_airtable() -> HttpResponse {
    let state = PAGE.lock().unwrap();
    let a = &state.args;

    let find_form = form(
        "/airtable",
        &[
            ("base", "base id", &a.base),
            ("table", "table id or name", &a.table),
            ("field", "field name", &a.field),
            ("value", "field value", &a.value),
            ("count", "spam count", &a.count.to_string()),
            ("seconds", "spam seconds", &a.seconds.to_string()),
        ],
        &format!(
            "{}{}{}",
            button("find", "find_records"),
            button("spam", "spam find_records"),
            button("probe", "probe rate limit")
        ),
    );

    let upsert_form = form(
        "/airtable",
        &[
            ("base", "base id", &a.base),
            ("table", "table id or name", &a.table),
            ("merge_on", "merge on fields (comma separated)", &a.merge_on),
            ("code_url", "Code URL", &a.code_url),
            ("playable_url", "Playable URL", &a.playable_url),
            (
                "how_did_you_hear",
                "How did you hear about this?",
                &a.how_did_you_hear,
            ),
            ("doing_well", "What are we doing well?", &a.doing_well),
            (
                "how_can_we_improve",
                "How can we improve?",
                &a.how_can_we_improve,
            ),
            ("first_name", "First Name", &a.first_name),
            ("last_name", "Last Name", &a.last_name),
            ("email", "Email", &a.email),
        ],
        &button("upsert", "upsert_records"),
    );

    let controls = form(
        "/airtable",
        &[],
        &format!(
            "{}{}",
            button("refresh", "refresh"),
            button("clear", "clear")
        ),
    );

    page(
        &format!(
            "<h2>find_records</h2>{find_form}{}\
             <h2>upsert_records</h2>{upsert_form}{}\
             <h2>requests</h2>{controls}<div id=\"requests\">{}</div>{POLL_SCRIPT}",
            state.find_out,
            state.upsert_out,
            requests_fragment()
        ),
        "/airtable",
    )
}

#[derive(Deserialize, Clone)]
#[serde(default)]
struct SubmissionArgs {
    base: String,
    table: String,
    code_url: String,
    playable_url: String,
    screenshot_url: String,
    description: String,
    hackatime_projects: String,
    slack_username: String,
}

impl Default for SubmissionArgs {
    fn default() -> Self {
        Self {
            base: AIRTABLE_BASE_ID.to_string(),
            table: AIRTABLE_TABLE.to_string(),
            code_url: "https://example.com/john-hack-club/tale".to_string(),
            playable_url: "https://example.com/john-hack-club/tale/releases/v1.0.0".to_string(),
            screenshot_url: "https://example.com/john-hack-club/tale/screenshot.png".to_string(),
            description: "John Hack Club wrote a tale that compiles, which is more than \
                          most tales manage."
                .to_string(),
            hackatime_projects: String::new(),
            slack_username: "johnhackclub".to_string(),
        }
    }
}

impl SubmissionArgs {
    fn project_names(&self) -> Vec<String> {
        self.hackatime_projects
            .split(',')
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect()
    }

    fn target(&self) -> Result<AirtableTable, String> {
        if self.base.is_empty() || self.table.is_empty() {
            return Err(table(&[("error", "base id and table are required")]));
        }
        Ok(AirtableTable {
            base_id: self.base.clone(),
            table_id_or_name: self.table.clone(),
        })
    }
}

#[derive(Default)]
struct SubmissionPageState {
    args: SubmissionArgs,
    out: String,
}

static SUBMISSION_PAGE: LazyLock<Mutex<SubmissionPageState>> =
    LazyLock::new(|| Mutex::new(SubmissionPageState::default()));

#[derive(Serialize)]
struct AdditionalFields {
    #[serde(rename = "Slack Username")]
    slack_username: String,
}

async fn push_result(req: &HttpRequest, a: &SubmissionArgs) -> String {
    let target = match a.target() {
        Ok(target) => target,
        Err(e) => return e,
    };

    match submission::push_unified(
        req,
        target,
        a.code_url.clone(),
        a.playable_url.clone(),
        a.screenshot_url.clone(),
        a.description.clone(),
        a.project_names(),
        AdditionalFields {
            slack_username: a.slack_username.clone(),
        },
    )
    .await
    {
        Ok(()) => table(&[
            ("push_unified", "ok"),
            ("projects sent", &a.project_names().join(", ")),
        ]),
        Err(e) => table(&[("push_unified error", &format!("{e:#}"))]),
    }
}

async fn submission_get(req: HttpRequest) -> HttpResponse {
    render_submission(&missing_logins(&req))
}

/// `push_unified` reads both the Hack Club and the Hackatime cookie, so the tab is useless
/// without either.
fn missing_logins(req: &HttpRequest) -> String {
    let mut out = String::new();
    if login::get_auth_token_with_handling(req).is_none() {
        out.push_str(LOGIN_BUTTON);
    }
    if hackatime_login::get_hackatime_token_with_handling(req).is_none() {
        out.push_str(HACKATIME_LOGIN_BUTTON);
    }
    out
}

async fn submission_post(req: HttpRequest, form: web::Form<SubmissionArgs>) -> HttpResponse {
    let args = form.into_inner();
    let out = push_result(&req, &args).await;

    *SUBMISSION_PAGE.lock().unwrap() = SubmissionPageState { args, out };

    HttpResponse::SeeOther()
        .insert_header(("Location", "/submission"))
        .finish()
}

fn render_submission(logins: &str) -> HttpResponse {
    let state = SUBMISSION_PAGE.lock().unwrap();
    let a = &state.args;

    let push_form = form(
        "/submission",
        &[
            ("base", "base id", &a.base),
            ("table", "table id or name", &a.table),
            ("code_url", "Code URL", &a.code_url),
            ("playable_url", "Playable URL", &a.playable_url),
            ("screenshot_url", "Screenshot", &a.screenshot_url),
            ("description", "Description", &a.description),
            (
                "hackatime_projects",
                "Hackatime project names (comma separated)",
                &a.hackatime_projects,
            ),
            ("slack_username", "Slack Username", &a.slack_username),
        ],
        &button("push", "push_unified"),
    );

    page(
        &format!("{logins}<h2>push_unified</h2>{push_form}{}", state.out),
        "/submission",
    )
}

fn callback_redirect_url(req: &HttpRequest, path: &str) -> String {
    format!(
        "{}://{}{path}",
        req.connection_info().scheme(),
        hc_helper::keys::base_url(),
    )
}

async fn login(req: HttpRequest) -> HttpResponse {
    let redirect_url = callback_redirect_url(&req, "/callback");
    login::handle_login(&req, all_scopes(), redirect_url).await
}

async fn callback(req: HttpRequest, query: Query<CallbackArgs>) -> HttpResponse {
    let redirect_url = callback_redirect_url(&req, "/callback");
    login::handle_callback(&req, query, redirect_url, "/".to_string()).await
}

async fn hackatime_login(req: HttpRequest) -> HttpResponse {
    let redirect_url = callback_redirect_url(&req, "/hackatime/callback");
    hackatime_login::handle_login(&req, all_hackatime_scopes(), redirect_url).await
}

async fn hackatime_callback(req: HttpRequest, query: Query<HackatimeCallbackArgs>) -> HttpResponse {
    let redirect_url = callback_redirect_url(&req, "/hackatime/callback");
    hackatime_login::handle_callback(&req, query, redirect_url, "/hackatime".to_string()).await
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let _ = dotenvy::dotenv();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    if let Err(e) = airtable::spawn_airtable_queue_handler() {
        log::error!("failed to spawn the airtable queue handler: {e:#}");
    }

    log::info!("listening on http://localhost:{PORT}");
    log::info!("register http://localhost:{PORT}/callback as the redirect URI on your HCA app");
    log::info!(
        "register http://localhost:{PORT}/hackatime/callback as the redirect URI on your Hackatime app"
    );

    HttpServer::new(|| {
        App::new()
            .route("/", web::get().to(index))
            .route("/hackatime", web::get().to(hackatime_page))
            .route("/airtable", web::get().to(airtable_get))
            .route("/airtable/requests", web::get().to(airtable_requests))
            .route("/airtable", web::post().to(airtable_post))
            .route("/submission", web::get().to(submission_get))
            .route("/submission", web::post().to(submission_post))
            .route("/login", web::get().to(login))
            .route("/callback", web::get().to(callback))
            .route("/hackatime/login", web::get().to(hackatime_login))
            .route("/hackatime/callback", web::get().to(hackatime_callback))
    })
    .bind(("0.0.0.0", PORT))?
    .run()
    .await
}
