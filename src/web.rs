// SPDX-License-Identifier: AGPL-3.0-or-later
//! Web interface: embedded single page app, JSON API and a WebSocket of live
//! events.

use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Form, Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::clock::SharedClock;
use crate::hub::{Event, HubMsg, SharedStatus, Status};
use crate::protocol;
use crate::store::{ChannelRow, ContactRow, Conversation, Message, Store};

const COOKIE: &str = "meshkeep_session";
const SESSION_MAX_AGE: u64 = 30 * 24 * 3600;
const FAILED_LOGIN_DELAY: Duration = Duration::from_secs(1);

const INDEX_HTML: &str = include_str!("../web/index.html");
const LOGIN_HTML: &str = include_str!("../web/login.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE_CSS: &str = include_str!("../web/style.css");
const ICON_SVG: &str = include_str!("../web/icon.svg");

#[derive(Clone)]
pub struct WebState {
    pub store: Store,
    pub hub: mpsc::UnboundedSender<HubMsg>,
    pub events: broadcast::Sender<Event>,
    pub status: SharedStatus,
    pub clock: SharedClock,
    /// `None` means authentication is off.
    pub password: Option<String>,
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/login", get(login_page).post(login))
        .route("/logout", post(logout))
        .route("/app.js", get(|| asset("text/javascript; charset=utf-8", APP_JS)))
        .route("/style.css", get(|| asset("text/css; charset=utf-8", STYLE_CSS)))
        .route("/icon.svg", get(|| asset("image/svg+xml", ICON_SVG)))
        .route("/api/state", get(api_state))
        .route("/api/messages", get(api_messages))
        .route("/api/send", post(api_send))
        .route("/api/events", get(api_events))
        .with_state(state)
}

async fn asset(ctype: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, ctype),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

fn authed(s: &WebState, headers: &HeaderMap) -> bool {
    if s.password.is_none() {
        return true;
    }
    cookie_token(headers)
        .is_some_and(|t| s.store.session_valid(&t, s.clock.now_unix(), SESSION_MAX_AGE))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "Sign in required").into_response()
}

async fn index(State(s): State<WebState>, headers: HeaderMap) -> Response {
    if !authed(&s, &headers) {
        return Redirect::to("/login").into_response();
    }
    html(INDEX_HTML.replace("{{AUTH}}", if s.password.is_some() { "1" } else { "0" }))
}

fn html(body: String) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

async fn login_page(State(s): State<WebState>, headers: HeaderMap) -> Response {
    if s.password.is_none() || authed(&s, &headers) {
        return Redirect::to("/").into_response();
    }
    html(LOGIN_HTML.replace("{{ERROR}}", ""))
}

#[derive(Deserialize)]
struct LoginForm {
    password: String,
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).expect("system random source");
    protocol::hex(&b)
}

async fn login(State(s): State<WebState>, Form(f): Form<LoginForm>) -> Response {
    let Some(pw) = &s.password else {
        return Redirect::to("/").into_response();
    };
    if !ct_eq(f.password.as_bytes(), pw.as_bytes()) {
        s.clock.sleep(FAILED_LOGIN_DELAY).await;
        let page = LOGIN_HTML.replace(
            "{{ERROR}}",
            r#"<p class="error" role="alert">Wrong password.</p>"#,
        );
        return (StatusCode::UNAUTHORIZED, html(page)).into_response();
    }
    let now = s.clock.now_unix();
    s.store.prune_sessions(now, SESSION_MAX_AGE);
    let token = new_token();
    s.store.add_session(&token, now);
    let cookie = format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={SESSION_MAX_AGE}"
    );
    let mut r = Redirect::to("/").into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    r
}

async fn logout(State(s): State<WebState>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie_token(&headers) {
        s.store.delete_session(&t);
    }
    let mut r = Redirect::to("/login").into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("meshkeep_session=; Path=/; Max-Age=0"),
    );
    r
}

#[derive(Serialize)]
struct StateResp {
    status: Status,
    contacts: Vec<ContactRow>,
    channels: Vec<ChannelRow>,
    conversations: Vec<Conversation>,
}

async fn api_state(State(s): State<WebState>, headers: HeaderMap) -> Response {
    if !authed(&s, &headers) {
        return unauthorized();
    }
    let status = s.status.lock().unwrap().clone();
    Json(StateResp {
        status,
        contacts: s.store.contacts(),
        channels: s.store.channels(),
        conversations: s.store.conversations(),
    })
    .into_response()
}

#[derive(Deserialize)]
struct MessagesQuery {
    conv: String,
    before: Option<i64>,
    limit: Option<u32>,
}

async fn api_messages(
    State(s): State<WebState>,
    headers: HeaderMap,
    Query(q): Query<MessagesQuery>,
) -> Response {
    if !authed(&s, &headers) {
        return unauthorized();
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let msgs: Vec<Message> = s.store.messages(&q.conv, q.before, limit);
    Json(msgs).into_response()
}

#[derive(Deserialize)]
pub struct SendReq {
    pub conv: String,
    pub text: String,
}

#[derive(Serialize)]
struct SendResp {
    id: i64,
}

#[derive(Serialize)]
struct ErrResp {
    error: String,
}

fn bad(msg: impl Into<String>) -> Response {
    (StatusCode::BAD_REQUEST, Json(ErrResp { error: msg.into() })).into_response()
}

async fn api_send(
    State(s): State<WebState>,
    headers: HeaderMap,
    Json(req): Json<SendReq>,
) -> Response {
    if !authed(&s, &headers) {
        return unauthorized();
    }
    let (tx, rx) = oneshot::channel();
    let msg = if let Some(h) = req.conv.strip_prefix("dm:") {
        let Some(b) = protocol::unhex(h).filter(|b| b.len() == 6) else {
            return bad("Unknown conversation");
        };
        let mut prefix = [0u8; 6];
        prefix.copy_from_slice(&b);
        HubMsg::SendDm {
            prefix,
            text: req.text,
            reply: tx,
        }
    } else if let Some(i) = req.conv.strip_prefix("ch:") {
        let Ok(idx) = i.parse::<u8>() else {
            return bad("Unknown conversation");
        };
        HubMsg::SendChannel {
            idx,
            text: req.text,
            reply: tx,
        }
    } else {
        return bad("Unknown conversation");
    };
    if s.hub.send(msg).is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "Shutting down").into_response();
    }
    match rx.await {
        Ok(Ok(id)) => Json(SendResp { id }).into_response(),
        Ok(Err(e)) => bad(e),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "Shutting down").into_response(),
    }
}

async fn api_events(
    State(s): State<WebState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    if !authed(&s, &headers) {
        return unauthorized();
    }
    let rx = s.events.subscribe();
    ws.on_upgrade(move |sock| pump_events(sock, rx))
}

async fn pump_events(mut sock: WebSocket, mut rx: broadcast::Receiver<Event>) {
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let text = match ev {
                    Ok(e) => serde_json::to_string(&e).unwrap_or_default(),
                    Err(broadcast::error::RecvError::Lagged(_)) => r#"{"type":"resync"}"#.to_string(),
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if sock.send(WsMessage::Text(text.into())).await.is_err() {
                    return;
                }
            }
            m = sock.recv() => match m {
                Some(Ok(WsMessage::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
        }
    }
}
