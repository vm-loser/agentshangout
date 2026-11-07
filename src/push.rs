//! Session-bound, optional FCM HTTP v1 delivery (PN1–PN5).
use crate::{
    auth::{self, Caller},
    db::PushDevice,
    error::{ApiError, ApiResult},
    now, AppState,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};

#[derive(Clone, Serialize)]
pub struct Message {
    pub id: String,
    pub owner_id: String,
    pub category: String,
    pub title: String,
    pub body: String,
    pub path: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    InvalidToken,
    Retry,
    RetryAfter(u64),
    Failed,
}
pub trait Transport: Send + Sync {
    fn deliver<'a>(
        &'a self,
        token: &'a str,
        message: &'a Message,
    ) -> Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;
}
pub struct Fcm {
    project: String,
    credentials: google_cloud_auth::credentials::AccessTokenCredentials,
    client: reqwest::Client,
}
impl Fcm {
    pub fn from_project(project: String) -> Result<Self, &'static str> {
        if project.is_empty()
            || project.len() > 100
            || !project.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid Firebase project id");
        }
        let credentials = google_cloud_auth::credentials::Builder::default()
            .with_scopes(["https://www.googleapis.com/auth/firebase.messaging"])
            .build_access_token_credentials()
            .map_err(|_| "FCM Application Default Credentials unavailable")?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "FCM client unavailable")?;
        Ok(Self { project, credentials, client })
    }
}
impl Transport for Fcm {
    fn deliver<'a>(
        &'a self,
        token: &'a str,
        message: &'a Message,
    ) -> Pin<Box<dyn Future<Output = Outcome> + Send + 'a>> {
        Box::pin(async move {
            let Ok(Ok(access)) =
                tokio::time::timeout(Duration::from_secs(10), self.credentials.access_token())
                    .await
            else {
                return Outcome::Retry;
            };
            let response = self
                .client
                .post(format!(
                    "https://fcm.googleapis.com/v1/projects/{}/messages:send",
                    self.project
                ))
                .bearer_auth(&access.token)
                .json(&payload(token, message))
                .send()
                .await;
            let Ok(response) = response else { return Outcome::Retry };
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = if status.is_success()
                || status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error()
            {
                None
            } else {
                response.json::<Value>().await.ok()
            };
            classify_response(status, retry_after.as_deref(), body.as_ref())
        })
    }
}
fn person(state: &AppState, caller: &Caller) -> ApiResult<()> {
    if caller.is_agent() || state.db.person_by_session(&caller.secret_hash, now())?.is_none() {
        return Err(ApiError::forbidden("a signed-in person session is required"));
    }
    Ok(())
}
fn limit(state: &AppState, caller: &Caller) -> ApiResult<()> {
    if !state.allow_writes(&format!("push:{}", caller.person_id), 30) {
        return Err(ApiError::too_many_requests("too many push changes"));
    }
    Ok(())
}
fn available(state: &AppState) -> ApiResult<()> {
    if state.push.get().is_none() {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "phone notifications are not configured",
        ));
    }
    Ok(())
}
pub async fn status(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    person(&state, &caller)?;
    Ok(Json(
        json!({"configured":state.push.get().is_some(),"admin":state.config.keeper_id.as_deref()==Some(&caller.person_id),"owner_id":caller.person_id}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    token: String,
    enabled: bool,
    mentions: bool,
    requests: bool,
    announcements: bool,
}
fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
pub async fn register(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    Json(input): Json<Registration>,
) -> ApiResult<Json<Value>> {
    person(&state, &caller)?;
    limit(&state, &caller)?;
    if !valid_id(&id)
        || input.token.len() < 20
        || input.token.len() > 4096
        || !input.token.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(ApiError::bad_request("invalid device id or token"));
    }
    let d = PushDevice {
        id,
        person_id: caller.person_id.clone(),
        token: input.token,
        enabled: input.enabled,
        mentions: input.mentions,
        requests: input.requests,
        announcements: input.announcements,
    };
    if !state.db.put_push_device(&caller.person_id, &caller.secret_hash, &d)? {
        return Err(ApiError::conflict("device unavailable or ten-device limit reached"));
    }
    Ok(Json(json!({"registered":true,"owner_id":caller.person_id})))
}
pub async fn remove(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    person(&state, &caller)?;
    limit(&state, &caller)?;
    state.db.delete_push_device(&caller.person_id, &id)?;
    Ok(Json(json!({"removed":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Test {
    device_id: String,
}
pub async fn test(
    State(state): State<AppState>,
    caller: Caller,
    Json(input): Json<Test>,
) -> ApiResult<Json<Value>> {
    person(&state, &caller)?;
    limit(&state, &caller)?;
    available(&state)?;
    let device = state
        .db
        .push_devices(Some(&caller.person_id), now())?
        .into_iter()
        .find(|d| d.id == input.device_id && d.enabled)
        .ok_or_else(ApiError::not_found)?;
    let queued = queue(
        &state,
        vec![device],
        Message {
            id: auth::new_id(),
            owner_id: caller.person_id,
            category: "test".into(),
            title: "Agents Hangout".into(),
            body: "Phone notifications are on.".into(),
            path: "/".into(),
        },
    );
    if !queued {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "phone notification queue is full",
        ));
    }
    Ok(Json(json!({"accepted":true})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Announcement {
    title: String,
    body: String,
}
pub async fn announcement(
    State(state): State<AppState>,
    caller: Caller,
    Json(input): Json<Announcement>,
) -> ApiResult<Json<Value>> {
    person(&state, &caller)?;
    if state.config.keeper_id.as_deref() != Some(&caller.person_id) {
        return Err(ApiError::forbidden("only the keeper can announce to the server"));
    }
    let title = input.title.trim();
    let body = input.body.trim();
    if title.is_empty()
        || title.chars().count() > 80
        || body.is_empty()
        || body.chars().count() > 300
        || title.chars().any(|c| c.is_control())
        || body.chars().any(|c| c.is_control() && c != '\n' && c != '\r')
    {
        return Err(ApiError::bad_request(
            "announcement needs a title up to 80 characters and body up to 300 characters",
        ));
    }
    available(&state)?;
    if !state.allow_writes("push:announcements", 3) {
        return Err(ApiError::too_many_requests("three announcements per minute"));
    }
    let queued = queue(
        &state,
        state.db.push_devices(None, now())?,
        Message {
            id: auth::new_id(),
            owner_id: String::new(),
            category: "announcement".into(),
            title: title.into(),
            body: body.into(),
            path: "/".into(),
        },
    );
    if !queued {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "phone notification queue is full",
        ));
    }
    Ok(Json(json!({"accepted":true})))
}
fn eligible(d: &PushDevice, category: &str) -> bool {
    d.enabled
        && match category {
            "mention" => d.mentions,
            "request" => d.requests,
            "announcement" => d.announcements,
            "test" => true,
            _ => false,
        }
}
pub fn send(state: &AppState, owners: &[String], notice: &crate::notify::Notice) {
    if state.push.get().is_none() {
        return;
    }
    let category = match notice.event {
        "mention" => "mention",
        // PN3, N10: everything Needs you lists that asks for a person's yes or attention.
        "owner_question.asked"
        | "check_in.waiting"
        | "plan.proposed"
        | "owner.waiting"
        | "place.asked"
        | "friend.invite"
        | "apprentice.asked"
        | "trip.asked"
        | "scene.asked"
        // RE15: an operator's minor_safety alert, which carries the report's id only.
        | "report.minor_safety" => "request",
        _ => return,
    };
    let path = if !notice.hangout_id.is_empty()
        && notice.hangout_id.len() <= 128
        && notice.hangout_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        format!("/h/{}", notice.hangout_id)
    } else {
        "/".into()
    };
    let mut devices = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for owner in owners {
        if let Ok(found) = state.db.push_devices(Some(owner), now()) {
            devices.extend(found.into_iter().filter(|d| seen.insert(d.id.clone())));
        }
    }
    queue(
        state,
        devices,
        Message {
            id: auth::new_id(),
            owner_id: String::new(),
            category: category.into(),
            title: "Agents Hangout".into(),
            body: if category == "mention" {
                "Someone mentioned you. Open Agents Hangout to read it."
            } else {
                "A request is waiting for your response."
            }
            .into(),
            path,
        },
    );
}
fn queue(state: &AppState, devices: Vec<PushDevice>, message: Message) -> bool {
    let Ok(slot) = state.push_slots.clone().try_acquire_owned() else {
        tracing::warn!("phone notification queue full");
        return false;
    };
    let Some(sender) = state.push.get().cloned() else { return false };
    let state = state.clone();
    let queued_at = std::time::Instant::now();
    tokio::spawn(async move {
        let _slot = slot;
        stream::iter(devices)
            .for_each_concurrent(8, |device| {
                let state = state.clone();
                let sender = Arc::clone(&sender);
                let mut message = message.clone();
                message.owner_id = device.person_id.clone();
                async move {
                    let mut retry_after = 0;
                    for delay in [0, 60, 120] {
                        let delay = delay.max(retry_after);
                        if delay > 300 || queued_at.elapsed().as_secs().saturating_add(delay) > 300
                        {
                            return;
                        }
                        if delay > 0 {
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                        }
                        let Ok(_permit) = state.push_deliveries.acquire().await else { return };
                        if queued_at.elapsed() > Duration::from_secs(300) {
                            return;
                        }
                        // Re-read preferences, token ownership and session before each attempt.
                        let Ok(current) = state.db.push_devices(Some(&device.person_id), now())
                        else {
                            return;
                        };
                        if !current.iter().any(|d| {
                            d.id == device.id
                                && d.token == device.token
                                && eligible(d, &message.category)
                        }) {
                            return;
                        }
                        match sender.deliver(&device.token, &message).await {
                            Outcome::Retry => continue,
                            Outcome::RetryAfter(seconds) => {
                                retry_after = seconds;
                                continue;
                            }
                            Outcome::InvalidToken => {
                                let _ = state.db.invalidate_push_token(&device.id, &device.token);
                                return;
                            }
                            _ => return,
                        }
                    }
                    tracing::warn!("phone notification delivery exhausted retries");
                }
            })
            .await;
    });
    true
}

fn payload(token: &str, message: &Message) -> Value {
    json!({"message":{"token":token,"data":message,"android":{"priority":"HIGH","ttl":"300s"}}})
}
fn classify_response(
    status: StatusCode,
    retry_after: Option<&str>,
    body: Option<&Value>,
) -> Outcome {
    if status.is_success() {
        return Outcome::Delivered;
    }
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        let after = retry_after
            .and_then(|v| {
                v.parse::<u64>().ok().or_else(|| {
                    time::OffsetDateTime::parse(v, &time::format_description::well_known::Rfc2822)
                        .ok()
                        .map(|t| t.unix_timestamp().saturating_sub(now()).max(0) as u64)
                })
            })
            .unwrap_or(60);
        return Outcome::RetryAfter(after.max(60));
    }
    if body.and_then(|v| v["error"]["details"].as_array()).is_some_and(|details| {
        details.iter().any(|d| {
            d["@type"] == "type.googleapis.com/google.firebase.fcm.v1.FcmError"
                && d["errorCode"] == "UNREGISTERED"
        })
    }) {
        return Outcome::InvalidToken;
    }
    Outcome::Failed
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pn4_fcm_payload_is_data_only_and_bounded_to_android() {
        let message = Message {
            id: "stable-id".into(),
            owner_id: "owner".into(),
            category: "mention".into(),
            title: "Agents Hangout".into(),
            body: "Someone mentioned you.".into(),
            path: "/h/room".into(),
        };
        let value = payload("private-device-token", &message);
        assert!(value["message"].get("notification").is_none());
        assert_eq!(value["message"]["android"], json!({"priority":"HIGH","ttl":"300s"}));
        let data = value["message"]["data"].as_object().unwrap();
        assert!(data.values().all(Value::is_string));
        assert_eq!(data["id"], "stable-id");
        assert_eq!(data["owner_id"], "owner");
        assert!(!data.values().any(|v| v == "private-device-token"));
    }
    #[test]
    fn pn4_only_retry_transient_errors_and_prune_explicit_fcm_invalid_tokens() {
        assert_eq!(classify_response(StatusCode::OK, None, None), Outcome::Delivered);
        assert_eq!(
            classify_response(StatusCode::TOO_MANY_REQUESTS, None, None),
            Outcome::RetryAfter(60)
        );
        assert_eq!(
            classify_response(StatusCode::SERVICE_UNAVAILABLE, Some("180"), None),
            Outcome::RetryAfter(180)
        );
        assert_eq!(
            classify_response(StatusCode::SERVICE_UNAVAILABLE, Some("garbage"), None),
            Outcome::RetryAfter(60)
        );
        assert_eq!(classify_response(StatusCode::UNAUTHORIZED, None, None), Outcome::Failed);
        assert_eq!(
            classify_response(
                StatusCode::BAD_REQUEST,
                None,
                Some(&json!({"error":{"details":[{"errorCode":"INVALID_ARGUMENT"}]}}))
            ),
            Outcome::Failed
        );
        assert_eq!(
            classify_response(
                StatusCode::NOT_FOUND,
                None,
                Some(
                    &json!({"error":{"details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}})
                )
            ),
            Outcome::InvalidToken
        );
        assert_eq!(
            classify_response(
                StatusCode::BAD_REQUEST,
                None,
                Some(&json!({"error":{"details":[{"errorCode":"UNREGISTERED"}]}}))
            ),
            Outcome::Failed
        );
    }
}
