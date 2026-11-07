//! RE8 to RE18 (docs/reports.md, from RV6 to RV14 in docs/reports-design.md): the operators'
//! review queue for reports, the actions that close them, and the one place their holds are
//! enforced. Operators are the people with god mode on (W75). Nothing here is a bot tool, wakes a
//! bot, or posts in a room (RE18). RE20's `is_paused` is what every ring and stream asks.

use axum::{
    extract::{rejection::JsonRejection, MatchedPath, Path, State},
    http::Method,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    auth::Caller,
    db::{Closing, QueueReport, Released, ReviewHold, Tell},
    error::{ApiError, ApiResult},
    god_mode, notify, now, ops, AppState,
};

/// RE10: the actions an operator closes a report with.
pub const ACTIONS: [&str; 6] =
    ["dismiss", "remove_content", "warn", "suspend_bot", "restrict_person", "ban_person"];
/// RE12: the holds an operator can lift.
pub const HOLDS: [&str; 3] = ["suspend_bot", "restrict_person", "ban_person"];
/// RE13: the longest note an operator writes.
pub const MAX_NOTE_CHARS: usize = 1000;
/// RE16: how long a closed report keeps its copy and details.
pub const KEEP_COPY_SECS: i64 = 90 * 24 * 60 * 60;
/// RE8: the most open reports the queue reads at once.
const QUEUE_LIMIT: i64 = 1000;

/// RE11: what a bot is told when it, or its person, is held. Never why (RE14).
pub const PAUSED: &str = "an operator has paused this bot, so it can't act for now. Retrying \
                          won't help until an operator lifts it.";

/// RE11: the `/api` routes a banned person can still reach, whatever the method: their account,
/// deleting it and downloading its data (RE19), signing in and out, and seeing their notices.
pub const BANNED_ROUTES: &[&str] = &[
    "GET /api/me",
    "GET /api/account/export",
    "POST /api/me/delete",
    "POST /api/session",
    "DELETE /api/session",
    "GET /api/rules",
    "POST /api/notices/{id}/seen",
];

/// RE11: the write routes a restricted person can still use: signing in, their account and its
/// keys, deleting it or their bots, their own blocks and mutes, and seeing their notices. Every
/// other POST, PUT or PATCH is refused, and every read and DELETE still works.
pub const RESTRICTED_ROUTES: &[&str] = &[
    "POST /api/session",
    "POST /api/sign-in/finish",
    "POST /api/passkeys/login/start",
    "POST /api/passkeys/login/finish",
    "POST /api/passkeys/register/start",
    "POST /api/passkeys/register/finish",
    "POST /api/discord/finish",
    "POST /api/me/delete",
    "POST /api/key/rotate",
    "POST /api/rules/accept",
    "POST /api/notices/{id}/seen",
    "POST /api/blocks",
    "POST /api/mutes",
    "POST /api/agents/{id}/delete",
    "PUT /api/clock",
];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/reports/{id}/close", post(close))
        .route("/api/reports/{id}/release", post(release))
        .route("/api/reports/lift", post(lift))
        .route("/api/notices/{id}/seen", post(seen))
        .route("/api/account/export", get(crate::export::download))
}

// ------------------------------------------------------------------------------------------------
// RE11: the one place holds are enforced.
// ------------------------------------------------------------------------------------------------

/// Where a caller stands with the operators right now.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Standing {
    Clear,
    /// A person who can read, sign in and delete their account, and write nothing else.
    Restricted,
    /// A person who can only reach their account to delete it.
    Banned,
    /// A bot that is suspended, or whose person is restricted or banned.
    Paused,
}

/// RE11: how this caller stands. A bot is paused by its own suspension or by any hold on its person.
pub fn standing(state: &AppState, caller: &Caller) -> ApiResult<Standing> {
    let holds = state.db.holds_on(&caller.person_id, caller.agent_id.as_deref())?;
    Ok(if caller.is_agent() {
        if holds.bot_suspended || holds.restricted || holds.banned {
            Standing::Paused
        } else {
            Standing::Clear
        }
    } else if holds.banned {
        Standing::Banned
    } else if holds.restricted {
        Standing::Restricted
    } else {
        Standing::Clear
    })
}

fn paused() -> ApiError {
    ApiError::forbidden(PAUSED).with_reason("paused_by_operator")
}

fn banned() -> ApiError {
    ApiError::forbidden(
        "an operator has suspended this account. You can still delete it from your account page.",
    )
    .with_reason("banned")
}

fn restricted() -> ApiError {
    ApiError::forbidden(
        "an operator has restricted this account: you can read, and delete your account, but \
         not post, place, build or send your bots anywhere until an operator lifts it",
    )
    .with_reason("restricted")
}

/// RE11: whether `"{method} {route}"` may go ahead for someone standing so, on an `/api` route.
pub fn allows(standing: Standing, method: &Method, route: &str) -> bool {
    let asked = format!("{} {route}", method.as_str());
    match standing {
        Standing::Clear => true,
        Standing::Paused => false,
        Standing::Banned => BANNED_ROUTES.contains(&asked.as_str()),
        Standing::Restricted => {
            !matches!(*method, Method::POST | Method::PUT | Method::PATCH)
                || RESTRICTED_ROUTES.contains(&asked.as_str())
        }
    }
}

fn refusal(standing: Standing) -> ApiError {
    match standing {
        Standing::Paused => paused(),
        Standing::Banned => banned(),
        _ => restricted(),
    }
}

/// RE11: every `/api` request with a credential meets this, beside the rules gate (A109), so
/// none can be missed: a paused bot gets nothing, a banned person only their account, and a
/// restricted person no writes but the few on `RESTRICTED_ROUTES`. A request with no credential
/// never touches the database here (W29), and with no holds anywhere one with a credential costs
/// one query.
pub async fn gate(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let signed = crate::auth::bearer_token(request.headers()).is_some()
        || crate::auth::session_cookie(request.headers()).is_some();
    if !signed
        || !request.uri().path().starts_with("/api/")
        || !state.db.any_holds().unwrap_or(true)
    {
        return next.run(request).await;
    }
    let Ok(caller) = crate::auth::identify(request.headers(), &state) else {
        return next.run(request).await;
    };
    let standing = match standing(&state, &caller) {
        Ok(standing) => standing,
        Err(error) => return error.into_response(),
    };
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path().to_string(), |path| path.as_str().to_string());
    if allows(standing, request.method(), &route) {
        next.run(request).await
    } else {
        refusal(standing).into_response()
    }
}

/// RE11: the same for a bot tool, called by the MCP dispatcher before any tool runs (the bridge
/// calls tools through it too). Any hold refuses every tool.
pub fn require_tool(state: &AppState, caller: &Caller) -> ApiResult<()> {
    if !state.db.any_holds()? {
        return Ok(());
    }
    match standing(state, caller)? {
        Standing::Clear => Ok(()),
        held => Err(refusal(held)),
    }
}

/// RE20: whether this bot is paused: suspended, or its person restricted or banned. Every ring,
/// webhook, OAuth delivery and push to a bot, and every open stream of one, asks this right before
/// it goes on. With no holds anywhere it costs one query. A person's id is never paused, and a
/// failed read counts as paused.
pub fn is_paused(state: &AppState, agent_id: &str) -> bool {
    if !state.db.any_holds().unwrap_or(true) {
        return false;
    }
    state.db.bot_paused(agent_id).unwrap_or(true)
}

/// RE20: a hold just landed on these bots, so their open streams look again and close. Event
/// streams and waiting polls hear it on the bot's own channel, and room streams on the hub.
pub fn close_streams(state: &AppState, bots: &[String]) {
    for agent_id in bots {
        let _ =
            state.bot_events.send(crate::events::Ping { agent_id: agent_id.clone(), event_id: 0 });
    }
    if !bots.is_empty() {
        let _ = state.hub.send(crate::HubEvent {
            hangout_id: String::new(),
            kind: RECHECK,
            data: "{}".into(),
        });
    }
}

/// RE20: the hub event that asks every open room stream to check its caller again. It is never
/// sent on to a page.
pub const RECHECK: &str = "recheck";

/// RE20: whether a caller's open stream may stay open: not a paused bot, nor a banned person.
pub fn may_stream(state: &AppState, caller: &Caller) -> bool {
    if !state.db.any_holds().unwrap_or(true) {
        return true;
    }
    !matches!(standing(state, caller), Ok(Standing::Paused | Standing::Banned) | Err(_))
}

// ------------------------------------------------------------------------------------------------
// RE8, RE9: the queue.
// ------------------------------------------------------------------------------------------------

/// RE8: anyone but an operator gets W75's 404, a bot included, and someone on the list with god
/// mode off is told to turn it on. Asked again on every call, so an action needs it on then (RE18).
fn operator(state: &AppState, caller: &Caller) -> ApiResult<()> {
    if caller.is_agent() || !god_mode::may_use(state, &caller.person_id) {
        return Err(ApiError::not_found());
    }
    if !god_mode::is_on(state, &caller.person_id)? {
        return Err(ApiError::forbidden("turn god mode on first"));
    }
    Ok(())
}

fn person_ref(state: &AppState, id: Option<&str>) -> ApiResult<Value> {
    let Some(id) = id else { return Ok(Value::Null) };
    let name = state.db.person(id)?.map(|person| person.name);
    Ok(json!({ "id": id, "name": name }))
}

fn bot_ref(state: &AppState, id: Option<&str>) -> ApiResult<Value> {
    let Some(id) = id else { return Ok(Value::Null) };
    let name = state.db.agent(id, now())?.map(|bot| bot.name);
    Ok(json!({ "id": id, "name": name }))
}

fn copy_of(report: &QueueReport) -> Value {
    report
        .snapshot
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok())
        .unwrap_or(Value::Null)
}

/// RE10: the bot a report is about, or that wrote what it's about, if any.
fn bot_of(report: &QueueReport, copy: &Value) -> Option<String> {
    match report.target_kind.as_str() {
        "bot" => report.target_id.clone(),
        "message" => copy["speaker_bot_id"].as_str().map(str::to_string),
        "note" => copy["via_bot_id"].as_str().map(str::to_string),
        _ => None,
    }
}

/// RE9: a target's kind and id, or its kind and copy once the id was blanked.
type GroupKey = (String, Option<String>, Option<String>);

/// RE9: a group is one target, or one copy once the target's id was blanked.
fn group_key(report: &QueueReport) -> GroupKey {
    match &report.target_id {
        Some(id) => (report.target_kind.clone(), Some(id.clone()), None),
        None => (report.target_kind.clone(), None, report.snapshot.clone()),
    }
}

/// RE8: `GET /api/reports`, beside A's `POST /api/reports` (in reports.rs, on the same path).
pub async fn queue(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    operator(&state, &caller)?;
    // Minor_safety first, then oldest first, so each group lands where its first report does.
    let mut groups: Vec<(GroupKey, Vec<QueueReport>)> = Vec::new();
    for report in state.db.open_reports(QUEUE_LIMIT)? {
        let key = group_key(&report);
        match groups.iter_mut().find(|(seen, _)| *seen == key) {
            Some((_, reports)) => reports.push(report),
            None => groups.push((key, vec![report])),
        }
    }
    let mut shown = Vec::new();
    for (_, reports) in &groups {
        let first = &reports[0];
        let newest = reports.iter().max_by_key(|r| (r.created_at, r.id)).unwrap_or(first);
        let copy = copy_of(newest);
        let mut reasons: Vec<&str> = Vec::new();
        for report in reports {
            if !reasons.contains(&report.reason.as_str()) {
                reasons.push(&report.reason);
            }
        }
        let room = match &first.hangout_id {
            Some(id) => {
                let name = state.db.hangout(id)?.map(|room| room.name);
                json!({ "id": id, "name": name })
            }
            None => Value::Null,
        };
        let listed = reports
            .iter()
            .map(|report| {
                Ok(json!({
                    "id": report.id,
                    "reason": report.reason,
                    "details": report.details,
                    "by_bot": report.by_bot,
                    "reporter": person_ref(&state, report.reporter_id.as_deref())?,
                    "bot": bot_ref(&state, report.via_bot_id.as_deref())?,
                    "created_at": report.created_at,
                }))
            })
            .collect::<ApiResult<Vec<_>>>()?;
        shown.push(json!({
            "report_id": first.id,
            "target_kind": first.target_kind,
            "target_id": first.target_id,
            "minor_safety": reports.iter().any(|r| r.reason == "minor_safety"),
            "reasons": reasons,
            "reporters": reports.len(),
            "by_bot": reports.iter().filter(|r| r.by_bot).count(),
            "copy": copy,
            "room": room,
            "person": person_ref(&state, first.target_person_id.as_deref())?,
            "bot": bot_ref(&state, bot_of(newest, &copy).as_deref())?,
            "oldest_at": first.created_at,
            "reports": listed,
        }));
    }
    let holds = state
        .db
        .all_holds()?
        .into_iter()
        .map(|hold| {
            let subject = if hold.hold == "suspend_bot" {
                bot_ref(&state, Some(&hold.subject_id))?
            } else {
                person_ref(&state, Some(&hold.subject_id))?
            };
            Ok(json!({
                "hold": hold.hold,
                "id": hold.subject_id,
                "name": subject["name"],
                "person": person_ref(&state, Some(&hold.person_id))?,
                "reason": hold.reason,
                "report_id": hold.report_id,
                "at": hold.at,
            }))
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let held: Vec<Value> = state
        .db
        .held_report_copies()?
        .into_iter()
        .map(|(id, closed_at)| json!({ "report_id": id, "closed_at": closed_at }))
        .collect();
    Ok(Json(json!({ "groups": shown, "holds": holds, "held": held })))
}

// ------------------------------------------------------------------------------------------------
// RE10, RE12, RE13, RE14: actions, lifts and notices.
// ------------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseBody {
    action: String,
    #[serde(default)]
    note: Option<String>,
}

fn clean_note(note: Option<&str>) -> ApiResult<Option<String>> {
    match note.map(str::trim) {
        None | Some("") => Ok(None),
        Some(text) if text.chars().count() > MAX_NOTE_CHARS => {
            Err(ApiError::bad_request(format!("a note can be at most {MAX_NOTE_CHARS} characters")))
        }
        Some(text) => Ok(Some(text.to_string())),
    }
}

/// RE14: the rule a reason falls under, in words.
pub fn rule_words(reason: &str) -> &'static str {
    match reason {
        "harassment" => "harassment",
        "hate" => "hate",
        "sexual" => "sexual content",
        "minor_safety" => "the safety of minors",
        "spam" => "spam",
        "impersonation" => "impersonation",
        _ => "the community rules",
    }
}

/// RE14: what each action tells its person. It names the rule, never who reported.
fn notice_text(action: &str, reason: &str, bot_name: Option<&str>) -> String {
    let rule = rule_words(reason);
    match action {
        "warn" => format!(
            "An operator is warning you under the community rules, about {rule}. Please read them \
             at {}.",
            crate::rules::RULES_PATH
        ),
        "suspend_bot" => format!(
            "An operator suspended your bot {} under the community rules, about {rule}. It can't \
             act until an operator lifts it.",
            bot_name.unwrap_or("of yours")
        ),
        "restrict_person" => format!(
            "An operator restricted your account under the community rules, about {rule}. You can \
             still sign in, read and delete your account, but you can't post, place, build or \
             send your bots anywhere until an operator lifts it."
        ),
        _ => format!(
            "An operator suspended your account under the community rules, about {rule}. You can \
             delete your account. Nothing else is open to you until an operator lifts it."
        ),
    }
}

/// RE10 `remove_content`: deletes the message, card, note or pin for everyone, through the same
/// deletes its writer's own removal uses. Something already gone is fine: the report still closes.
fn remove_content(state: &AppState, report: &QueueReport) -> ApiResult<()> {
    let Some(id) = report.target_id.as_deref() else {
        return Err(ApiError::bad_request("there's nothing left to remove: its id is gone"));
    };
    let room = match report.target_kind.as_str() {
        "message" => {
            let message_id: i64 = id.parse().map_err(|_| ApiError::not_found())?;
            let room = state.db.message(message_id)?.map(|m| m.hangout_id);
            state.db.remove_message(message_id)?;
            room
        }
        "card" => {
            let room = state.db.board_card(id)?.map(|card| card.hangout_id);
            state.db.remove_board_card(id)?;
            room
        }
        "note" => {
            let item = state.db.item_note(id)?.map(|note| note.item_id);
            let room = match item {
                Some(item) => state.db.floor_item(&item)?.map(|item| item.hangout_id),
                None => None,
            };
            state.db.remove_item_note(id)?;
            room
        }
        "pin" => {
            let room = state.db.room_pin(id)?.map(|pin| pin.hangout_id);
            state.db.remove_room_pin(id)?;
            room
        }
        _ => {
            return Err(ApiError::bad_request(
                "remove_content is for a message, a card, a note or a pin",
            ))
        }
    };
    if let Some(room) = room {
        ops::refresh(state, &room);
    }
    Ok(())
}

async fn close(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<i64>,
    body: Result<Json<CloseBody>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    operator(&state, &caller)?;
    let Json(body) = body.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    let action = body.action.trim();
    if !ACTIONS.contains(&action) {
        return Err(ApiError::bad_request(format!("action must be one of {}", ACTIONS.join(", "))));
    }
    let note = clean_note(body.note.as_deref())?;
    let report = state.db.queue_report(id)?.ok_or_else(ApiError::not_found)?;
    if report.status != "open" {
        return Err(ApiError::conflict("that report is already closed"));
    }
    let copy = copy_of(&report);
    let nobody = || ApiError::bad_request("there's nobody to act on: the account is gone");
    let person = report.target_person_id.clone();
    let bot = bot_of(&report, &copy);
    let bot_row = match (action, &bot) {
        ("suspend_bot", Some(id)) => state.db.agent(id, now())?,
        _ => None,
    };
    let (subject, person_id, text) = match action {
        "dismiss" | "remove_content" => (None, person.clone(), None),
        "warn" | "restrict_person" | "ban_person" => {
            let person = person.clone().ok_or_else(nobody)?;
            (Some(person.clone()), Some(person), Some(notice_text(action, &report.reason, None)))
        }
        _ => {
            let bot = bot_row.as_ref().ok_or_else(|| {
                ApiError::bad_request(
                    "suspend_bot is for a bot, or a message or note a bot wrote, that still exists",
                )
            })?;
            let text = notice_text(action, &report.reason, Some(&bot.name));
            (Some(bot.id.clone()), Some(bot.owner_id.clone()), Some(text))
        }
    };
    if action == "remove_content" {
        remove_content(&state, &report)?;
    }
    let hold = match action {
        "suspend_bot" => ReviewHold::Bot { agent_id: subject.as_deref().unwrap_or_default() },
        "restrict_person" | "ban_person" => {
            ReviewHold::Person { person_id: subject.as_deref().unwrap_or_default(), hold: action }
        }
        _ => ReviewHold::None,
    };
    let tell = match (&person_id, &text) {
        (Some(person_id), Some(text)) => Some(Tell { person_id, kind: action, text }),
        _ => None,
    };
    let closing = Closing {
        report_id: id,
        status: if action == "dismiss" { "dismissed" } else { "actioned" },
        action,
        note: note.as_deref(),
        operator_id: &caller.person_id,
        subject_id: subject.as_deref(),
        person_id: person_id.as_deref(),
        hold,
        tell,
    };
    let closed = state
        .db
        .close_report_group(&closing, now())?
        .ok_or_else(|| ApiError::conflict("that report is already closed"))?;
    // RE20: the bots this pauses stop listening now, not at their next request.
    let paused = match action {
        "suspend_bot" => subject.into_iter().collect(),
        "restrict_person" | "ban_person" => match &person_id {
            Some(person) => {
                state.db.agents_of(person, now())?.into_iter().map(|bot| bot.id).collect()
            }
            None => Vec::new(),
        },
        _ => Vec::new(),
    };
    close_streams(&state, &paused);
    Ok(Json(json!({ "closed": closed, "action": action })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiftBody {
    hold: String,
    id: String,
    #[serde(default)]
    note: Option<String>,
}

/// RE12: an operator lifts a suspension, a restriction or a ban. Its person can't.
async fn lift(
    State(state): State<AppState>,
    caller: Caller,
    body: Result<Json<LiftBody>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    operator(&state, &caller)?;
    let Json(body) = body.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    if !HOLDS.contains(&body.hold.as_str()) {
        return Err(ApiError::bad_request(format!("hold must be one of {}", HOLDS.join(", "))));
    }
    let note = clean_note(body.note.as_deref())?;
    if !state.db.lift_hold(&body.hold, &body.id, &caller.person_id, note.as_deref(), now())? {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "lifted": body.hold, "id": body.id })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseBody {
    #[serde(default)]
    note: Option<String>,
}

/// RE16: an operator releases a closed minor_safety report's copy, which then drops on the usual
/// timer: straight away if it closed more than 90 days ago.
async fn release(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<i64>,
    body: Result<Json<ReleaseBody>, JsonRejection>,
) -> ApiResult<Json<Value>> {
    operator(&state, &caller)?;
    let Json(body) = body.map_err(|rejection| ApiError::bad_request(rejection.body_text()))?;
    let note = clean_note(body.note.as_deref())?;
    match state.db.release_report(id, &caller.person_id, note.as_deref(), now())? {
        Released::Released => {}
        Released::NotFound => return Err(ApiError::not_found()),
        Released::StillOpen => return Err(ApiError::conflict("close the report first")),
        Released::NotMinorSafety => {
            return Err(ApiError::bad_request("only a minor_safety report's copy is held"))
        }
        Released::AlreadyReleased => return Err(ApiError::conflict("it's already released")),
    }
    drop_old_copies(&state)?;
    Ok(Json(json!({ "released": id })))
}

/// RE14, RE15: a notice is seen, so it isn't shown again.
async fn seen(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "see a notice")?;
    if !state.db.see_moderation_notice(id, &caller.person_id, now())? {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}

/// RE14, RE15: what `/api/me` and Needs you show a person: their notices not yet seen.
pub fn notices_for(state: &AppState, person_id: &str) -> ApiResult<Vec<Value>> {
    Ok(state.db.moderation_notices(person_id)?.into_iter().map(|notice| json!(notice)).collect())
}

/// RE11: what `/api/me` says about a person's own hold, if any.
pub fn hold_for(state: &AppState, person_id: &str) -> ApiResult<Option<&'static str>> {
    let holds = state.db.holds_on(person_id, None)?;
    Ok(if holds.banned {
        Some("banned")
    } else if holds.restricted {
        Some("restricted")
    } else {
        None
    })
}

/// RE15: a minor_safety report alerts every operator at once, in Needs you and through their own
/// notification if they set one. The alert names the report's id and nothing from it.
pub fn alert_minor_safety(state: &AppState, report_id: i64) -> ApiResult<()> {
    let text = format!("Report {report_id} is about the safety of a minor and needs review now.");
    let alerted = state.db.alert_operators(report_id, &state.config.god_ids, &text, now())?;
    if !alerted.is_empty() {
        let notice =
            notify::Notice { event: "report.minor_safety", hangout_id: String::new(), text };
        notify::send(state, &alerted, &notice);
    }
    Ok(())
}

/// RE16: drops old copies. Called as the server starts and with the usage sweep every minute.
pub fn drop_old_copies(state: &AppState) -> ApiResult<usize> {
    Ok(state.db.drop_old_report_copies(now() - KEEP_COPY_SECS)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RE11: what each standing lets through.
    #[test]
    fn each_standing_lets_through_only_its_own_routes() {
        let (get, post, delete) = (Method::GET, Method::POST, Method::DELETE);
        assert!(allows(Standing::Clear, &post, "/api/hangouts/{id}/messages"));
        assert!(!allows(Standing::Paused, &get, "/api/me"));
        assert!(allows(Standing::Banned, &get, "/api/me"));
        assert!(allows(Standing::Banned, &post, "/api/me/delete"));
        assert!(!allows(Standing::Banned, &get, "/api/hangouts/{id}"));
        assert!(allows(Standing::Restricted, &get, "/api/hangouts/{id}"));
        assert!(allows(Standing::Restricted, &delete, "/api/photos/{id}"));
        assert!(allows(Standing::Restricted, &post, "/api/me/delete"));
        assert!(!allows(Standing::Restricted, &post, "/api/hangouts/{id}/messages"));
        assert!(!allows(Standing::Restricted, &Method::PUT, "/api/floors/{id}"));
    }

    // RE14: a notice names the rule and never who reported.
    #[test]
    fn a_notice_names_the_rule() {
        let text = notice_text("warn", "harassment", None);
        assert!(text.contains("harassment") && text.contains("/rules"), "{text}");
        let text = notice_text("suspend_bot", "spam", Some("Pip"));
        assert!(text.contains("Pip") && text.contains("spam"), "{text}");
    }
}
