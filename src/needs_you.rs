//! Needs you: everything a person has to handle, from every room, in one list, and what was handled
//! in the last day (docs/needs-you.md). It reads what's already there, and adds two things of its
//! own: a bot saying it's stopped somewhere else, and the messages that mentioned the person.

use axum::{
    extract::{Path, State},
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    auth::{self, Caller},
    db::NewOwnerQuestion,
    error::{ApiError, ApiResult},
    notify, now, ops, AppState,
};

/// N5: how much a bot can say about where it's stopped.
pub const MAX_ABOUT_CHARS: usize = 200;
/// N6: a wait nobody cleared stops showing after a day.
pub const SHOWN_SECS: i64 = 86_400;
/// N12: how long a mention waits to be seen.
pub const MENTION_SECS: i64 = 7 * 86_400;
/// N12: how much of a mentioning message the list shows. The room has the rest.
const MENTION_CHARS: usize = 300;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/needs-you", get(needs_you))
        .route("/api/needs-you/answered", get(answered))
        .route("/api/needs-you/waiting/{agent_id}", delete(dismiss))
        .route("/api/needs-you/waiting/{agent_id}/answer", post(answer_wait))
        .route("/api/needs-you/mentions/{id}", delete(clear_mention))
}

// N1, N2
async fn needs_you(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "see what needs you")?;
    let now = now();
    let questions: Vec<Value> = state
        .db
        .open_owner_questions_anywhere(&caller.person_id, now - ops::QUESTION_LAPSE_SECS)?
        .into_iter()
        .map(|(question, room)| {
            json!({
                "id": question.id,
                "bot": question.asker,
                "question": question.question,
                "asked_at": question.created_at,
                "room": { "id": question.hangout_id, "name": room },
            })
        })
        .collect();
    let mut check_ins = Vec::new();
    let mut plans = Vec::new();
    for room in state.db.hangouts_for_person(&caller.person_id)? {
        // Only waiting ones come back for a moment as old as now.
        for check_in in state.db.recent_check_ins(&room.id, now + 1, now)? {
            let addressed = check_in.to_person.as_ref().is_none_or(|to| *to == caller.person_id);
            // A103: none from the other side of a block.
            if check_in.status == "waiting"
                && check_in.asked_by != caller.person_id
                && addressed
                && !crate::blocks::quiet(&state, "check_in", &check_in.id)?
            {
                check_ins.push((check_in, room.name.clone()));
            }
        }
        // N11: the plans a bot's inbox would call its owner's to answer.
        for event in state.db.upcoming_events(&room.id, now)? {
            if !event.rsvps.iter().any(|rsvp| rsvp.person_id == caller.person_id) {
                let mut plan = json!(event);
                plan["room"] = json!({ "id": room.id, "name": room.name });
                plans.push(plan);
            }
        }
    }
    // F58: plans a friend invited them to by name, which aren't in any room of theirs.
    let invited: Vec<Value> = state
        .db
        .invited_plans(&caller.person_id, now)?
        .iter()
        .map(|(event, by)| crate::referrals::invited_plan_json(event, by))
        .collect();
    check_ins.sort_by(|a, b| (a.0.created_at, &a.0.id).cmp(&(b.0.created_at, &b.0.id)));
    let check_ins: Vec<Value> = check_ins
        .into_iter()
        .map(|(check_in, room)| {
            json!({
                "id": check_in.id,
                "asker": check_in.asker,
                "about": check_in.about,
                "needs_human": check_in.needs_human,
                "expires_at": check_in.expires_at,
                "room": { "id": check_in.hangout_id, "name": room },
            })
        })
        .collect();
    let waiting: Vec<Value> = state
        .db
        .owner_waits(&caller.person_id, now - SHOWN_SECS)?
        .into_iter()
        .map(|wait| {
            json!({
                "bot": { "id": wait.agent_id, "name": wait.name },
                "about": wait.about,
                "since": wait.since,
            })
        })
        .collect();
    // N10: what asks for a person's yes on the map, listed here too.
    let requests = crate::spaces::requests_for(&state, &caller.person_id)?;
    let invites = crate::friends::invited(&state, &caller.person_id)?;
    let apprentices = crate::bonds::asking(&state, &caller.person_id)?;
    // N11: in a city, what waits on this person's yes, as a bot's inbox has it for its owner.
    let trips = crate::city::trips_to_answer(&state, &caller, now)?;
    let scenes = crate::city::scenes_to_answer(&state, &caller, now)?
        .into_iter()
        .map(|scene| {
            let house = state.db.house(&scene.house_id)?;
            let mut value = json!(scene);
            value["house_name"] = json!(house.map(|house| house.name));
            Ok(value)
        })
        .collect::<rusqlite::Result<Vec<Value>>>()?;
    let passes = crate::city::passes_to_accept(&state, &caller)?
        .into_iter()
        .map(|pass| {
            let house = state.db.house(&pass.house_id)?;
            let mut value = json!(pass);
            value["house_name"] = json!(house.map(|house| house.name));
            Ok(value)
        })
        .collect::<rusqlite::Result<Vec<Value>>>()?;
    // N12
    let mentions: Vec<Value> = state
        .db
        .person_mentions(&caller.person_id, now - MENTION_SECS)?
        .into_iter()
        .map(|mention| {
            json!({
                "id": mention.id,
                "by": mention.by,
                "text": mention.text.chars().take(MENTION_CHARS).collect::<String>(),
                "at": mention.at,
                "room": { "id": mention.hangout_id, "name": mention.room },
            })
        })
        .collect();
    // JB6: what an owner's bots found or did on the jobs they gave them.
    let job_reports = crate::jobs::reports_for(&state, &caller.person_id, now)?;
    // #647 B: bots an agent drafted for this person, waiting for their approval.
    let bot_setups = crate::bot_setup::for_needs_you(&state, &caller.person_id)?;
    // A111: once a bot of theirs was refused for the community rules, until they accept them.
    let refused = state.rules_refused.bots_of(&caller.person_id);
    let rules = if refused.is_empty()
        || crate::rules::standing(&state, &caller.person_id, now)? != crate::rules::Standing::Needed
    {
        Value::Null
    } else {
        json!({
            "version": crate::rules::version(),
            "url": crate::rules::RULES_PATH,
            "bots": refused,
        })
    };
    // RE14, RE15: what operators told them, and an operator's minor_safety alerts.
    let notices = crate::review::notices_for(&state, &caller.person_id)?;
    let count = usize::from(!rules.is_null())
        + notices.len()
        + questions.len()
        + bot_setups.len()
        + job_reports.len()
        + check_ins.len()
        + waiting.len()
        + requests.len()
        + invites.len()
        + apprentices.len()
        + plans.len()
        + invited.len()
        + trips.len()
        + scenes.len()
        + passes.len()
        + mentions.len();
    Ok(Json(json!({
        "rules": rules,
        "notices": notices,
        "job_reports": job_reports,
        "bot_setups": bot_setups,
        "questions": questions,
        "check_ins": check_ins,
        "waiting": waiting,
        "requests": requests,
        "invites": invites,
        "apprentices": apprentices,
        "plans": plans,
        "invited_plans": invited,
        "trips": trips,
        "scenes": scenes,
        "passes": passes,
        "mentions": mentions,
        "count": count,
    })))
}

// N13: what was handled in the last day, so nothing just disappears from the list.
async fn answered(State(state): State<AppState>, caller: Caller) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "see what was answered")?;
    let now = now();
    let since = now - SHOWN_SECS;
    let questions: Vec<Value> = state
        .db
        .handled_owner_questions(&caller.person_id, since)?
        .into_iter()
        .map(|(question, room, closed)| {
            json!({
                "id": question.id,
                "bot": question.asker,
                "question": question.question,
                "asked_at": question.created_at,
                "answer": question.answer,
                "answered_at": question.answered_at,
                "closed": closed,
                "room": { "id": question.hangout_id, "name": room },
            })
        })
        .collect();
    let mut check_ins = Vec::new();
    for room in state.db.hangouts_for_person(&caller.person_id)? {
        for (check_in, handled_at) in
            state.db.handled_check_ins(&room.id, &caller.person_id, since, now)?
        {
            // What's waiting on this person is under Waiting. Their own side's is here, since only
            // someone on another side can answer it.
            let can_answer = check_in.asked_by != caller.person_id
                && check_in.to_person.as_ref().is_none_or(|to| *to == caller.person_id)
                && (check_in.status == "waiting"
                    || (check_in.answered_by_bot && check_in.status != "expired"));
            let recent = handled_at.unwrap_or(check_in.created_at) >= since;
            if (check_in.status != "waiting" && (recent || can_answer))
                || (check_in.status == "waiting" && check_in.asked_by == caller.person_id)
            {
                check_ins.push(json!({
                    "id": check_in.id,
                    "asker": check_in.asker,
                    "about": check_in.about,
                    "status": check_in.status,
                    "answered_by": check_in.answered_by,
                    "yours": check_in.asked_by == caller.person_id,
                    "asked_at": check_in.created_at,
                    "handled_at": handled_at,
                    "answered_by_bot": check_in.answered_by_bot,
                    "can_answer": can_answer,
                    "room": { "id": room.id, "name": room.name },
                }));
            }
        }
    }
    check_ins.sort_by_key(|check_in| {
        std::cmp::Reverse(check_in["handled_at"].as_i64().or(check_in["asked_at"].as_i64()))
    });
    Ok(Json(json!({ "questions": questions, "check_ins": check_ins })))
}

// N12: one mention cleared, only by the person it mentions.
async fn clear_mention(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "clear a mention")?;
    if !state.db.delete_person_mention(id, &caller.person_id)? {
        return Err(ApiError::not_found());
    }
    Ok(Json(json!({ "ok": true })))
}

// N6: only the bot's owner can dismiss it, and anyone else's bot is as good as not there.
async fn dismiss(
    State(state): State<AppState>,
    caller: Caller,
    Path(agent_id): Path<String>,
) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "dismiss a bot's wait")?;
    let agent = state
        .db
        .agent(&agent_id, now())?
        .filter(|agent| agent.owner_id == caller.person_id)
        .ok_or_else(ApiError::not_found)?;
    state.db.clear_owner_wait(&agent.id)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct WaitAnswer {
    answer: String,
}

// N15: an answer in words, kept as the bot's own question so it reaches the bot the usual way.
async fn answer_wait(
    State(state): State<AppState>,
    caller: Caller,
    Path(agent_id): Path<String>,
    Json(body): Json<WaitAnswer>,
) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "answer a bot's wait")?;
    let now = now();
    let wait = state
        .db
        .owner_waits(&caller.person_id, now - SHOWN_SECS)?
        .into_iter()
        .find(|wait| wait.agent_id == agent_id)
        .ok_or_else(ApiError::not_found)?;
    let answer = ops::clean(&body.answer, "answer", ops::MAX_ABOUT_CHARS)?;
    let room = state
        .db
        .hangouts_for_agent(&wait.agent_id, now)?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::conflict("that bot has no room to hear an answer in"))?;
    let id = auth::new_id();
    let question = NewOwnerQuestion {
        id: &id,
        hangout_id: &room.id,
        agent_id: &wait.agent_id,
        owner_id: &caller.person_id,
        asker: &wait.name,
        question: &wait.about,
        now,
    };
    // Its owner is answering it now, so the limit on open questions doesn't come into it.
    state.db.insert_owner_question(&question, now, i64::MAX)?;
    let answered = ops::answer_owner_question(&state, &caller, &id, &answer)?;
    state.db.clear_owner_wait(&wait.agent_id)?;
    Ok(Json(json!(answered)))
}

/// N5 to N7: a bot says it's waiting for its owner somewhere else, or with no `about` that it isn't.
/// It doesn't count as a write, because a hook calls it on its own.
pub fn waiting_on_owner(
    state: &AppState,
    caller: &Caller,
    about: Option<&str>,
) -> ApiResult<Value> {
    ops::no_house_bot(caller)?;
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only bots wait for their owner"));
    };
    let about = ops::clean_optional(about, "about", MAX_ABOUT_CHARS)?;
    if about.is_empty() {
        state.db.clear_owner_wait(agent_id)?;
        return Ok(json!({ "ok": true, "waiting": false }));
    }
    let now = now();
    if state.db.set_owner_wait(agent_id, &about, now, now - SHOWN_SECS)? {
        // The owner alone, with no room, so the link goes to the map.
        let notice = notify::Notice {
            event: "owner.waiting",
            hangout_id: String::new(),
            text: format!("{} is waiting for you: {about}", caller.name),
        };
        notify::send(state, std::slice::from_ref(&caller.person_id), &notice);
    }
    Ok(json!({ "ok": true, "waiting": true }))
}

/// For `GET /api/me/bots`: each of this person's bots that's waiting for them, by id.
pub fn waits_of(
    state: &AppState,
    person_id: &str,
) -> ApiResult<std::collections::HashMap<String, Value>> {
    Ok(state
        .db
        .owner_waits(person_id, now() - SHOWN_SECS)?
        .into_iter()
        .map(|wait| (wait.agent_id, json!({ "about": wait.about, "since": wait.since })))
        .collect())
}
