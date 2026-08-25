use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web, web::Query};
use hc_helper::auth::data::{self, Address, AuthData, VerificationStatus};
use hc_helper::auth::login::{self, CallbackArgs, Scopes};

const PORT: u16 = 8080;

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

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn row(label: &str, value: &str) -> String {
    let value = if value.is_empty() {
        "<em>empty</em>".to_string()
    } else {
        escape(value)
    };
    format!("<tr><th>{label}</th><td>{value}</td></tr>")
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

fn address_table(heading: &str, address: &Address) -> String {
    let rows = [
        row("id", &address.id),
        row("first_name", &address.first_name),
        row("last_name", &address.last_name),
        row("line_1", &address.line_1),
        row("line_2", &address.line_2),
        row("city", &address.city),
        row("state", &address.state),
        row("postal_code", &address.postal_code),
        row("country", &address.country),
        row("phone_number", &address.phone_number),
        row("primary", &address.primary.to_string()),
    ]
    .concat();

    format!("<h3>{heading}</h3><table>{rows}</table>")
}

fn data_page(data: &AuthData, verified: Option<&VerificationStatus>) -> String {
    let rows = [
        row("id", &data.id),
        row("ysws_eligible", &data.ysws_eligible.to_string()),
        row("verification_status", &data.verification_status),
        row("first_name", &data.first_name),
        row("last_name", &data.last_name),
        row("primary_email", &data.primary_email),
        row("slack_id", &data.slack_id),
        row("phone_number", &data.phone_number),
        row("birthday", &data.birthday),
        row("legal_first_name", &data.legal_first_name),
        row("legal_last_name", &data.legal_last_name),
    ]
    .concat();

    let addresses = if data.addresses.is_empty() {
        "<h3>addresses</h3><p><em>none returned</em></p>".to_string()
    } else {
        data.addresses
            .iter()
            .enumerate()
            .map(|(i, a)| address_table(&format!("addresses[{i}]"), a))
            .collect::<Vec<_>>()
            .concat()
    };

    let primary = match data.primary_address() {
        Some(a) => address_table("primary_address()", &a),
        None => "<h3>primary_address()</h3><p><em>none</em></p>".to_string(),
    };

    let check = match verified {
        Some(status) => [
            row("check_verified()", verification_label(status)),
            row("is_verified()", &status.is_verified().to_string()),
        ]
        .concat(),
        None => row("check_verified()", "None"),
    };

    format!("<h2>AuthData</h2><table>{rows}</table><h3>checks</h3><table>{check}</table>{primary}{addresses}")
}

fn page(body: &str) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(format!(
            "<!doctype html><html><head><meta charset=\"utf-8\">\
             <title>hc_helper auth test</title><style>\
             :root{{--nord0:#2E3440;--nord1:#3B4252;--nord2:#434C5E;--nord3:#4C566A;\
             --nord4:#D8DEE9;--nord6:#ECEFF4;--nord8:#88C0D0;--nord9:#81A1C1;--nord10:#5E81AC;\
             --muted:#616E88}}\
             html{{background:var(--nord0)}}\
             body{{font:15px/1.5 system-ui,sans-serif;max-width:44rem;margin:3rem auto;padding:0 1rem;\
             background:var(--nord0);color:var(--nord4)}}\
             h1,h2,h3{{color:var(--nord6);font-weight:600}}\
             h1{{border-bottom:1px solid var(--nord3);padding-bottom:.5rem}}\
             table{{border-collapse:collapse;width:100%;margin-bottom:1.5rem;\
             border:1px solid var(--nord3);border-radius:.3rem;overflow:hidden}}\
             th,td{{border:1px solid var(--nord3);padding:.4rem .6rem;text-align:left;vertical-align:top}}\
             th{{width:14rem;background:var(--nord1);color:var(--nord9);\
             font-family:ui-monospace,monospace;font-weight:600}}\
             td{{background:var(--nord0);color:var(--nord4);font-family:ui-monospace,monospace;\
             word-break:break-word}}\
             tr:nth-child(even) td{{background:#333A47}}\
             em{{color:var(--muted);font-style:italic}}\
             code{{color:var(--nord8);font-family:ui-monospace,monospace}}\
             a.button{{display:inline-block;background:var(--nord10);color:var(--nord6);text-decoration:none;\
             padding:.7rem 1.4rem;border-radius:.4rem;font-weight:600}}\
             a.button:hover{{background:var(--nord9)}}\
             </style></head><body><h1>hc_helper auth test</h1>{body}</body></html>"
        ))
}

const LOGIN_BUTTON: &str = "<p><a class=\"button\" href=\"/login\">Log in with Hack Club</a></p>";

async fn index(req: HttpRequest) -> HttpResponse {
    if login::get_auth_token_with_handling(&req).is_none() {
        return page(LOGIN_BUTTON);
    }

    let Some(data) = data::get_auth_data(&req).await else {
        return page(&format!(
            "<p><code>get_auth_data</code> returned <code>None</code> \
             (token present but the userinfo request failed).</p>{LOGIN_BUTTON}"
        ));
    };

    let verified = data::check_verified(&req).await;
    page(&data_page(&data, verified.as_ref()))
}

async fn login(req: HttpRequest) -> HttpResponse {
    login::handle_login(&req, all_scopes()).await
}

async fn callback(req: HttpRequest, query: Query<CallbackArgs>) -> HttpResponse {
    login::handle_callback(&req, query, "/".to_string()).await
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let _ = dotenvy::dotenv();
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    log::info!("listening on http://localhost:{PORT}");
    log::info!("register http://localhost:{PORT}/callback as the redirect URI on your HCA app");

    HttpServer::new(|| {
        App::new()
            .route("/", web::get().to(index))
            .route("/login", web::get().to(login))
            .route("/callback", web::get().to(callback))
    })
    .bind(("127.0.0.1", PORT))?
    .run()
    .await
}
