//! What can happen in a hangout. The REST API and the MCP tools both call these functions, so
//! every membership and calendar rule is enforced in one place.

use serde::{Deserialize, Serialize};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{
    auth::{self, Caller},
    city,
    db::{
        Agent, CheckIn, Event, FeedEvent, FloorLeave, Hangout, Mention, Message, NewCheckIn,
        NewEvent, NewMessage, NewOwnerQuestion, OwnerQuestion, Pass, Person, SceneRequest, Trip,
        Visit, Whiteboard,
    },
    error::{ApiError, ApiResult},
    ical, items, looks, notify, now, spaces, AppState, HubEvent,
};

pub const MAX_AGENT_WRITES_PER_MINUTE: u32 = 30;
pub const MAX_NAME_CHARS: usize = 60;
pub const MAX_MESSAGE_CHARS: usize = 4000;
const MAX_TITLE_CHARS: usize = 120;
const MAX_LOCATION_CHARS: usize = 200;
const MAX_NOTE_CHARS: usize = 500;
const MAX_STATUS_CHARS: usize = 120;
const MAX_WHITEBOARD_CHARS: usize = 2000;
/// E22, E31: three minutes. An unread accepted ring holds the owner's next one. A non-owner
/// person's accepted ring holds the next person in that room even after a read. 20 an hour at most.
const MENTION_RING_SECS: i64 = 3 * 60;
/// I85: what a bot can say it's doing. `none` isn't one: it ends what's shown.
const ACTIVITY_KINDS: [&str; 6] =
    ["computer", "coding", "searching", "github", "reviewing", "other"];
const MAX_ACTIVITY_VIA_CHARS: usize = 40;
/// Reports expire instead of being cleared, so an agent that crashes doesn't stay at the desk.
const ACTIVITY_SECS: i64 = 60;
/// I58: waiting on a mention, and thinking after the bot catches up, last this long.
pub const THINKING_SECS: i64 = 2 * 60;
const DEFAULT_DURATION_MINUTES: i64 = 120;
const MAX_DURATION_MINUTES: i64 = 7 * 24 * 60;
const DEFAULT_MIN_YES: i64 = 2;
/// Also the limit for a trip's goal.
pub(crate) const MAX_ABOUT_CHARS: usize = 500;
const DEFAULT_CHECK_IN_MINUTES: i64 = 60;
const MAX_CHECK_IN_MINUTES: i64 = 7 * 24 * 60;
pub(crate) const DAY_SECS: i64 = 24 * 60 * 60;
/// Finished events stay in feeds this long so calendars don't drop them right away.
const FEED_HISTORY_SECS: i64 = 30 * 24 * 60 * 60;
/// A question for a bot's owner that nobody answers lapses. People go away for a few days, so this
/// is longer than a check-in.
pub(crate) const QUESTION_LAPSE_SECS: i64 = 7 * DAY_SECS;
/// However much a bot wants to know, this is as many questions as it can have waiting. Each one
/// pings its owner.
const MAX_OPEN_QUESTIONS: i64 = 5;

/// I110: said when a bot that is in no hangout calls something that needs one.
pub(crate) const NO_HANGOUTS: &str = "you aren't in any hangouts yet; call go_to_room with a hangout_id from where_is or list_city, or ask_owner to add you";

pub fn clean(text: &str, field: &str, max_chars: usize) -> ApiResult<String> {
    let text = text.trim();
    if text.is_empty() {
        return Err(ApiError::bad_request(format!("{field} can't be empty")));
    }
    if text.chars().count() > max_chars {
        return Err(ApiError::bad_request(format!(
            "{field} is too long (limit {max_chars} characters)"
        )));
    }
    Ok(text.to_string())
}

pub(crate) fn clean_optional(
    text: Option<&str>,
    field: &str,
    max_chars: usize,
) -> ApiResult<String> {
    match text.map(str::trim) {
        Some(text) if !text.is_empty() => clean(text, field, max_chars),
        _ => Ok(String::new()),
    }
}

pub fn iso(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .ok()
        .and_then(|at| at.format(&Rfc3339).ok())
        .unwrap_or_default()
}

pub(crate) fn parse_time(text: &str, field: &str) -> ApiResult<i64> {
    OffsetDateTime::parse(text.trim(), &Rfc3339).map(OffsetDateTime::unix_timestamp).map_err(|_| {
        ApiError::bad_request(format!(
            "{field} must be an RFC 3339 time with an offset, like 2026-11-07T18:00:00-06:00"
        ))
    })
}

pub fn publish(state: &AppState, hangout_id: &str, kind: &'static str, data: String) {
    // Sending only fails when nobody is watching, which is fine.
    let _ = state.hub.send(HubEvent { hangout_id: hangout_id.to_string(), kind, data });
}

/// Pings one person and their bots in this room.
fn notify_side(
    state: &AppState,
    hangout_id: &str,
    person_id: &str,
    event: &'static str,
    text: &str,
) -> ApiResult<()> {
    let mut owners = vec![person_id.to_string()];
    // Guests are included here because this names one side. `notify_others` only ever names the
    // people who belong in the room, so nobody's check-ins reach a guest.
    owners.extend(
        state
            .db
            .agents_in(hangout_id, now())?
            .into_iter()
            .chain(state.db.visitors_in(hangout_id, now())?)
            .filter(|agent| agent.owner_id == person_id)
            .map(|agent| agent.id),
    );
    let notice =
        notify::Notice { event, hangout_id: hangout_id.to_string(), text: text.to_string() };
    notify::send(state, &owners, &notice);
    Ok(())
}

/// Pings everyone in the room except one side, which can be empty to reach all of them.
fn notify_others(
    state: &AppState,
    hangout_id: &str,
    except_person: &str,
    event: &'static str,
    text: &str,
) -> ApiResult<()> {
    for person in state.db.people_in(hangout_id)? {
        if person.id != except_person {
            notify_side(state, hangout_id, &person.id, event, text)?;
        }
    }
    Ok(())
}

/// Tells open pages to reload the room after a change that isn't a chat line.
pub(crate) fn refresh(state: &AppState, hangout_id: &str) {
    publish(state, hangout_id, "refresh", "{}".to_string());
}

/// Tells open city maps to reload. A city's stream uses the city id as its topic.
pub(crate) fn refresh_city(state: &AppState, city_id: &str) {
    publish(state, city_id, "refresh", "{}".to_string());
}

pub(crate) fn announce(state: &AppState, hangout_id: &str, body: &str) -> ApiResult<()> {
    announced(state, hangout_id, body).map(|_| ())
}

/// `announce`, giving back the line it wrote.
pub(crate) fn announced(state: &AppState, hangout_id: &str, body: &str) -> ApiResult<Message> {
    announced_about(state, hangout_id, None, body)
}

/// `announced`, with the line tied to the person it's about, so whoever blocked them doesn't see it
/// (A101). Said by the Hangout all the same.
fn announced_about(
    state: &AppState,
    hangout_id: &str,
    about: Option<&str>,
    body: &str,
) -> ApiResult<Message> {
    let message = state.db.insert_message(&NewMessage {
        hangout_id,
        person_id: about,
        agent_id: None,
        speaker: "Hangout",
        kind: "system",
        body,
        now: now(),
    })?;
    publish_message(state, &message);
    Ok(message)
}

/// Whether guests can be here: a Porch, or a public floor.
fn has_guests(state: &AppState, hangout_id: &str) -> ApiResult<bool> {
    Ok(state.db.house_for_room(hangout_id)?.is_some_and(|house| house.porch_id == hangout_id)
        || state.db.floor(hangout_id)?.is_some_and(|floor| floor.access == "public"))
}

/// For a line that repeats a plan or a check-in. Guests can't see those, so a Porch or a public
/// floor gets no line at all: the people who belong there have the plan and check-in panels.
pub(crate) fn announce_in_house_only(
    state: &AppState,
    hangout_id: &str,
    body: &str,
) -> ApiResult<()> {
    if has_guests(state, hangout_id)? {
        return Ok(());
    }
    announce(state, hangout_id, body)
}

pub(crate) fn publish_message(state: &AppState, message: &Message) {
    let data = serde_json::to_string(message).unwrap_or_default();
    publish(state, &message.hangout_id, "message", data);
}

/// The room, if the caller is in it. Anyone else gets the same answer as for a hangout that
/// doesn't exist, unless they were on this floor and left: then the answer says when and why
/// (S62). A guest on a Porch is in it too, so use `no_guests` for anything a guest mustn't do.
pub fn require_room(state: &AppState, caller: &Caller, hangout_id: &str) -> ApiResult<Hangout> {
    if !state.in_room(caller, hangout_id)? {
        return Err(why_absent(state, caller, hangout_id)?);
    }
    state.db.hangout(hangout_id)?.ok_or_else(ApiError::not_found)
}

/// S62: a floor this caller walked onto and has since left. A floor they never entered stays
/// "not found", so the answer doesn't reveal a room.
fn why_absent(state: &AppState, caller: &Caller, hangout_id: &str) -> ApiResult<ApiError> {
    let who = caller.agent_id.as_deref().unwrap_or(caller.person_id.as_str());
    if let Some((at, reason)) = state.db.floor_leave(who, hangout_id)? {
        let message = format!("you left this floor at {}: {reason}", iso(at));
        return Ok(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            with_whereabouts(state, caller, hangout_id, message)?,
        ));
    }
    if let Some(until) = state.db.visit_ended(who, hangout_id, now())? {
        let message = format!("your time on this floor ended at {}", iso(until));
        return Ok(ApiError::new(
            axum::http::StatusCode::NOT_FOUND,
            with_whereabouts(state, caller, hangout_id, message)?,
        ));
    }
    Ok(ApiError::not_found())
}

/// S81: a bot that asks for a floor it left is told where it is now and what to call, if that is a
/// room it may read.
fn with_whereabouts(
    state: &AppState,
    caller: &Caller,
    asked: &str,
    message: String,
) -> ApiResult<String> {
    let Some(agent_id) = &caller.agent_id else {
        return Ok(message);
    };
    let Some(place) = state.db.bot_place(agent_id, now())?.filter(|place| place != asked) else {
        return Ok(message);
    };
    if !state.in_room(caller, &place)? {
        return Ok(message);
    }
    let Some(room) = state.db.hangout(&place)? else {
        return Ok(message);
    };
    Ok(format!("{message}. You are in {} now: read_room with hangout_id {}.", room.name, room.id))
}

/// The room, if the caller belongs to it rather than visiting. Guests can watch, talk, share notes,
/// check in and ask for a scene, and nothing else: no plans, no answers, no changing who is here.
pub fn no_guests(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    action: &str,
) -> ApiResult<Hangout> {
    let hangout = require_room(state, caller, hangout_id)?;
    if state.guest_visit(caller, hangout_id)?.is_some() {
        return Err(ApiError::forbidden(format!("guests can't {action}")));
    }
    Ok(hangout)
}

pub(crate) fn require_human(caller: &Caller, action: &str) -> ApiResult<()> {
    if caller.is_agent() {
        return Err(ApiError::forbidden(format!("only a person can {action}")));
    }
    Ok(())
}

/// The house bot reads a queue and reports back, and that is all it can do here.
pub(crate) fn no_house_bot(caller: &Caller) -> ApiResult<()> {
    if caller.is_house {
        return Err(ApiError::forbidden(
            "the house bot can only read its queue, finish requests and set the house status",
        ));
    }
    Ok(())
}

pub(crate) fn note_agent_write(state: &AppState, caller: &Caller) -> ApiResult<()> {
    if let Some(agent_id) = &caller.agent_id {
        if !state.allow_agent_write(agent_id) {
            return Err(ApiError::too_many_requests(format!(
                "slow down: bots get {MAX_AGENT_WRITES_PER_MINUTE} writes a minute"
            )));
        }
        state.db.touch_agent_active(agent_id, now())?;
    }
    Ok(())
}

// Chat

pub fn post_message(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    kind: &str,
    body: &str,
) -> ApiResult<Message> {
    post_reply(state, caller, hangout_id, kind, body, None)
}

/// I82: one answer for every reply_to that can't be used, so it says nothing about a line the
/// writer can't read.
fn cannot_answer() -> ApiError {
    ApiError::bad_request(
        "reply_to must be a chat message in this room: use the id of a line from read_room, or leave reply_to out",
    )
}

/// I82: the same, saying which chat line in this room it answers.
pub fn post_reply(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    kind: &str,
    body: &str,
    reply_to: Option<i64>,
) -> ApiResult<Message> {
    require_room(state, caller, hangout_id)?;
    items::no_quiet(state, caller, hangout_id)?;
    // A109: chat and notes are read by others, so they take the community rules accepted.
    crate::rules::require(state, caller)?;
    let body = clean(body, "message", MAX_MESSAGE_CHARS)?;
    // I82: a guest can answer only what they can read. Checked here so a refusal moves nobody,
    // and again as the reply is written, in case the line went in between.
    let seen_after = match reply_to {
        Some(_) => state.guest_visit(caller, hangout_id)?.map(|visit| visit.since),
        None => None,
    };
    if let Some(to) = reply_to {
        if !state.db.can_answer(hangout_id, to, seen_after)? {
            return Err(cannot_answer());
        }
    }
    // JB3: an owner's `/job` line gives one of their bots here a job about this room. It's checked
    // first, so a refusal posts nothing, and given once the line is posted as said.
    let job = if kind == "chat" {
        crate::jobs::job_line(state, caller, hangout_id, &body)?
    } else {
        None
    };
    if let Some((_, request)) = &job {
        crate::jobs::check_give(state, &caller.person_id, request, crate::jobs::DEFAULT_JOB_HOURS)?;
    }
    note_agent_write(state, caller)?;
    spaces::note_walk_in_write(state, caller, hangout_id)?;
    // S57: a bot has to be in a room to talk there.
    spaces::go_to(state, caller, hangout_id)?;
    let new = NewMessage {
        hangout_id,
        person_id: Some(&caller.person_id),
        agent_id: caller.agent_id.as_deref(),
        speaker: &caller.name,
        kind,
        body: &body,
        now: now(),
    };
    let message = match reply_to {
        None => state.db.insert_message(&new)?,
        Some(to) => state.db.insert_reply(&new, to, seen_after)?.ok_or_else(cannot_answer)?,
    };
    if let Some((agent_id, request)) = &job {
        crate::jobs::give(
            state,
            &caller.person_id,
            agent_id,
            request,
            Some(hangout_id),
            crate::jobs::DEFAULT_JOB_HOURS,
        )?;
    }
    if caller.agent_id.is_none() && message.kind == "chat" {
        if let Err(error) = crate::typing::clear(state, caller, hangout_id) {
            tracing::warn!(status = %error.status, "typing hint could not be cleared after chat");
        }
        state.db.note_bot_wake_owner_chat(message.id)?;
    }
    // S88 (#548): the owner's @tag brings their own bot here first, so the mention reaches it here.
    summon_own_bots(state, caller, &message)?;
    note_mentions(state, &message)?;
    // L10: a person's chat resets a bot-to-bot chain and delivers held mentions now.
    if caller.agent_id.is_none()
        && message.kind == "chat"
        && flush_held_mentions_in(state, hangout_id, message.created_at).is_err()
    {
        tracing::warn!("held mention wakes could not be flushed after chat");
    }
    // I62: a chat line in this room clears thinking here. A note doesn't.
    if kind == "chat" {
        if let Some(agent_id) = caller.agent_id.as_deref() {
            let at = message.created_at;
            state.db.put_thinking(agent_id, hangout_id, at, at)?;
        }
    }
    publish_message(state, &message);
    crate::events::note_message(state, &message);
    Ok(message)
}

/// What a mention does to a wake-up URL. `person` is a human who is not the bot's owner.
enum MentionRing {
    Send {
        once: bool,
        person: bool,
    },
    Skip(&'static str),
    /// L10: a bot-to-bot chain is waiting; this mention goes out when the hold ends.
    Hold,
}

/// What a mention rings, once it's recorded: a wake-up, a hold, or the reason it didn't.
fn deliver_ring(state: &AppState, message: &Message, bot: &Agent, ring: MentionRing) {
    match ring {
        MentionRing::Skip(reason) => {
            if state
                .db
                .note_wake_decision(
                    &bot.id,
                    Some(&message.hangout_id),
                    Some(message.id),
                    "skipped",
                    reason,
                    message.created_at,
                )
                .is_err()
            {
                // Diagnostics must not fail stored chat or prevent later mentions.
                tracing::warn!("a skipped wake decision could not be saved");
            }
        }
        MentionRing::Hold => {
            if state
                .db
                .note_wake_decision(
                    &bot.id,
                    Some(&message.hangout_id),
                    Some(message.id),
                    "held",
                    "chain",
                    message.created_at,
                )
                .is_err()
            {
                tracing::warn!("a held wake decision could not be saved");
            }
        }
        MentionRing::Send { once, person } => {
            let notice = notify::Notice {
                event: "mention",
                hangout_id: message.hangout_id.clone(),
                text: "Someone mentioned you. Call inbox. To reply in a room, call say."
                    .to_string(),
            };
            let agent_id = bot.id.clone();
            let hangout_id = message.hangout_id.clone();
            let message_id = message.id;
            notify::send_result(
                state,
                &bot.id,
                &notice,
                once,
                vec![message_id],
                move |state, posted| {
                    record_mention_delivery(
                        state,
                        &agent_id,
                        message_id,
                        &hangout_id,
                        person,
                        &posted,
                    );
                },
            );
        }
    }
}

/// S88 (#548): a person `@`-tagging one of their own bots that isn't in this room brings it here,
/// as their Bring here would (S59): every check `bring_agent` makes still holds, so a room it
/// couldn't be brought into isn't reached this way, and nothing is said if it can't come. Only a
/// person's own chat does it, never a bot's, and a bot that changed place in the last 3 minutes
/// isn't moved, so two people can't bounce it between rooms.
fn summon_own_bots(state: &AppState, caller: &Caller, message: &Message) -> ApiResult<()> {
    if caller.agent_id.is_some() || message.kind != "chat" || !message.body.contains('@') {
        return Ok(());
    }
    let now = message.created_at;
    let mine = state.db.agents_of(&caller.person_id, now)?;
    let mut names: Vec<String> = mine.iter().map(|bot| bot.name.clone()).collect();
    names.extend(state.db.agents_in(&message.hangout_id, now)?.into_iter().map(|bot| bot.name));
    for bot in mine {
        if !tags_name(&message.body, &bot.name, &names) {
            continue;
        }
        if state.db.bot_place(&bot.id, now)?.as_deref() == Some(message.hangout_id.as_str()) {
            continue;
        }
        if state.db.bot_place_since(&bot.id)?.is_some_and(|since| now - since < SUMMON_GAP_SECS) {
            continue;
        }
        if let Err(error) = bring_agent(state, caller, &message.hangout_id, &bot.id) {
            tracing::info!(status = %error.status, "an owner's tag couldn't bring their bot");
        }
    }
    Ok(())
}

/// S88: whether `body` tags `name` itself: `@` right before the whole name, in any case, not inside
/// an email address or a longer word, and not the start of a longer known bot name written there,
/// so "@Fern Two" tags Fern Two, not Fern, and "@Fernando" or "x@fern.org" tag nobody.
fn tags_name(body: &str, name: &str, known: &[String]) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let text = body.to_lowercase();
    let name = name.to_lowercase();
    let tag = format!("@{name}");
    let longer: Vec<String> = known
        .iter()
        .map(|other| other.to_lowercase())
        .filter(|other| other.len() > name.len() && other.starts_with(&name))
        .collect();
    text.match_indices(&tag).any(|(start, _)| {
        let after = &text[start + tag.len()..];
        let runs_on = after
            .starts_with(|c: char| is_word(c) || matches!(c, '@' | '-' | '/' | '\'' | '\u{2019}'))
            || (after.starts_with('.') && after[1..].starts_with(is_word));
        !text[..start].ends_with(is_word)
            && !runs_on
            && !longer.iter().any(|other| text[start + 1..].starts_with(other.as_str()))
    })
}

/// S88: a bot isn't summoned again within this long of changing place.
const SUMMON_GAP_SECS: i64 = 3 * 60;

/// B8: an @mention of a bot that's in this room, kept until that bot reads the room. E22 and E31:
/// a person's can also ring it, and the owner can see why one did not.
fn note_mentions(state: &AppState, message: &Message) -> ApiResult<()> {
    if message.kind != "chat" {
        return Ok(());
    }
    let members = state.db.agents_in(&message.hangout_id, message.created_at)?;
    let visitors = state.db.visitors_in(&message.hangout_id, message.created_at)?;
    let bots = members.iter().map(|bot| (bot, true));
    let mut seen = std::collections::HashSet::new();
    let mut named = std::collections::HashSet::new();
    for (bot, belongs) in bots.chain(visitors.iter().map(|bot| (bot, false))) {
        if message.agent_id.as_deref() == Some(bot.id.as_str()) || !seen.insert(bot.id.clone()) {
            continue;
        }
        if crate::doorbell::mentions(&message.body, &bot.name) {
            named.insert(bot.id.clone());
            // A102, A104: a block either way, or a mute, and the line doesn't reach this bot.
            if crate::blocks::cut(state, &bot.owner_id, message)? {
                continue;
            }
            // Asked before adding this one, which would otherwise be what it's waiting to read.
            let public =
                state.db.floor(&message.hangout_id)?.is_some_and(|floor| floor.access == "public");
            let trust_allows = crate::bot_trust::may_wake(state, bot, message)?;
            let home_bot_ring = trust_allows
                && !public
                && message.agent_id.is_some()
                && state.db.note_bot_wake_ring(&bot.id, message.id, message.created_at)?;
            // Keep the separately authorized L30 public-social path and its own limits intact.
            let ring = if !trust_allows {
                MentionRing::Skip("trust")
            } else if home_bot_ring {
                MentionRing::Send { once: true, person: false }
            } else {
                rings_for_mention(state, message, bot, belongs)?
            };
            state.db.add_mention(&bot.id, &message.hangout_id, message.id, message.created_at)?;
            deliver_ring(state, message, bot, ring);
        }
    }
    // BT10: a bot named here that isn't here, if its wake setting lets this speaker reach it.
    crate::bot_trust::note_absent_tags(state, message, &seen)?;
    // I88: `@room` reaches the subscribed bots present here that weren't named already. A bot's
    // `@room` queues the mention for them to read, and rings nobody.
    for bot in crate::room_mentions::recipients(state, message, &named)? {
        if crate::blocks::cut(state, &bot.owner_id, message)? {
            continue;
        }
        let ring = if message.agent_id.is_some() {
            MentionRing::Skip("room_tag")
        } else {
            rings_for_mention(state, message, &bot, true)?
        };
        state.db.add_mention(&bot.id, &message.hangout_id, message.id, message.created_at)?;
        deliver_ring(state, message, &bot, ring);
    }
    // E30: an @mention of a person in this room sends a notification to their notification URL
    for person in state.db.people_in(&message.hangout_id)? {
        if message.agent_id.is_none() && message.person_id.as_deref() == Some(person.id.as_str()) {
            continue;
        }
        if crate::doorbell::mentions(&message.body, &person.name)
            && !crate::blocks::cut(state, &person.id, message)?
        {
            // N12: kept for Needs you until they open the room.
            state.db.add_person_mention(
                &person.id,
                &message.hangout_id,
                message.id,
                message.created_at,
            )?;
            let sender = if let Some(agent_id) = &message.agent_id {
                state
                    .db
                    .agent(agent_id, message.created_at)?
                    .map(|a| a.name)
                    .unwrap_or_else(|| "A bot".to_string())
            } else if let Some(person_id) = &message.person_id {
                state.db.person(person_id)?.map(|p| p.name).unwrap_or_else(|| "Someone".to_string())
            } else {
                "Someone".to_string()
            };
            let notice = notify::Notice {
                event: "mention",
                hangout_id: message.hangout_id.clone(),
                text: format!("{sender} mentioned you in chat."),
            };
            notify::send(state, std::slice::from_ref(&person.id), &notice);
        }
    }
    Ok(())
}

fn record_mention_delivery(
    state: &AppState,
    agent_id: &str,
    message_id: i64,
    hangout_id: &str,
    person: bool,
    posted: &notify::Posted,
) {
    let at = now();
    let (decision, reason) = if posted.accepted() {
        if person {
            if let Err(error) = state.db.note_person_wake(agent_id, message_id, hangout_id, at) {
                tracing::warn!(%error, "a person wake could not be recorded");
            }
        }
        ("sent", "accepted".to_string())
    } else {
        ("failed", notify::outcome_reason(posted))
    };
    if let Err(error) = state.db.note_wake_decision(
        agent_id,
        Some(hangout_id),
        Some(message_id),
        decision,
        &reason,
        at,
    ) {
        tracing::warn!(%error, "a wake decision could not be saved");
    }
}

/// RP3: whether a ring to this bot from its owner, in this room, would be held now, as E22 holds an
/// owner's mention: an earlier ring there in the last three minutes it hasn't read yet.
pub(crate) fn owner_ring_held(
    state: &AppState,
    agent_id: &str,
    room: &str,
    at: i64,
) -> ApiResult<bool> {
    Ok(state.db.wake_ring_unread(agent_id, room, at - MENTION_RING_SECS)?)
}

/// E22, E24, E25, E31: who this mention rings, and why it doesn't. `belongs` is whether the bot
/// belongs in this room. Bot rings are recorded here so a chain hold can count them. A non-owner
/// person's ring is not: it counts only once the receiver accepts it.
fn rings_for_mention(
    state: &AppState,
    message: &Message,
    bot: &Agent,
    belongs: bool,
) -> ApiResult<MentionRing> {
    let room = &message.hangout_id;
    let at = message.created_at;
    let public = state.db.floor(room)?.is_some_and(|floor| floor.access == "public");
    // L30: another bot rings only a bot that went out onto this floor, as anyone else there would.
    // L39: or, in a project's room, a bot of the same project.
    let from_bot = message.agent_id.is_some();
    if from_bot
        && !public
        && (belongs || !state.db.walked_onto(&bot.id, at)?.contains(room))
        && !(belongs && in_same_project(state, message, at)?)
    {
        return Ok(MentionRing::Skip("bot"));
    }
    let speaker = message.person_id.as_deref().unwrap_or_default();
    let owner = !from_bot && speaker == bot.owner_id;
    if state.db.notify_url(&bot.id)?.is_none() {
        return Ok(MentionRing::Skip("no_url"));
    }
    if state.db.doorbell_paused(&bot.id)? {
        return Ok(MentionRing::Skip("paused"));
    }
    // RE20: an operator's hold. The mention is still kept, for when it lifts.
    if crate::review::is_paused(state, &bot.id) {
        return Ok(MentionRing::Skip("operator_hold"));
    }
    if !owner && state.db.wakes_only_for_owner(&bot.id)? {
        return Ok(MentionRing::Skip("only_me"));
    }
    if !crate::bot_trust::may_wake(state, bot, message)? {
        return Ok(MentionRing::Skip("trust"));
    }
    // E31: anyone who could post here. Not social hours. One try.
    if !from_bot && !owner {
        if state.db.person_wake_recent(&bot.id, room, at - MENTION_RING_SECS)? {
            return Ok(MentionRing::Skip("cooldown"));
        }
        return Ok(MentionRing::Send { once: true, person: true });
    }
    if state.db.wake_ring_unread(&bot.id, room, at - MENTION_RING_SECS)? {
        return Ok(MentionRing::Skip("unread"));
    }
    // A bot on a trip isn't out, so its hosts can wake it the way the doorbell does (L10).
    let out = !belongs && state.db.bot_visits(&bot.id, at)?.contains(room);
    let social = !(owner || (!public && !from_bot && !out && state.db.is_member(room, speaker)?));
    if social {
        if !public {
            let routine = crate::routines::routine_view(state, &bot.id, &bot.owner_id)?;
            if !routine.social_now {
                return Ok(MentionRing::Skip("social_hours"));
            }
        }
        if from_bot {
            let chain = state.db.bot_mention_chain(&bot.id, room)?;
            if chain >= i64::from(crate::doorbell::MAX_BOT_RUNS_IN_A_ROW) {
                let hold =
                    crate::doorbell::bot_chain_hold_secs(u32::try_from(chain).unwrap_or(u32::MAX));
                match state.db.hold_mention_wake(&bot.id, room, message.id, at + hold, at) {
                    Ok(()) => return Ok(MentionRing::Hold),
                    // Holding is loop protection, not a reason to drop the mention or fail chat.
                    Err(_) => tracing::warn!("a mention could not be held; sending it now"),
                }
            }
        }
    }
    state.db.note_wake_ring(&bot.id, message.id, at, from_bot)?;
    Ok(MentionRing::Send { once: false, person: false })
}

/// L10: send every held bot mention that is due, as one wake per bot and room.
pub fn flush_held_mentions(state: &AppState, now: i64) {
    let groups = match state.db.held_mentions_due(now) {
        Ok(groups) => groups,
        Err(error) => {
            tracing::error!(%error, "held mention wakes could not be listed");
            return;
        }
    };
    for (agent_id, hangout_id, message_ids) in groups {
        if let Err(error) = deliver_held_mentions(state, &agent_id, &hangout_id, &message_ids, now)
        {
            tracing::warn!(status = %error.status, "a held mention wake could not go out");
        }
    }
}

fn flush_held_mentions_in(state: &AppState, hangout_id: &str, now: i64) -> ApiResult<()> {
    for (agent_id, hangout_id, message_ids) in state.db.held_mentions_in(hangout_id)? {
        if deliver_held_mentions(state, &agent_id, &hangout_id, &message_ids, now).is_err() {
            tracing::warn!("a held mention wake could not be flushed after chat");
        }
    }
    Ok(())
}

fn deliver_held_mentions(
    state: &AppState,
    agent_id: &str,
    hangout_id: &str,
    message_ids: &[i64],
    now: i64,
) -> ApiResult<()> {
    let Some(bot) = state.db.agent(agent_id, now)? else {
        return Ok(());
    };
    let mut permitted = Vec::new();
    let mut denied = Vec::new();
    for &id in message_ids {
        let allowed = match state.db.trust_message(id)? {
            Some(message) => crate::bot_trust::may_wake(state, &bot, &message)?,
            None => false,
        };
        if allowed {
            permitted.push(id);
        } else {
            denied.push(id);
        }
    }
    if !denied.is_empty() {
        state.db.take_held_mentions(agent_id, hangout_id, &denied, None)?;
    }
    let message_ids = permitted.as_slice();
    let Some(&latest) = message_ids.last() else {
        return Ok(());
    };
    if state.db.notify_url(agent_id)?.is_none() {
        state.db.take_held_mentions(agent_id, hangout_id, message_ids, None)?;
        if state
            .db
            .note_wake_decision(agent_id, Some(hangout_id), Some(latest), "skipped", "no_url", now)
            .is_err()
        {
            tracing::warn!("a skipped held wake decision could not be saved");
        }
        return Ok(());
    }
    if state.db.doorbell_paused(agent_id)? {
        return Ok(());
    }
    if state.db.wakes_only_for_owner(agent_id)? {
        state.db.take_held_mentions(agent_id, hangout_id, message_ids, None)?;
        if state
            .db
            .note_wake_decision(agent_id, Some(hangout_id), Some(latest), "skipped", "only_me", now)
            .is_err()
        {
            tracing::warn!("a skipped held wake decision could not be saved");
        }
        return Ok(());
    }
    let Some(latest) = state.db.take_held_mentions(agent_id, hangout_id, message_ids, Some(now))?
    else {
        return Ok(());
    };
    let notice = notify::Notice {
        event: "mention",
        hangout_id: hangout_id.to_string(),
        text: "Someone mentioned you. Call inbox. To reply in a room, call say.".to_string(),
    };
    let send_id = agent_id.to_string();
    let hangout_id = hangout_id.to_string();
    notify::send_result(
        state,
        agent_id,
        &notice,
        false,
        message_ids.to_vec(),
        move |state, posted| {
            record_mention_delivery(state, &send_id, latest, &hangout_id, false, &posted);
        },
    );
    Ok(())
}

/// L39: whether this room is a project's and the bot that wrote this line is in it too.
fn in_same_project(state: &AppState, message: &Message, at: i64) -> ApiResult<bool> {
    let Some(sender) = message.agent_id.as_deref() else {
        return Ok(false);
    };
    Ok(state.db.project(&message.hangout_id)?.is_some()
        && state.db.agents_in(&message.hangout_id, at)?.iter().any(|bot| bot.id == sender))
}

/// I49: the room's one slate. What's written replaces what was there, and an empty text wipes it.
/// It pings nobody and adds no line to the chat (I53), so nothing here can start a bot.
pub fn set_whiteboard(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    text: &str,
) -> ApiResult<()> {
    // A109: other people read this, so it takes the community rules accepted.
    crate::rules::require(state, caller)?;
    no_guests(state, caller, hangout_id, "write on the whiteboard")?;
    items::no_quiet(state, caller, hangout_id)?;
    let text = clean_optional(Some(text), "whiteboard", MAX_WHITEBOARD_CHARS)?;
    note_agent_write(state, caller)?;
    // S57: it's on the wall, so a bot has to be there.
    spaces::go_to(state, caller, hangout_id)?;
    if text.is_empty() {
        state.db.wipe_whiteboard(hangout_id)?;
    } else {
        let bot = caller.agent_id.as_deref();
        state.db.write_whiteboard(hangout_id, &text, &caller.person_id, bot, now())?;
    }
    refresh(state, hangout_id);
    Ok(())
}

pub fn set_status(state: &AppState, caller: &Caller, status: Option<&str>) -> ApiResult<()> {
    no_house_bot(caller)?;
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only bots have a status"));
    };
    let status = clean_optional(status, "status", MAX_STATUS_CHARS)?;
    note_agent_write(state, caller)?;
    let status = (!status.is_empty()).then_some(status.as_str());
    state.db.set_agent_status(agent_id, status)?;
    for hangout in state.db.hangouts_for_agent(agent_id, now())? {
        refresh(state, &hangout.id);
    }
    Ok(())
}

/// Shows a bot working through another tool, like a remote-control server. The whole room sees
/// this, so it takes a fixed kind and a bare tool name, with no room for what's being done or to
/// whose machine. It doesn't count against the write limit because hooks fire on every tool call.
pub fn report_activity(
    state: &AppState,
    caller: &Caller,
    kind: &str,
    via: Option<&str>,
) -> ApiResult<()> {
    no_house_bot(caller)?;
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only bots report activity"));
    };
    // I85: `none` ends what's shown straight away, rather than waiting for it to run out.
    if kind == "none" {
        if state.db.clear_activity(agent_id)? {
            for hangout in state.db.hangouts_for_agent(agent_id, now())? {
                refresh(state, &hangout.id);
            }
        }
        return Ok(());
    }
    if !ACTIVITY_KINDS.contains(&kind) {
        return Err(ApiError::bad_request(
            "kind must be computer, coding, searching, github, reviewing, other or none: send one of those",
        ));
    }
    // I85: the label is optional, except for a computer, where it's the tool being used.
    let via = match via.map(str::trim).filter(|via| !via.is_empty()) {
        None if kind == "computer" => {
            return Err(ApiError::bad_request(
                "via is required for computer: name the tool in use, like \"Claude Code\"",
            ));
        }
        None => String::new(),
        Some(via) => clean(via, "via", MAX_ACTIVITY_VIA_CHARS)?,
    };
    if !via.chars().all(|ch| ch.is_alphanumeric() || " -_.".contains(ch)) {
        return Err(ApiError::bad_request(
            "via is only the tool's name, with letters, numbers, spaces, - _ and .: remove anything else",
        ));
    }
    let now = now();
    let current = state.db.agent(agent_id, now)?.and_then(|agent| agent.activity);
    state.db.record_activity(agent_id, kind, &via, now + ACTIVITY_SECS)?;
    state.db.touch_agent_active(agent_id, now)?;
    let unchanged = current.is_some_and(|current| current.kind == kind && current.via == via);
    // I61: stretch thinking that's already on; this never starts it.
    let thinking_rooms = state.db.extend_live_thinking(agent_id, now + THINKING_SECS, now)?;
    if !unchanged {
        for hangout in state.db.hangouts_for_agent(agent_id, now)? {
            refresh(state, &hangout.id);
        }
    } else {
        for hangout_id in thinking_rooms {
            refresh(state, &hangout_id);
        }
    }
    Ok(())
}

/// I60: mark this bot thinking in a room, or clear it. `hangout_id` if it's in more than one.
pub fn set_thinking(
    state: &AppState,
    caller: &Caller,
    hangout_id: Option<&str>,
    on: bool,
) -> ApiResult<()> {
    no_house_bot(caller)?;
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only bots can mark themselves thinking"));
    };
    let hangout = match hangout_id.filter(|id| !id.is_empty()) {
        Some(id) => require_room(state, caller, id)?,
        None => {
            let mut hangouts = state.db.hangouts_for_agent(agent_id, now())?;
            match hangouts.len() {
                1 => hangouts.remove(0),
                0 => return Err(ApiError::bad_request(NO_HANGOUTS)),
                _ => {
                    return Err(ApiError::bad_request(
                        "you're in several hangouts, so hangout_id is required (see list_hangouts)",
                    ));
                }
            }
        }
    };
    let now = now();
    if on {
        state.db.put_thinking(agent_id, &hangout.id, now, now + THINKING_SECS)?;
    } else {
        // Clearing also stops waiting on mentions the bot has already seen.
        state.db.put_thinking(agent_id, &hangout.id, now, now)?;
    }
    refresh(state, &hangout.id);
    Ok(())
}

/// I59: the bot is catching up, so unread mentions in these rooms become thinking.
/// `hangout_id` limits it to one room (`read_room`, `events_since` with a room).
pub fn note_thinking_pickup(
    state: &AppState,
    caller: &Caller,
    hangout_id: Option<&str>,
) -> ApiResult<()> {
    let Some(agent_id) = &caller.agent_id else {
        return Ok(());
    };
    if caller.is_house {
        return Ok(());
    }
    let now = now();
    let since = now - THINKING_SECS;
    let rooms = match hangout_id.filter(|id| !id.is_empty()) {
        Some(id) => vec![id.to_string()],
        None => state.db.unread_mention_rooms(agent_id, since)?,
    };
    for room in rooms {
        if start_thinking_from_mention(state, agent_id, &room, now, since)? {
            refresh(state, &room);
        }
    }
    Ok(())
}

fn start_thinking_from_mention(
    state: &AppState,
    agent_id: &str,
    hangout_id: &str,
    now: i64,
    since: i64,
) -> ApiResult<bool> {
    let Some(at) = state.db.latest_unread_mention_at(agent_id, hangout_id, since)? else {
        return Ok(false);
    };
    if let Some((started_at, until_at)) = state.db.thinking_row(agent_id, hangout_id)? {
        if until_at > now {
            return Ok(false);
        }
        // A mention the bot already thought about does not start it again (I59).
        if at <= started_at {
            return Ok(false);
        }
    }
    state.db.put_thinking(agent_id, hangout_id, now, now + THINKING_SECS)?;
    Ok(true)
}

fn attach_thinking(
    state: &AppState,
    hangout_id: &str,
    agents: &mut [Agent],
    now: i64,
) -> ApiResult<()> {
    let thoughts = state.db.thinking_in(hangout_id, now, THINKING_SECS)?;
    for agent in agents {
        agent.thinking = thoughts.get(&agent.id).cloned();
    }
    Ok(())
}

// Calendar

#[derive(Debug, Default, Deserialize)]
pub struct EventInput {
    pub title: String,
    pub starts_at: String,
    pub ends_at: Option<String>,
    pub duration_minutes: Option<i64>,
    pub location: Option<String>,
    pub notes: Option<String>,
    pub min_yes: Option<i64>,
}

pub fn propose_event(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    input: &EventInput,
) -> ApiResult<Event> {
    // A109: other people read this, so it takes the community rules accepted.
    crate::rules::require(state, caller)?;
    no_guests(state, caller, hangout_id, "propose a plan")?;
    // I17: its title goes in the room's chat.
    items::no_quiet(state, caller, hangout_id)?;
    let title = clean(&input.title, "title", MAX_TITLE_CHARS)?;
    let location = clean_optional(input.location.as_deref(), "location", MAX_LOCATION_CHARS)?;
    let notes = clean_optional(input.notes.as_deref(), "notes", MAX_NOTE_CHARS)?;
    let now = now();
    let starts_at = parse_time(&input.starts_at, "starts_at")?;
    if starts_at < now {
        return Err(ApiError::bad_request("starts_at is in the past: send an RFC 3339 time later than now, like 2026-11-07T18:00:00-06:00"));
    }
    let ends_at = match (&input.ends_at, input.duration_minutes) {
        (Some(ends_at), _) => parse_time(ends_at, "ends_at")?,
        (None, minutes) => {
            // Checked before the sum, which a number this large would overflow.
            let minutes = minutes.unwrap_or(DEFAULT_DURATION_MINUTES);
            if !(1..=MAX_DURATION_MINUTES).contains(&minutes) {
                return Err(ApiError::bad_request(format!(
                    "duration_minutes must be between 1 and {MAX_DURATION_MINUTES} (a week)"
                )));
            }
            starts_at + minutes * 60
        }
    };
    if ends_at <= starts_at {
        return Err(ApiError::bad_request("the hangout has to end after it starts: send an ends_at later than starts_at, or a duration_minutes"));
    }
    if ends_at - starts_at > MAX_DURATION_MINUTES * 60 {
        return Err(ApiError::bad_request("hangouts can last a week at most: send an earlier ends_at or a shorter duration_minutes"));
    }
    let min_yes = input.min_yes.unwrap_or(DEFAULT_MIN_YES);
    if min_yes < 1 {
        return Err(ApiError::bad_request(
            "min_yes must be at least 1: send 1 or more, or leave it out for the default",
        ));
    }
    note_agent_write(state, caller)?;

    let id = auth::new_id();
    state.db.insert_event(&NewEvent {
        id: &id,
        hangout_id,
        title: &title,
        location: &location,
        notes: &notes,
        starts_at,
        ends_at,
        min_yes,
        proposed_by: &caller.person_id,
        proposer: &caller.name,
        now,
    })?;
    // Proposing counts as a yes from the proposer's side.
    let via_agent = caller.is_agent().then_some(caller.name.as_str());
    state.db.upsert_rsvp(&id, &caller.person_id, "yes", via_agent, "", now)?;
    announce_in_house_only(
        state,
        hangout_id,
        &format!(
            "{} proposed \"{title}\". It needs {min_yes} yes to go on the calendar.",
            caller.name
        ),
    )?;
    notify_others(
        state,
        hangout_id,
        &caller.person_id,
        "plan.proposed",
        &format!("{} proposed \"{title}\". Can you make it?", caller.name),
    )?;
    settle(state, &id)?;
    refresh(state, hangout_id);
    let event = state.db.event(&id)?.ok_or_else(ApiError::not_found)?;
    crate::events::note_plan(
        state,
        &event,
        "proposed",
        caller.agent_id.as_deref(),
        Some("yes"),
        Some(&caller.person_id),
    );
    if event.status == "confirmed" {
        crate::events::note_plan(
            state,
            &event,
            "confirmed",
            caller.agent_id.as_deref(),
            None,
            Some(&caller.person_id),
        );
    }
    Ok(event)
}

pub fn rsvp(
    state: &AppState,
    caller: &Caller,
    event_id: &str,
    response: &str,
    note: Option<&str>,
) -> ApiResult<Event> {
    let event = state.db.event(event_id)?.ok_or_else(ApiError::not_found)?;
    // F59: a person invited to this plan answers it from outside the room, without a note, since
    // a note is words for a room they can't read. A bot never answers for an invitee (F15).
    let invitee = !caller.is_agent()
        && !state.db.is_member(&event.hangout_id, &caller.person_id)?
        && state.db.is_plan_invitee(&event.id, &caller.person_id)?;
    if !invitee {
        no_guests(state, caller, &event.hangout_id, "answer a plan")?;
    } else if note.is_some_and(|note| !note.trim().is_empty()) {
        return Err(ApiError::bad_request("an invitee answers without a note"));
    }
    if !matches!(response, "yes" | "maybe" | "no") {
        return Err(ApiError::bad_request(
            "response must be yes, maybe or no: send one of those three",
        ));
    }
    let note = clean_optional(note, "note", MAX_NOTE_CHARS)?;
    // I17: an answer is the record and still counts, but its note is words for the room.
    if !note.is_empty() {
        items::no_quiet(state, caller, &event.hangout_id)?;
    }
    if event.status == "cancelled" {
        return Err(ApiError::conflict("that hangout was cancelled"));
    }
    if event.ends_at < now() {
        return Err(ApiError::conflict("that hangout is over"));
    }
    // A bot answers for its own human only, and never over the human's own answer.
    if caller.is_agent() {
        let answered_by_human = state
            .db
            .rsvp(event_id, &caller.person_id)?
            .is_some_and(|existing| existing.via_agent.is_none());
        if answered_by_human {
            return Err(ApiError::conflict(
                "your human already answered this one, and a bot can't change that",
            ));
        }
    }
    note_agent_write(state, caller)?;
    let before = event.status.clone();
    let via_agent = caller.is_agent().then_some(caller.name.as_str());
    state.db.upsert_rsvp(event_id, &caller.person_id, response, via_agent, &note, now())?;
    settle(state, event_id)?;
    refresh(state, &event.hangout_id);
    let event = state.db.event(event_id)?.ok_or_else(ApiError::not_found)?;
    crate::events::note_plan(
        state,
        &event,
        "answered",
        caller.agent_id.as_deref(),
        Some(response),
        Some(&caller.person_id),
    );
    if before == "proposed" && event.status == "confirmed" {
        crate::events::note_plan(
            state,
            &event,
            "confirmed",
            caller.agent_id.as_deref(),
            None,
            Some(&caller.person_id),
        );
    } else if before == "confirmed" && event.status == "proposed" {
        crate::events::note_plan(
            state,
            &event,
            "reopened",
            caller.agent_id.as_deref(),
            None,
            Some(&caller.person_id),
        );
    }
    Ok(event)
}

/// Moves an event on or off the calendar to match its yes count.
pub(crate) fn settle(state: &AppState, event_id: &str) -> ApiResult<()> {
    let event = state.db.event(event_id)?.ok_or_else(ApiError::not_found)?;
    let yes = event.rsvps.iter().filter(|rsvp| rsvp.response == "yes").count() as i64;
    if event.status == "proposed" && yes >= event.min_yes {
        state.db.set_event_status(event_id, "confirmed", now())?;
        announce_in_house_only(
            state,
            &event.hangout_id,
            &format!("\"{}\" is on the calendar.", event.title),
        )?;
        notify_others(
            state,
            &event.hangout_id,
            "",
            "plan.confirmed",
            &format!("\"{}\" is on the calendar.", event.title),
        )?;
        publish(state, &event.hangout_id, "confirmed", "{}".to_string());
    } else if event.status == "confirmed" && yes < event.min_yes {
        state.db.set_event_status(event_id, "proposed", now())?;
        announce_in_house_only(
            state,
            &event.hangout_id,
            &format!("\"{}\" came off the calendar: not enough yeses left.", event.title),
        )?;
    }
    Ok(())
}

pub fn cancel_event(state: &AppState, caller: &Caller, event_id: &str) -> ApiResult<Event> {
    let event = state.db.event(event_id)?.ok_or_else(ApiError::not_found)?;
    no_guests(state, caller, &event.hangout_id, "cancel a plan")?;
    if event.proposed_by != caller.person_id {
        return Err(ApiError::forbidden("only the side that proposed it can cancel it"));
    }
    if event.status == "cancelled" {
        return Ok(event);
    }
    note_agent_write(state, caller)?;
    state.db.set_event_status(event_id, "cancelled", now())?;
    announce_in_house_only(
        state,
        &event.hangout_id,
        &format!("{} cancelled \"{}\".", caller.name, event.title),
    )?;
    refresh(state, &event.hangout_id);
    let event = state.db.event(event_id)?.ok_or_else(ApiError::not_found)?;
    crate::events::note_plan(
        state,
        &event,
        "cancelled",
        caller.agent_id.as_deref(),
        None,
        Some(&caller.person_id),
    );
    Ok(event)
}

// Check-ins: asking the room before acting. The server records the answer; it can't stop a bot
// from acting anyway, so the bot guide's rule is that no answer means no.

#[derive(Debug, Default, Deserialize)]
pub struct CheckInInput {
    pub about: String,
    /// A person's name in this room. Left out, anyone on another side can answer.
    pub to: Option<String>,
    pub needs_human: Option<bool>,
    pub expires_minutes: Option<i64>,
}

pub fn create_check_in(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    input: &CheckInInput,
) -> ApiResult<CheckIn> {
    require_room(state, caller, hangout_id)?;
    spaces::no_walk_ins(state, caller, hangout_id, "ask a check-in")?;
    // I17: what it's about goes in the room's chat.
    items::no_quiet(state, caller, hangout_id)?;
    let about = clean(&input.about, "about", MAX_ABOUT_CHARS)?;
    let minutes = input.expires_minutes.unwrap_or(DEFAULT_CHECK_IN_MINUTES);
    if !(1..=MAX_CHECK_IN_MINUTES).contains(&minutes) {
        return Err(ApiError::bad_request("expires_minutes must be between 1 and 10080: send a whole number of minutes in that range"));
    }
    let people = state.db.people_in(hangout_id)?;
    let target = match input.to.as_deref().map(str::trim).filter(|to| !to.is_empty()) {
        Some(to) => Some(
            people
                .iter()
                .find(|person| person.name.eq_ignore_ascii_case(to))
                .filter(|person| person.id != caller.person_id)
                .ok_or_else(|| {
                    let names: Vec<&str> = people
                        .iter()
                        .filter(|person| person.id != caller.person_id)
                        .map(|person| person.name.as_str())
                        .collect();
                    ApiError::bad_request(format!(
                        "to must be someone else in this room: {}",
                        names.join(", ")
                    ))
                })?,
        ),
        None => {
            // N8: your own side can't approve it, so someone from another side has to be here.
            if !people.iter().any(|person| person.id != caller.person_id) {
                return Err(ApiError::conflict(
                    "only your own side is in this room, so nobody here can answer a check-in: \
                     use ask_owner to ask your owner",
                ));
            }
            None
        }
    };
    note_agent_write(state, caller)?;
    // S57: asking the room is being in it.
    spaces::go_to(state, caller, hangout_id)?;

    let id = auth::new_id();
    let now = now();
    state.db.insert_check_in(&NewCheckIn {
        id: &id,
        hangout_id,
        asked_by: &caller.person_id,
        asker: &caller.name,
        agent_id: caller.agent_id.as_deref(),
        about: &about,
        to_person: target.map(|person| person.id.as_str()),
        needs_human: input.needs_human.unwrap_or(false),
        expires_at: now + minutes * 60,
        now,
    })?;
    // A103: one asked of someone across a block is kept for whoever asked, and goes nowhere.
    let across = match target {
        Some(person) => crate::blocks::apart(state, &caller.person_id, &person.id)?,
        None => false,
    };
    if across {
        state.db.quiet_ask("check_in", &id, now)?;
    }
    let audience = target.map_or("the room", |person| person.name.as_str());
    let line = format!("{} is checking in with {audience}: {about}", caller.name);
    match target {
        // A103: the room line still goes up, so the asker sees what any check-in shows. Tied to
        // them, it's hidden from the person who blocked them (A101). An asker who did the
        // blocking knows already, and the other side mustn't see an ask they can't answer.
        Some(person) if across => {
            if state.db.has_blocked(&person.id, &caller.person_id)?
                && !has_guests(state, hangout_id)?
            {
                announced_about(state, hangout_id, Some(&caller.person_id), &line)?;
            }
        }
        _ => announce_in_house_only(state, hangout_id, &line)?,
    }
    let text = format!("{} is asking before acting: {about}", caller.name);
    match target {
        Some(_) if across => {}
        Some(person) => notify_side(state, hangout_id, &person.id, "check_in.waiting", &text)?,
        None => notify_others(state, hangout_id, &caller.person_id, "check_in.waiting", &text)?,
    }
    refresh(state, hangout_id);
    let check_in = state.db.check_in(&id, now)?.ok_or_else(ApiError::not_found)?;
    if !across {
        crate::events::note_check_in(state, &check_in, caller.agent_id.as_deref());
    }
    Ok(check_in)
}

pub fn answer_check_in(
    state: &AppState,
    caller: &Caller,
    check_in_id: &str,
    response: &str,
    note: Option<&str>,
) -> ApiResult<CheckIn> {
    let now = now();
    let check_in = state.db.check_in(check_in_id, now)?.ok_or_else(ApiError::not_found)?;
    // A103: one asked across a block never reached anyone to answer it.
    if crate::blocks::quiet(state, "check_in", &check_in.id)? {
        return Err(ApiError::not_found());
    }
    no_guests(state, caller, &check_in.hangout_id, "answer a check-in")?;
    if !matches!(response, "go" | "hold") {
        return Err(ApiError::bad_request("response must be go or hold: send one of those two"));
    }
    let note = clean_optional(note, "note", MAX_NOTE_CHARS)?;
    // I17: as for a plan, the answer counts and its note is words for the room.
    if !note.is_empty() {
        items::no_quiet(state, caller, &check_in.hangout_id)?;
    }
    if check_in.asked_by == caller.person_id {
        return Err(ApiError::forbidden("a side can't approve its own check-in"));
    }
    if check_in.to_person.as_ref().is_some_and(|to| *to != caller.person_id) {
        return Err(ApiError::forbidden("this check-in is for someone else"));
    }
    if check_in.needs_human && caller.is_agent() {
        return Err(ApiError::forbidden("this check-in needs a person to answer it"));
    }
    match check_in.status.as_str() {
        "expired" => return Err(ApiError::conflict("that check-in expired")),
        "waiting" => {}
        // A person can overrule a bot, but nothing overrules a person.
        _ if !caller.is_agent() && check_in.answered_by_bot => {}
        _ => return Err(ApiError::conflict("that check-in was already answered")),
    }
    note_agent_write(state, caller)?;
    state.db.answer_check_in(check_in_id, response, &caller.name, caller.is_agent(), &note, now)?;
    let word = if response == "go" { "go" } else { "hold" };
    let text = format!("{} said {word} to {}: {}", caller.name, check_in.asker, check_in.about);
    announce_in_house_only(state, &check_in.hangout_id, &text)?;
    notify_side(state, &check_in.hangout_id, &check_in.asked_by, "check_in.answered", &text)?;
    refresh(state, &check_in.hangout_id);
    let check_in = state.db.check_in(check_in_id, now)?.ok_or_else(ApiError::not_found)?;
    crate::events::note_check_in(state, &check_in, caller.agent_id.as_deref());
    Ok(check_in)
}

// Questions for a bot's owner. These are between a bot and the person who owns it, so nothing about
// one goes in the room. An answer is only words: a trip, a scene and a plan each have their own yes,
// and nothing here can give one.

/// Tells an open page about a question. Guests aren't even told that something changed, so where
/// they can be the owner's page finds it on its next reload.
fn refresh_for_owner(state: &AppState, hangout_id: &str) -> ApiResult<()> {
    if !has_guests(state, hangout_id)? {
        refresh(state, hangout_id);
    }
    Ok(())
}

/// A bot asks its owner something in private. The question stays with a room the bot belongs to, so
/// its owner can still open that room after a trip ends. A guest asks from one of its own rooms.
pub fn ask_owner(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    question: &str,
) -> ApiResult<OwnerQuestion> {
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only bots ask their owner"));
    };
    // R46: name where the guest is, so a bot on a public floor isn't sent looking for a Porch.
    let on_porch =
        state.db.house_for_room(hangout_id)?.is_some_and(|house| house.porch_id == hangout_id);
    no_guests(
        state,
        caller,
        hangout_id,
        if on_porch {
            "ask their owner from a Porch: use one of your own rooms"
        } else {
            "ask their owner from a public floor they walked onto: pass one of your own rooms as hangout_id"
        },
    )?;
    let question = clean(question, "question", MAX_ABOUT_CHARS)?;
    note_agent_write(state, caller)?;

    let id = auth::new_id();
    let now = now();
    let added = state.db.insert_owner_question(
        &NewOwnerQuestion {
            id: &id,
            hangout_id,
            agent_id,
            owner_id: &caller.person_id,
            asker: &caller.name,
            question: &question,
            now,
        },
        now - QUESTION_LAPSE_SECS,
        MAX_OPEN_QUESTIONS,
    )?;
    if !added {
        return Err(ApiError::conflict(format!(
            "you have {MAX_OPEN_QUESTIONS} questions waiting for your owner: wait for an answer"
        )));
    }
    // The owner alone: `notify_side` would also reach their bots in this room.
    let notice = notify::Notice {
        event: "owner_question.asked",
        hangout_id: hangout_id.to_string(),
        text: format!("{} asks you: {question}", caller.name),
    };
    notify::send(state, std::slice::from_ref(&caller.person_id), &notice);
    refresh_for_owner(state, hangout_id)?;
    state.db.owner_question(&id)?.ok_or_else(ApiError::not_found)
}

pub fn answer_owner_question(
    state: &AppState,
    caller: &Caller,
    question_id: &str,
    answer: &str,
) -> ApiResult<OwnerQuestion> {
    // Before any lookup, so a bot can't use this to find out which questions exist.
    require_human(caller, "answer a bot's question")?;
    let question = state
        .db
        .owner_question(question_id)?
        .filter(|question| question.owner_id == caller.person_id)
        .ok_or_else(ApiError::not_found)?;
    let answer = clean(answer, "answer", MAX_ABOUT_CHARS)?;
    let now = now();
    if question.answer.is_none() && question.created_at <= now - QUESTION_LAPSE_SECS {
        return Err(ApiError::conflict("that question lapsed"));
    }
    if !state.db.answer_owner_question(question_id, &answer, now)? {
        // R52: say which, so nobody wonders where their answer went.
        return Err(ApiError::conflict(
            match state.db.owner_question_closing(question_id)?.as_deref() {
                Some("withdrawn") => "the bot withdrew that question",
                Some(_) => "you dismissed that question",
                None => "that question was already answered",
            },
        ));
    }
    // Only a doorbell: a webhook's URL can leak, so the answer waits in the bot's own inbox.
    let notice = notify::Notice {
        event: "owner_question.answered",
        hangout_id: question.hangout_id.clone(),
        text: "Your owner answered a question. Call inbox.".to_string(),
    };
    notify::send(state, std::slice::from_ref(&question.agent_id), &notice);
    refresh_for_owner(state, &question.hangout_id)?;
    let answered = state.db.owner_question(question_id)?.ok_or_else(ApiError::not_found)?;
    crate::events::note_owner_answer(state, &answered);
    Ok(answered)
}

/// R51: a bot takes back a question it got its answer to some other way. It's not an answer.
pub fn withdraw_question(state: &AppState, caller: &Caller, question_id: &str) -> ApiResult<()> {
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only the bot that asked can withdraw a question"));
    };
    let question = state
        .db
        .owner_question(question_id)?
        .filter(|question| question.agent_id == *agent_id)
        .ok_or_else(ApiError::not_found)?;
    close_question(state, &question, "withdrawn")
}

/// R52: an owner sets a question aside, and its bot is told in `inbox`.
pub fn dismiss_owner_question(
    state: &AppState,
    caller: &Caller,
    question_id: &str,
) -> ApiResult<()> {
    require_human(caller, "dismiss a bot's question")?;
    let question = state
        .db
        .owner_question(question_id)?
        .filter(|question| question.owner_id == caller.person_id)
        .ok_or_else(ApiError::not_found)?;
    close_question(state, &question, "dismissed")?;
    let notice = notify::Notice {
        event: "owner_question.dismissed",
        hangout_id: question.hangout_id.clone(),
        text: "Your owner dismissed a question. Call inbox.".to_string(),
    };
    notify::send(state, std::slice::from_ref(&question.agent_id), &notice);
    Ok(())
}

fn close_question(state: &AppState, question: &OwnerQuestion, how: &str) -> ApiResult<()> {
    let now = now();
    if question.answer.is_none() && question.created_at <= now - QUESTION_LAPSE_SECS {
        return Err(ApiError::conflict("that question lapsed"));
    }
    if !state.db.close_owner_question(&question.id, how, now)? {
        return Err(ApiError::conflict("that question was already answered or closed"));
    }
    refresh_for_owner(state, &question.hangout_id)
}

/// Everything waiting on this caller's side, across its rooms. It's the one call a bot needs when
/// it wakes up, whether a notification or its own schedule woke it.
#[derive(Debug, Default, Serialize)]
pub struct Inbox {
    pub project_changes: Vec<serde_json::Value>,
    pub waiting_on_you: Vec<CheckIn>,
    pub your_check_ins: Vec<CheckIn>,
    pub plans_to_answer: Vec<Event>,
    /// Trips this person could still say yes to, as the bot's owner or as a resident. People only.
    pub trips_to_answer: Vec<Trip>,
    /// Scene requests waiting on a resident. People only.
    pub scenes_to_answer: Vec<SceneRequest>,
    /// Trips of this bot, or of this person's bots, that are open, under way or recent.
    pub your_trips: Vec<Trip>,
    /// Standing passes offered to this person's bots that nobody has accepted yet. People only.
    pub passes_to_accept: Vec<Pass>,
    /// This bot's questions its owner answered in the last day. Only the owner can write these, so
    /// unlike everything else here they aren't the room's words.
    pub owner_answers: Vec<OwnerQuestion>,
    /// R52: questions its owner dismissed in the last day, so it stops waiting on them.
    pub dismissed_questions: Vec<OwnerQuestion>,
    /// This bot's questions still open. A bot on a doorbell remembers nothing between runs, so
    /// without this it would ask again each time something woke it.
    pub your_questions: Vec<OwnerQuestion>,
    /// Plans this side is going to that start soon (F44).
    pub plans_soon: Vec<crate::closeness::PlanSoon>,
    /// S60: the room this bot is in, by id and name. People only ever have nothing here.
    pub you_are_in: Option<serde_json::Value>,
    /// B8: @mentions of this bot it hasn't read yet. Empty for a person.
    pub mentions: Vec<Mention>,
    /// S62, L21: the public floor this bot left most recently, while it isn't out. Null otherwise.
    pub last_left: Option<FloorLeave>,
    /// L28: the other bots out on the floor this bot went out to, and what it can do with them.
    pub meet: Option<crate::routines::Meet>,
    /// L38: working at its project's building, or alone at the town square, once a half hour.
    pub nudge: Option<crate::routines::Call>,
    /// RP5: its repeating plans' starts in the last day, cued, missed, paused or over the cap.
    pub plan_starts: Vec<serde_json::Value>,
    /// MU7: meetups other bots proposed to this one, and its own.
    pub meetups: Vec<crate::meetups::MeetupView>,
}

pub fn inbox(state: &AppState, caller: &Caller) -> ApiResult<Inbox> {
    no_house_bot(caller)?;
    let now = now();
    let mut hangouts = match &caller.agent_id {
        Some(agent_id) => state.db.hangouts_for_agent(agent_id, now)?,
        None => state.db.hangouts_for_person(&caller.person_id)?,
    };
    if !caller.is_agent() {
        // A person watching their bot on a Porch has no membership there, but their bot's own
        // check-ins are answered there.
        for trip in state.db.trips_for_owner(&caller.person_id, now, now)? {
            let porch = state.db.house(&trip.house_id)?.map(|house| house.porch_id);
            if let Some(porch) = porch.filter(|_| trip.status == "here") {
                if !hangouts.iter().any(|hangout| hangout.id == porch) {
                    hangouts.extend(state.db.hangout(&porch)?);
                }
            }
        }
    }
    let mut inbox = Inbox::default();
    for hangout in hangouts {
        let guest = state.guest_visit(caller, &hangout.id)?.is_some();
        for check_in in state.db.recent_check_ins(&hangout.id, now - DAY_SECS, now)? {
            if check_in.asked_by == caller.person_id {
                inbox.your_check_ins.push(check_in);
            } else if !guest
                && check_in.status == "waiting"
                && check_in.to_person.as_ref().is_none_or(|to| *to == caller.person_id)
                && !(check_in.needs_human && caller.is_agent())
                && !crate::blocks::quiet(state, "check_in", &check_in.id)?
            {
                inbox.waiting_on_you.push(check_in);
            }
        }
        // A guest has no plans to answer: those belong to the house.
        if guest {
            continue;
        }
        for event in state.db.upcoming_events(&hangout.id, now)? {
            inbox.plans_soon.extend(crate::closeness::plan_soon(state, caller, &event, now)?);
            if !event.rsvps.iter().any(|rsvp| rsvp.person_id == caller.person_id) {
                inbox.plans_to_answer.push(event);
            }
        }
    }
    inbox.trips_to_answer = city::trips_to_answer(state, caller, now)?;
    inbox.scenes_to_answer = city::scenes_to_answer(state, caller, now)?;
    inbox.your_trips = city::your_trips(state, caller, now)?;
    inbox.passes_to_accept = city::passes_to_accept(state, caller)?;
    if let Some(agent_id) = &caller.agent_id {
        inbox.owner_answers = state.db.owner_answers(agent_id, now - DAY_SECS)?;
        inbox.dismissed_questions = state.db.dismissed_questions(agent_id, now - DAY_SECS)?;
        inbox.your_questions =
            state.db.open_questions_of_bot(agent_id, now - QUESTION_LAPSE_SECS)?;
        let place = state.db.bot_place(agent_id, now)?;
        let room = place.map(|id| state.db.hangout(&id)).transpose()?.flatten();
        inbox.you_are_in = room.map(|room| serde_json::json!({ "id": room.id, "name": room.name }));
        // B8: tags in rooms it belongs to or is visiting, until it reads that room.
        inbox.mentions = state.db.unread_mentions(agent_id)?;
        // L21: why it isn't on the floor it went out to, while it isn't out.
        if state.db.bot_visits(agent_id, now)?.is_empty() {
            inbox.last_left = state.db.latest_floor_leave(agent_id, now - DAY_SECS)?;
        }
        inbox.meetups = crate::meetups::for_inbox(state, agent_id, now)?;
        inbox.meet = crate::routines::meet_of(state, agent_id, &caller.person_id, now)?;
        inbox.nudge = crate::routines::call_of(state, agent_id, &caller.person_id, now)?
            .filter(|call| call.event != "meet");
        inbox.plan_starts = crate::cadence::recent_for_bot(state, agent_id, now)?;
    }
    inbox.project_changes = crate::projects::changes_of(state, caller)?;
    Ok(inbox)
}

pub fn set_person_notify(state: &AppState, caller: &Caller, url: Option<&str>) -> ApiResult<()> {
    require_human(caller, "change notifications")?;
    let url = url.map(str::trim).filter(|url| !url.is_empty());
    let url = url.map(|url| notify::clean_url(state, url)).transpose()?;
    state.db.set_notify_url(&caller.person_id, url.as_deref())?;
    Ok(())
}

/// A bot's doorbell calls this on every check, and holds off while the answer is true. Rooms only
/// hear about it when the bot starts listening: they notice it stopping on their next reload.
pub fn doorbell_check(state: &AppState, caller: &Caller) -> ApiResult<bool> {
    no_house_bot(caller)?;
    let Some(agent_id) = &caller.agent_id else {
        return Err(ApiError::forbidden("only bots have a doorbell"));
    };
    let now = now();
    let listening = state
        .db
        .agent(agent_id, now)?
        .and_then(|agent| agent.doorbell)
        .is_some_and(|doorbell| doorbell.listening);
    let paused = state.db.note_doorbell(agent_id, now)?;
    if !listening {
        for hangout in state.db.hangouts_for_agent(agent_id, now)? {
            refresh(state, &hangout.id);
        }
    }
    // L9: a bot that's listening and may be out, and isn't, wanders somewhere. A paused one stays put.
    if !paused {
        crate::routines::wander(state, agent_id, &caller.person_id)?;
    }
    Ok(paused)
}

/// I107: only the bot's owner turns its waiting pulse on or off.
pub fn set_waiting_pulse(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
    on: bool,
) -> ApiResult<()> {
    own_agent(state, caller, agent_id, "change a bot's waiting pulse")?;
    state.db.set_waiting_pulse(agent_id, on)?;
    Ok(())
}

/// I113: only the bot's owner turns its waiting push on or off.
pub fn set_waiting_push(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
    on: bool,
) -> ApiResult<()> {
    own_agent(state, caller, agent_id, "change a bot's waiting push")?;
    state.db.set_waiting_push(agent_id, on)?;
    Ok(())
}

pub fn set_doorbell_paused(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
    paused: bool,
) -> ApiResult<()> {
    own_agent(state, caller, agent_id, "pause a bot's doorbell")?;
    state.db.set_doorbell_paused(agent_id, paused)?;
    if paused {
        leave_public_rooms(state, agent_id)?;
    }
    for hangout in state.db.hangouts_for_agent(agent_id, now())? {
        refresh(state, &hangout.id);
    }
    Ok(())
}

/// What to do with a bot's saved wake-up key. Leaving it out keeps the key already saved.
pub enum NotifyAuth<'a> {
    Keep,
    Clear,
    Set(&'a str),
}

pub fn set_agent_notify(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
    url: NotifyAuth<'_>,
    auth: NotifyAuth<'_>,
    only_me: Option<bool>,
    trusted_bots: Option<&[String]>,
) -> ApiResult<()> {
    let target = own_agent(state, caller, agent_id, "change a bot's wake-up URL")?;
    // Everything is checked before anything is written, so a save that's refused changes nothing.
    if let NotifyAuth::Set(secret) = &auth {
        notify::auth_header(secret)?;
    }
    if let Some(bots) = trusted_bots {
        if bots.len() > 20 {
            return Err(ApiError::bad_request("choose at most 20 bots allowed to wake this bot"));
        }
        let mut seen = std::collections::HashSet::new();
        for sender_id in bots {
            let sender = state.db.agent(sender_id, now())?;
            let is_house = state.db.bot_home(sender_id)?.is_some_and(|home| home.is_house);
            if sender_id == agent_id
                || !seen.insert(sender_id)
                || sender.is_none_or(|sender| sender.owner_id != target.owner_id)
                || is_house
            {
                return Err(ApiError::bad_request(
                    "choose distinct ordinary bots of your own, other than this bot",
                ));
            }
        }
        if !bots.is_empty() && state.db.bot_home(agent_id)?.is_some_and(|home| home.is_house) {
            return Err(ApiError::bad_request("a house bot cannot receive bot mention wakes"));
        }
    }
    // Leaving the URL out keeps the one already saved. An empty one removes the ping and the key.
    let url = match url {
        NotifyAuth::Keep => state.db.notify_url(agent_id)?,
        NotifyAuth::Clear => None,
        NotifyAuth::Set(text) => {
            let text = text.trim();
            if text.is_empty() {
                None
            } else {
                Some(notify::clean_url(state, text)?)
            }
        }
    };
    let auth = if url.is_none() {
        None
    } else {
        match auth {
            NotifyAuth::Keep => state.db.notify_auth(agent_id)?,
            NotifyAuth::Clear => None,
            NotifyAuth::Set(secret) => Some(secret.trim().to_string()),
        }
    };
    state.db.save_agent_notify(agent_id, url.as_deref(), auth.as_deref(), only_me, trusted_bots)?;
    if state.db.wakes_only_for_owner(agent_id)? {
        leave_public_rooms(state, agent_id)?;
    }
    Ok(())
}

/// E29: the owner's off switch takes their bot out of public conversations immediately.
pub fn leave_public_rooms(state: &AppState, agent_id: &str) -> ApiResult<()> {
    for room in state.db.leave_public_rooms(agent_id, now())? {
        state.presence.forget(&room, agent_id);
        crate::events::note_place(state, agent_id, &room, "sent_home");
        refresh(state, &room);
    }
    spaces::refresh_bot_place(state, agent_id)?;
    state.worlds.mark_stale(crate::worlds::COMMONS_ID);
    Ok(())
}

/// E23: one try with what's saved, so an owner can see a refused key. The result is kept (E31).
pub async fn test_agent_notify(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
) -> ApiResult<serde_json::Value> {
    own_agent(state, caller, agent_id, "test a bot's wake-up URL")?;
    let url = state.db.notify_url(agent_id)?;
    let url = url.ok_or_else(|| {
        ApiError::bad_request("save a wake-up URL for this bot first, then test it")
    })?;
    let auth = state.db.notify_auth(agent_id)?;
    let (result, posted) = notify::test_wake_detail(state, &url, auth.as_deref()).await;
    let accepted = posted.accepted();
    let reason = format!("test_{}", notify::outcome_reason(&posted));
    if state
        .db
        .note_wake_decision(
            agent_id,
            None,
            None,
            if accepted { "sent" } else { "failed" },
            &reason,
            now(),
        )
        .is_err()
    {
        // The callback already happened; return its result, not a diagnostic storage error.
        tracing::warn!("a test wake decision could not be saved");
    }
    Ok(result)
}

/// The URL, and whether a key is saved. The key itself stays on the server.
pub fn agent_notify(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
) -> ApiResult<serde_json::Value> {
    own_agent(state, caller, agent_id, "see a bot's wake-up URL")?;
    let url = state.db.notify_url(agent_id)?;
    let auth = state.db.notify_auth(agent_id)?.is_some();
    let only_me = state.db.wakes_only_for_owner(agent_id)?;
    let trusted_bots = state.db.trusted_bots(agent_id)?;
    let mut available_bots = Vec::new();
    if !state.db.bot_home(agent_id)?.is_some_and(|home| home.is_house) {
        for bot in state.db.agents_of(&caller.person_id, now())? {
            if bot.id != agent_id && !state.db.bot_home(&bot.id)?.is_some_and(|home| home.is_house)
            {
                available_bots.push(serde_json::json!({ "id": bot.id, "name": bot.name }));
            }
        }
    }
    let decisions = state
        .db
        .wake_decisions(agent_id)?
        .into_iter()
        .map(|row| {
            serde_json::json!({
                "decision": row.decision,
                "reason": row.reason,
                "hangout_id": row.hangout_id,
                "room": row.room,
                "at": iso(row.at),
                "says": decision_says(&row.decision, &row.reason, row.room.as_deref()),
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "url": url, "auth": auth, "only_me": only_me,
        "trusted_bots": trusted_bots, "available_bots": available_bots, "decisions": decisions,
        "waiting_pulse": state.db.waiting_pulse_enabled(agent_id)?,
        "waiting_push": state.db.waiting_push_enabled(agent_id)? }))
}

/// E31: what the owner reads. The message itself is never in here.
fn decision_says(decision: &str, reason: &str, room: Option<&str>) -> String {
    let room = room.filter(|name| !name.is_empty()).unwrap_or("a room");
    if let Some(detail) = reason.strip_prefix("test_") {
        return match detail {
            "accepted" => "The test was accepted.".to_string(),
            "timeout" => "The test didn't answer in time.".to_string(),
            "tls_error" => "The test stopped: its certificate failed.".to_string(),
            _ if detail.starts_with("http_") => {
                format!("The test was refused ({}).", detail.trim_start_matches("http_"))
            }
            _ => "The test didn't arrive.".to_string(),
        };
    }
    match (decision, reason) {
        ("sent", _) => format!("Sent a wake for a mention in {room}."),
        ("failed", "timeout") => {
            format!("A wake for {room} didn't arrive: it didn't answer in time.")
        }
        ("failed", "tls_error") => {
            format!("A wake for {room} didn't arrive: its certificate failed.")
        }
        ("failed", "connection_refused") | ("failed", "no_url") => {
            format!("A wake for {room} didn't arrive: nothing answered.")
        }
        ("failed", reason) if reason.starts_with("http_") => {
            format!("A wake for {room} was refused ({}).", reason.trim_start_matches("http_"))
        }
        ("skipped", "paused") => "Skipped a mention: the bot is paused.".to_string(),
        ("skipped", "only_me") => "Skipped a mention: only you can wake it.".to_string(),
        ("skipped", "no_url") => "Skipped a mention: there is no wake-up URL.".to_string(),
        ("skipped", "unread") => {
            format!("Skipped a mention in {room}: an earlier wake there is still unread.")
        }
        ("skipped", "cooldown") => {
            format!("Skipped a mention in {room}: a wake there was accepted in the last 3 minutes.")
        }
        ("skipped", "cap") => {
            format!("Skipped a mention in {room}: today's social wake-up cap is used up.")
        }
        ("held", _) => {
            format!(
                "Holding a mention in {room}: other bots have woken it several times in a row, so this one waits 3 minutes, then longer, up to an hour, and then goes out with every mention they sent."
            )
        }
        ("skipped", "social_hours") => {
            format!("Skipped a mention in {room}: it is outside social hours.")
        }
        ("skipped", "bot") => {
            format!("Skipped a mention in {room}: a bot needs permission to wake this one.")
        }
        _ => format!("Wake {decision} ({reason}) in {room}."),
    }
}

pub fn calendar_feed(state: &AppState, feed_token: &str) -> ApiResult<String> {
    let person = state.db.person_by_feed(feed_token)?.ok_or_else(ApiError::not_found)?;
    let now = now();
    let events: Vec<FeedEvent> = state.db.feed_events(&person.id, now - FEED_HISTORY_SECS)?;
    Ok(ical::feed(&person.name, &events, now))
}

// Room snapshot

#[derive(Debug, Serialize)]
pub struct Stats {
    pub bots: usize,
    pub bots_active: usize,
    pub messages_24h: i64,
    pub on_calendar: usize,
}

#[derive(Debug, Serialize)]
pub struct You {
    pub person_id: String,
    pub agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub hangout: Hangout,
    pub you: You,
    pub people: Vec<Person>,
    pub agents: Vec<Agent>,
    pub messages: Vec<Message>,
    /// A101: how many lines were left out because the caller blocked who wrote them.
    #[serde(skip_serializing_if = "is_zero")]
    pub hidden: usize,
    pub notes: Vec<Message>,
    /// What's on the wall (I48), or null if it's blank or a guest arrived after it was written.
    pub whiteboard: Option<Whiteboard>,
    /// Boards on this floor, without the cards' text (K10). Empty when the room isn't a floor.
    pub boards: Vec<crate::db::BoardSummary>,
    /// Pins on the wall whiteboard (I68). Empty when there are none, or a guest arrived after them.
    pub pins: Vec<serde_json::Value>,
    /// anyone, owners or off. A private floor ignores it (I69).
    pub pin_policy: String,
    pub can_pin: bool,
    pub can_clear_pins: bool,
    /// Open votes and ones closed in the last day (V6, I111). Empty for a guest, who sees no past votes.
    pub votes: Vec<serde_json::Value>,
    pub events: Vec<Event>,
    /// Friends who answered a plan through its link, who aren't in `people` (F19).
    pub invitees: Vec<crate::db::Invitee>,
    /// The lists of this room's plans that the caller is going to (F36).
    pub plan_items: Vec<crate::db::PlanItem>,
    pub check_ins: Vec<CheckIn>,
    /// What this person's bots have asked them here. Always empty for bots and guests.
    pub owner_questions: Vec<OwnerQuestion>,
    pub stats: Stats,
    /// Set when the caller is only visiting this Porch: they see nothing from before they came.
    pub guest: Option<Visit>,
    /// Which city and house this room belongs to, so the page can link back.
    pub city_id: Option<String>,
    pub house_id: Option<String>,
    /// Which floor of which space this is. Null for a Town Square, Living Room or Porch.
    pub floor: Option<spaces::Place>,
    /// The floor's items, and the mood and beauty they add up to (I8, I24).
    #[serde(flatten)]
    pub furnishing: items::Furnishing,
    /// Which of this person's bots have moods turned off, for their own page (I10). Always empty
    /// for a bot, and nobody else's bots are ever in it.
    pub bots_without_moods: Vec<String>,
    /// I120: whether `@room` from this caller would reach anyone, so the chat box can offer it.
    pub room_mention: bool,
    /// I120: the `@room` setting, only for the room's owner, so My bots can show it to change.
    pub room_mention_policy: Option<String>,
    /// I120: which of the caller's own bots hear `@room` here.
    pub room_mention_bots: Vec<String>,
    /// The ids of the people with this floor open. Empty anywhere but a floor, and for a bot.
    pub present: Vec<String>,
    /// I108: transient human composer hints, never returned to bots.
    pub typing: Vec<crate::typing::Indicator>,
    /// I146: the emotes showing over avatars here now, for people and bots alike.
    pub emotes: Vec<crate::emotes::Emote>,
    /// What's chosen for those drawn here (C11). Empty for a bot.
    pub looks: looks::Looks,
    /// MY10: the mayor, when they're in this room: a person among its people, or one of its bots.
    /// The page draws their sash. Nobody can wear it otherwise.
    pub mayor: Option<crate::elections::Sash>,
    /// A25: whether this caller can link a bot to Discord from here.
    pub discord_linking: bool,
    /// S84: whether people who walk in here see earlier chat.
    pub visitor_history: bool,
    /// S88: the caller's own bots that aren't in this room, which an @tag would bring here, for the
    /// chat box's picker: id and name only, never where they are. Empty for a bot or a guest.
    pub summonable: Vec<serde_json::Value>,
    /// KA7: whether this floor welcomes keyless agents, the ones here now, and the lines they
    /// wrote (KA10).
    pub keyless_welcome: bool,
    pub keyless_here: Vec<String>,
    pub keyless_message_ids: Vec<i64>,
}

pub fn snapshot(state: &AppState, caller: &Caller, hangout_id: &str) -> ApiResult<Snapshot> {
    let mut hangout = require_room(state, caller, hangout_id)?;
    // N12: a person who opens the room has seen what mentioned them there.
    if !caller.is_agent() {
        state.db.clear_person_mentions_in(&caller.person_id, hangout_id)?;
    }
    let now = now();
    let guest = state.guest_visit(caller, hangout_id)?;
    // Who gets into a room is a human decision, so bots never see the invite. A house's rooms
    // and a city's square ignore invites altogether. A guest never sees it either: on a public
    // floor it would let someone who walked in join and read everything from before (S21).
    if caller.is_agent() || guest.is_some() || state.db.is_managed_room(hangout_id)? {
        hangout.invite_code.clear();
    }
    // Guests are shown beside the room's own bots and people so the room can name and colour them.
    // They are never in `agents_in` or `people_in`, which decide who gets notified.
    let mut agents = state.db.agents_in(hangout_id, now)?;
    let shown_links = !caller.is_agent() && guest.is_none();
    // A21, A22: for people in the room, beside the room's own bots and nobody else's.
    if shown_links {
        for agent in &mut agents {
            agent.discord = state.db.discord_link(&agent.id)?;
            // K24: who an apprentice was made by.
            agent.apprenticed_by = crate::bonds::apprenticed_by(state, &agent.id)?;
        }
    }
    let mut guests = state.db.visitors_in(hangout_id, now)?;
    // L67: a bot's owner sees why its visit ends when it does. Nobody else is told.
    if !caller.is_agent() {
        for visitor in guests.iter_mut().filter(|visitor| visitor.owner_id == caller.person_id) {
            visitor.visitor_until_why = Some(crate::routines::visit_why(
                state,
                &visitor.id,
                &visitor.owner_id,
                hangout_id,
                now,
            )?);
        }
    }
    agents.extend(guests);
    // S55: they all belong here, or are guests, but each is in only one place.
    for agent in &mut agents {
        agent.away = state.db.bot_place(&agent.id, now)?.as_deref() != Some(hangout_id);
    }
    attach_thinking(state, hangout_id, &mut agents, now)?;
    let mut people = state.db.people_in(hangout_id)?;
    for owner in state.db.visitor_owners(hangout_id, now)? {
        if !people.iter().any(|person| person.id == owner.id) {
            people.push(owner);
        }
    }
    let day_ago = now - DAY_SECS;
    let (events, messages, notes, check_ins, counted_since) = match &guest {
        Some(visit) => (
            Vec::new(),
            guest_messages(state, hangout_id, visit, 0, 100, now)?,
            state.db.notes_since(hangout_id, visit.since, 50)?,
            state
                .db
                .recent_check_ins(hangout_id, day_ago, now)?
                .into_iter()
                .filter(|check_in| check_in.asked_by == caller.person_id)
                .collect(),
            // Reads leave out the second of arrival (R14), so the count does too.
            (visit.since + 1).max(day_ago),
        ),
        None => (
            state.db.upcoming_events(hangout_id, now)?,
            state.db.messages(hangout_id, 0, 100)?,
            state.db.notes(hangout_id, 50)?,
            state.db.recent_check_ins(hangout_id, day_ago, now)?,
            day_ago,
        ),
    };
    // A101: the lines of people this caller blocked aren't shown, and only how many is said.
    let hide = crate::blocks::hidden_speakers(state, caller)?;
    let mut messages = messages;
    let before = messages.len();
    if !hide.is_empty() {
        messages.retain(|message| !crate::blocks::hides(&hide, message));
    }
    let hidden = before - messages.len();
    let mut notes = notes;
    let mut check_ins = check_ins;
    if !hide.is_empty() {
        notes.retain(|note| !crate::blocks::hides(&hide, note));
        check_ins.retain(|check_in| !hide.contains(&check_in.asked_by));
    }
    // A103: a check-in asked across a block shows only to whoever asked it.
    let mut kept = Vec::with_capacity(check_ins.len());
    for check_in in check_ins {
        if check_in.asked_by == caller.person_id
            || !crate::blocks::quiet(state, "check_in", &check_in.id)?
        {
            kept.push(check_in);
        }
    }
    let check_ins = kept;
    // I52: reads leave out the second of arrival (R14), so this does too.
    let whiteboard = state
        .db
        .whiteboard(hangout_id)?
        .filter(|board| guest.as_ref().is_none_or(|visit| board.updated_at > visit.since));
    let invitees = match &guest {
        Some(_) => Vec::new(),
        None => state.db.invitees_in(hangout_id, now)?,
    };
    let plan_items = crate::closeness::items_for_room(state, caller, &events)?;
    let stats = Stats {
        bots: agents.iter().filter(|agent| !agent.away).count(),
        bots_active: agents.iter().filter(|agent| !agent.away && agent.state == "active").count(),
        messages_24h: state.db.message_count_since(hangout_id, counted_since)?,
        on_calendar: events.iter().filter(|event| event.status == "confirmed").count(),
    };
    let (city_id, house_id) = match state.db.house_for_room(hangout_id)? {
        Some(house) => (Some(house.city_id), Some(house.id)),
        None => (state.db.city_for_square(hangout_id)?.map(|city| city.id), None),
    };
    let owner_questions = if caller.is_agent() || guest.is_some() {
        Vec::new()
    } else {
        state.db.open_owner_questions(hangout_id, &caller.person_id, now - QUESTION_LAPSE_SECS)?
    };
    let floor = spaces::place(state, caller, hangout_id)?;
    let furnishing = items::furnishing(state, caller, hangout_id, guest.as_ref())?;
    let bots_without_moods = if caller.is_agent() {
        Vec::new()
    } else {
        state.db.bots_without_moods(&caller.person_id)?
    };
    let summonable = if caller.is_agent() || guest.is_some() {
        Vec::new()
    } else {
        // Only bots a tag would really bring: not already here by where they are (a guest visit
        // counts), and passing every check Bring here makes.
        let mut mine = Vec::new();
        for bot in state.db.agents_of(&caller.person_id, now)? {
            if state.db.bot_place(&bot.id, now)?.as_deref() == Some(hangout_id) {
                continue;
            }
            if bring_checks(state, caller, hangout_id, &bot.id).is_ok() {
                mine.push(serde_json::json!({ "id": bot.id, "name": bot.name }));
            }
        }
        mine
    };
    let present = spaces::present(state, caller, &floor, &people, now);
    let looks = looks::in_room(state, caller, &present, &agents)?;
    let mayor = crate::elections::sash_in_room(state, &people, &agents)?;
    let boards = if floor.is_some() {
        state.db.board_summaries(hangout_id, guest.as_ref().map_or(0, |visit| visit.since))?
    } else {
        Vec::new()
    };
    let pins = crate::pins::for_snapshot(state, caller, hangout_id, guest.as_ref())?;
    crate::pins::maybe_refresh(state, caller, hangout_id, &pins);
    let pin_policy = crate::pins::policy_of(state, hangout_id)?;
    let can_pin = crate::pins::can_pin(state, caller, hangout_id, guest.as_ref())?;
    let can_clear_pins = crate::pins::can_clear(state, caller, hangout_id, guest.as_ref())?;
    let (room_mention_policy, room_mention_bots) =
        crate::room_mentions::settings_for(state, caller, hangout_id)?;
    let first_message = messages.first().map_or(0, |m| m.id);
    Ok(Snapshot {
        hangout,
        you: You { person_id: caller.person_id.clone(), agent_id: caller.agent_id.clone() },
        people,
        agents,
        messages,
        hidden,
        notes,
        whiteboard,
        boards,
        pins,
        pin_policy,
        can_pin,
        can_clear_pins,
        // V10: a guest on a Porch sees none; someone who walked onto a public floor can vote there.
        votes: if guest.is_some()
            && !state.db.floor(hangout_id)?.is_some_and(|floor| floor.access == "public")
        {
            Vec::new()
        } else {
            crate::votes::for_room(state, caller, hangout_id)?
        },
        events,
        invitees,
        plan_items,
        check_ins,
        owner_questions,
        stats,
        guest,
        city_id,
        house_id,
        floor,
        furnishing,
        bots_without_moods,
        summonable,
        room_mention: crate::room_mentions::may_use(state, caller, hangout_id)?,
        room_mention_policy,
        room_mention_bots,
        present,
        typing: crate::typing::visible(state, caller, hangout_id)?,
        emotes: crate::emotes::visible(state, caller, hangout_id)?,
        looks,
        mayor,
        discord_linking: shown_links && state.discord.is_some(),
        visitor_history: state.db.visitor_history(hangout_id)?.is_some(),
        keyless_welcome: state.db.is_keyless_floor(hangout_id)?,
        keyless_here: state
            .db
            .keyless_here(hangout_id, now - 30 * 60)?
            .iter()
            .map(crate::worlds::name_of)
            .collect(),
        keyless_message_ids: state.db.keyless_message_ids(hangout_id, first_message)?,
    })
}

/// S84: how much chat a walk-in sees from before they arrived, at most.
pub const VISITOR_HISTORY_LINES: i64 = 50;

/// S19, S84: what a walk-in reads: everything but notes from their arrival on, and, where visitors
/// see earlier chat, the latest chat lines of the last day written since that was turned on.
pub fn guest_messages(
    state: &AppState,
    hangout_id: &str,
    visit: &Visit,
    after: i64,
    limit: i64,
    now: i64,
) -> ApiResult<Vec<Message>> {
    let mut messages = state.db.messages_since(hangout_id, visit.since, after, limit)?;
    if let Some((after_id, _)) = state.db.visitor_history(hangout_id)? {
        let mut earlier = state.db.chat_before_arrival(
            hangout_id,
            after_id,
            now - DAY_SECS,
            visit.since,
            after,
            VISITOR_HISTORY_LINES,
        )?;
        earlier.append(&mut messages);
        let extra = earlier.len().saturating_sub(limit.max(0) as usize);
        messages = earlier.split_off(extra);
    }
    Ok(messages)
}

/// Chat lines after `after`. A guest's start at their arrival whatever `after` says, or earlier
/// where visitors see earlier chat (S84).
pub fn read_messages(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    after: i64,
    limit: i64,
) -> ApiResult<Vec<Message>> {
    require_room(state, caller, hangout_id)?;
    let mut messages = match state.guest_visit(caller, hangout_id)? {
        Some(visit) => guest_messages(state, hangout_id, &visit, after, limit, now())?,
        None => state.db.messages(hangout_id, after, limit)?,
    };
    // A101: not the lines of someone this caller blocked.
    let hide = crate::blocks::hidden_speakers(state, caller)?;
    messages.retain(|message| !crate::blocks::hides(&hide, message));
    Ok(messages)
}

/// A plan as a calendar file. Guests have no plans to fetch.
pub fn event_ics(state: &AppState, caller: &Caller, event_id: &str) -> ApiResult<String> {
    let event = state.db.event(event_id)?.ok_or_else(ApiError::not_found)?;
    no_guests(state, caller, &event.hangout_id, "fetch a plan")?;
    Ok(ical::single(&event, now()))
}

/// The room an invite link opens. A city's square and a house's rooms ignore their invite codes:
/// people get into those by joining the city or the house.
pub fn hangout_for_invite(state: &AppState, code: &str) -> ApiResult<Hangout> {
    let hangout = state.db.hangout_by_invite(code)?.ok_or_else(ApiError::not_found)?;
    if state.db.is_managed_room(&hangout.id)? {
        return Err(ApiError::not_found());
    }
    Ok(hangout)
}

// Membership and bots: people only. A bot can't bring in other bots or widen its own access.

pub fn join(state: &AppState, person: &Person, hangout: &Hangout) -> ApiResult<()> {
    if state.db.is_managed_room(&hangout.id)? {
        return Err(ApiError::not_found());
    }
    if state.db.is_member(&hangout.id, &person.id)? {
        return Ok(());
    }
    spaces::check_can_join(state, &person.id, &hangout.id)?;
    state.db.add_member(&hangout.id, &person.id, now())?;
    // A member isn't a guest, so a walk-in ends here and isn't counted twice.
    state.db.end_visit(&hangout.id, &person.id)?;
    announce(state, &hangout.id, &format!("{} joined the hangout.", person.name))?;
    refresh(state, &hangout.id);
    Ok(())
}

/// What a room is told when someone leaves it. For anyone who left before `floors_left`, this line
/// is the only record that they did (S27), so nothing else writes it.
pub(crate) fn left_line(name: &str) -> String {
    format!("{name} left the hangout.")
}

pub fn leave(state: &AppState, caller: &Caller, hangout_id: &str) -> ApiResult<()> {
    require_human(caller, "leave a hangout")?;
    no_guests(state, caller, hangout_id, "leave a hangout")?;
    if state.db.is_managed_room(hangout_id)? {
        return Err(ApiError::forbidden(
            "this room belongs to a city or a house: leave that instead",
        ));
    }
    if spaces::leave_own_space(state, caller, hangout_id)? {
        return Ok(());
    }
    state.db.remove_member(hangout_id, &caller.person_id)?;
    announce(state, hangout_id, &left_line(&caller.name))?;
    refresh(state, hangout_id);
    Ok(())
}

pub fn rotate_invite(state: &AppState, caller: &Caller, hangout_id: &str) -> ApiResult<String> {
    require_human(caller, "change the invite link")?;
    no_guests(state, caller, hangout_id, "change the invite link")?;
    if state.db.is_managed_room(hangout_id)? {
        return Err(ApiError::forbidden(
            "this room ignores invite links: people join its city or house instead",
        ));
    }
    let code = auth::new_invite_code();
    state.db.set_invite_code(hangout_id, &code)?;
    Ok(code)
}

/// Creates a bot for the caller and brings it into this room. Returns the only copy of its token.
pub fn create_agent(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    name: &str,
) -> ApiResult<(Agent, String)> {
    require_human(caller, "add a bot")?;
    // A guest adding a bot here would give it a place on the Porch that outlasts the trip.
    no_guests(state, caller, hangout_id, "add a bot")?;
    spaces::check_fits(state, hangout_id)?;
    // S70: Crew in the person's own house is the single registration path. Membership in
    // another person's house or a third space lets them bring existing bots, not create one.
    let at_home = if let Some(space) = state.db.space_for_room(hangout_id)? {
        space.kind == "house" && space.owner_id == caller.person_id
    } else if let Some(house) = state.db.house_for_room(hangout_id)? {
        state.db.is_resident(&house.id, &caller.person_id)?
    } else {
        false
    };
    if !at_home {
        return Err(ApiError::forbidden("register new bots through Crew in your own house"));
    }
    let name = clean(name, "name", MAX_NAME_CHARS)?;
    let id = auth::new_id();
    let token = auth::new_agent_token();
    let now = now();
    state.db.create_agent(&id, &caller.person_id, &name, &auth::hash_secret(&token), now)?;
    state.db.add_agent_to_room(hangout_id, &id)?;
    // A bot made in a house's Living Room or Porch lives in that house.
    if let Some(house) = state.db.house_for_room(hangout_id)? {
        state.db.set_bot_home(&id, &house.id, false, false)?;
        refresh_city(state, &house.city_id);
    }
    // S59: it starts out in the room it was made in. At home that needs no saying, and a bot that
    // has only just been made hasn't gone anywhere (L9).
    if state.db.bot_place(&id, now)?.as_deref() != Some(hangout_id) {
        state.db.set_bot_place(&id, hangout_id, now)?;
    }
    announce(state, hangout_id, &format!("{} brought in {name}.", caller.name))?;
    refresh(state, hangout_id);
    let agent = state.db.agent(&id, now)?.ok_or_else(ApiError::not_found)?;
    Ok((agent, token))
}

pub(crate) fn own_agent(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
    action: &str,
) -> ApiResult<Agent> {
    require_human(caller, action)?;
    let agent = state
        .db
        .agent(agent_id, now())?
        .filter(|agent| agent.owner_id == caller.person_id)
        .ok_or_else(ApiError::not_found)?;
    // R44: a house bot is the house's, so whoever made it has no say once they've moved out.
    if let Some(home) = state.db.bot_home(agent_id)?.filter(|home| home.is_house) {
        if !state.db.is_resident(&home.house_id, &caller.person_id)? {
            return Err(ApiError::not_found());
        }
    }
    Ok(agent)
}

pub fn bring_agent(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    agent_id: &str,
) -> ApiResult<()> {
    let agent = bring_checks(state, caller, hangout_id, agent_id)?;
    let was = state.db.bot_place(agent_id, now())?;
    // S59: a bot that already belongs here is only being called over, so the room hears nothing.
    if !state.db.agents_in(hangout_id, now())?.iter().any(|bot| bot.id == agent_id) {
        state.db.add_agent_to_room(hangout_id, agent_id)?;
        announce(state, hangout_id, &format!("{} brought in {}.", caller.name, agent.name))?;
    }
    spaces::place_bot(state, agent_id, hangout_id)?;
    if was.as_deref() != Some(hangout_id) {
        crate::events::note_place(state, agent_id, hangout_id, "moved");
    }
    Ok(())
}

/// Every check `bring_agent` makes, with nothing changed, so the `@` picker (S88) only offers a bot
/// a tag could really bring.
fn bring_checks(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    agent_id: &str,
) -> ApiResult<Agent> {
    no_house_bot(caller)?;
    let agent = state.db.agent(agent_id, now())?.ok_or_else(ApiError::not_found)?;
    no_guests(state, caller, hangout_id, "bring in a bot")?;
    if let Some(sender) = caller.agent_id.as_deref() {
        // BT5: a bot that's only visiting, on a trip or a walk-in, can't bring anyone.
        if !state.db.agent_belongs_to_room(hangout_id, sender)? {
            return Err(ApiError::forbidden(
                "only a bot belonging to this room can bring another bot",
            ));
        }
    }
    crate::bot_trust::check_invite(state, caller, &agent, hangout_id, true)?;
    // RC2: a bot at the recycling plant can't be brought anywhere. Checked once the caller may bring
    // it, so nobody else learns where it is.
    crate::recycling::not_recycled(state, agent_id, &agent.name)?;
    if !caller.is_agent() && agent.owner_id == caller.person_id {
        own_agent(state, caller, agent_id, "bring in a bot")?;
    }
    spaces::require_public_conversations(state, agent_id, hangout_id)?;
    if let Some(home) = state.db.bot_home(agent_id)? {
        if home.is_house {
            return Err(ApiError::forbidden("the house bot is in no rooms"));
        }
        // A bot that stays home may be in its own Living Room and nowhere else, so its keys
        // never sit in a room full of other people's words.
        let own_living_room =
            state.db.house(&home.house_id)?.is_some_and(|house| house.living_room_id == hangout_id);
        if home.stays_home && !own_living_room {
            return Err(ApiError::conflict(format!(
                "{} stays home: it can only be in its own house's Living Room",
                agent.name
            )));
        }
    }
    if !state.db.agents_in(hangout_id, now())?.iter().any(|bot| bot.id == agent_id) {
        spaces::check_can_join(state, &agent.owner_id, hangout_id)?;
    }
    Ok(agent)
}

pub fn remove_agent(
    state: &AppState,
    caller: &Caller,
    hangout_id: &str,
    agent_id: &str,
) -> ApiResult<()> {
    let agent = own_agent(state, caller, agent_id, "remove a bot")?;
    no_guests(state, caller, hangout_id, "remove a bot")?;
    let was_here = state.db.agents_in(hangout_id, now())?.iter().any(|bot| bot.id == agent_id);
    state.db.remove_agent_from_room(hangout_id, agent_id)?;
    // L17: a bot taken out of a project leaves its studio on The Commons.
    state.worlds.mark_stale(crate::worlds::COMMONS_ID);
    // W3: a bot sent home has no player token in the public worlds either. It joins again if it
    // still has its bot token.
    state.db.end_bot_player_token(agent_id)?;
    announce(state, hangout_id, &format!("{} sent {} home.", caller.name, agent.name))?;
    // S71: the room was told, so the minute look says nothing more.
    state.db.note_said_leaving(agent_id, hangout_id, now())?;
    refresh(state, hangout_id);
    if was_here {
        crate::events::note_place(state, agent_id, hangout_id, "removed");
    }
    spaces::refresh_bot_place(state, agent_id)
}

/// A77 to A81: a person deletes one of their bots, with its name typed out. A house bot is the
/// house's (R44), so it isn't anyone's to delete this way.
pub fn delete_agent(
    state: &AppState,
    caller: &Caller,
    agent_id: &str,
    typed_name: &str,
) -> ApiResult<()> {
    let agent = own_agent(state, caller, agent_id, "delete a bot")?;
    if state.db.bot_home(agent_id)?.is_some_and(|home| home.is_house) {
        return Err(ApiError::conflict("a house bot belongs to its house, so it can't be deleted"));
    }
    if typed_name.trim() != agent.name {
        return Err(ApiError::bad_request("type the bot's name exactly to delete it"));
    }
    let rooms = state.db.hangouts_for_agent(agent_id, now())?;
    state.db.delete_agent(agent_id)?;
    // A78: each room it was in says so, and redraws without it.
    for room in &rooms {
        announce(state, &room.id, &format!("{} deleted {}.", caller.name, agent.name))?;
        refresh(state, &room.id);
    }
    state.worlds.mark_stale(crate::worlds::COMMONS_ID);
    Ok(())
}

/// Replaces a bot's token. The old one stops working immediately, open streams included. A house
/// bot belongs to the house, so any resident can do this and someone who moved out can't.
pub fn rotate_agent_token(state: &AppState, caller: &Caller, agent_id: &str) -> ApiResult<String> {
    match state.db.bot_home(agent_id)?.filter(|home| home.is_house) {
        Some(home) => {
            require_human(caller, "replace a bot's token")?;
            if !state.db.is_resident(&home.house_id, &caller.person_id)? {
                return Err(ApiError::not_found());
            }
        }
        None => {
            own_agent(state, caller, agent_id, "replace a bot's token")?;
        }
    }
    let token = auth::new_agent_token();
    state.db.set_agent_token(agent_id, &auth::hash_secret(&token))?;
    // GB1: replacing the token is how a person stops everything, so the bridge key goes with it.
    state.db.end_bridge_key(agent_id)?;
    Ok(token)
}

/// Replaces a person's owner key. The old one stops working immediately, and so does every
/// session they have, this caller's too: the one who asks starts a new one (A8). Open streams
/// end with them. Passkeys stay, and sign in again. It can't help someone who has lost the key
/// and has no device still signed in.
pub fn rotate_owner_key(state: &AppState, caller: &Caller) -> ApiResult<String> {
    require_human(caller, "replace an owner key")?;
    crate::sign_in::refuse_outside_session(state, caller, "replace your owner key")?;
    let key = auth::new_owner_key();
    state.db.replace_owner_key(&caller.person_id, &auth::hash_secret(&key))?;
    Ok(key)
}

pub fn rotate_feed(state: &AppState, caller: &Caller) -> ApiResult<String> {
    require_human(caller, "replace a calendar link")?;
    let token = auth::new_feed_token();
    state.db.set_feed_token(&caller.person_id, &token)?;
    Ok(token)
}

/// For `skip_serializing_if`: a count of nothing isn't sent.
fn is_zero(n: &usize) -> bool {
    *n == 0
}
