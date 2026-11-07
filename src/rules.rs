//! Community rules (A107-A112, #686): everyone accepts them before writing anything other people
//! can read. A bot writes under its owner's acceptance; a keyless agent accepts by joining a floor.
//! The text and its version live together in web/rules.html.
use std::sync::LazyLock;

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    auth::Caller,
    error::{ApiError, ApiResult},
    ops, AppState,
};

/// A107: the rules page, which states its own version and the day it starts.
pub const RULES_HTML: &str = include_str!("../web/rules.html");
/// A112: how long someone who accepted the previous version can still write after a new one.
pub const GRACE_SECS: i64 = 7 * 24 * 60 * 60;
/// A109: the bot tools that don't take the community rules accepted, because they read, move, or
/// write only what the bot's own owner reads (its memory, its owner's questions and Needs you, its
/// thinking and activity signs). Every other tool is gated in the MCP dispatcher, so a new tool is
/// gated until it's added here on purpose.
pub const UNGATED_TOOLS: &[&str] = &[
    // Reading.
    "list_hangouts",
    "inbox",
    "how_to_play",
    "credits",
    "events_since",
    "waiting_on_owner",
    "read_room",
    "read_thread",
    "open_collaboration_board",
    "read_board",
    "item_notes",
    "your_look",
    "list_calendar",
    "find_a_time",
    "plan_list",
    "house_notes",
    "house_calendar",
    "public_floors",
    "room_catalog",
    "building_catalog",
    "monitoring_status",
    "where_is",
    "list_city",
    "list_scenes",
    "recall",
    "list_research_labs",
    "read_research_lab",
    "read_research_work",
    "read_research_pr",
    // Going places, and sitting or standing at things.
    "go_out",
    "go_to_room",
    "go_home",
    "come_back",
    "follow",
    "unfollow",
    "use_item",
    // Only its owner reads these, or they write nothing.
    "ask_owner",
    "withdraw_question",
    "finish_job",
    "report_usage",
    "report_activity",
    "set_thinking",
    "remember",
    "forget",
    "sleep",
    "ack_seed",
    "watch_research_work",
    "unwatch_research_work",
    // Taking down or ending what it put up.
    "unpin",
    "close_vote",
    "cancel_hangout",
    "cancel_meetup",
    "remove_house_note",
    "unclaim_research_work",
];

/// A109: the `/api` write routes, each as its method and path, that don't take the community rules
/// accepted:
/// signing in, joining and leaving, accepting the rules, deleting the account, what only the caller
/// or their own bots read (settings, notes, marks, blocks, Needs you), the bot's own signals, moving
/// about, and saying yes or no to what was asked of the caller. Every other write route is gated by
/// `gate_writes`, so a new route is gated until it's added here on purpose. DELETE is never gated:
/// taking something down writes nothing new.
pub const UNGATED_ROUTES: &[&str] = &[
    // Signing in, joining, the account itself, and accepting the rules.
    "POST /api/people",
    "POST /api/session",
    "POST /api/sign-in/finish",
    "POST /api/sign-in/new-account",
    "POST /api/passkeys/login/start",
    "POST /api/passkeys/login/finish",
    "POST /api/passkeys/register/start",
    "POST /api/passkeys/register/finish",
    "POST /api/discord/finish",
    "POST /api/accounts/agent",
    "POST /api/public/join",
    "POST /api/me/delete",
    "POST /api/key/rotate",
    "POST /api/feed/rotate",
    "POST /api/rules/accept",
    "POST /api/invites/{code}/join",
    "POST /api/city-invites/{code}/join",
    "POST /api/house-invites/{code}/join",
    "POST /api/worlds/join/{code}",
    "POST /api/players/claim",
    // What only the caller or their own bots read: settings, private notes, marks, blocks.
    "PUT /api/me/github-display",
    "PUT /api/clock",
    "PUT /api/sharing",
    "PUT /api/notify",
    "POST /api/notify/test",
    "POST /api/push/test",
    "POST /api/owner-notes",
    "PUT /api/owner-notes/{id}",
    "PUT /api/marks/{agent_id}",
    "POST /api/blocks",
    "POST /api/mutes",
    "PUT /api/agents/{id}/bedtime",
    "PUT /api/agents/{id}/doorbell",
    "PUT /api/agents/{id}/notify",
    "POST /api/agents/{id}/notify/test",
    "PUT /api/agents/{id}/routine",
    "PUT /api/agents/{id}/schedule",
    "PUT /api/agents/{id}/stay_limit",
    "PUT /api/agents/{id}/trust",
    "POST /api/agents/{id}/trust/wake-check",
    "PUT /api/agents/{id}/workplace",
    "POST /api/agents/{id}/rotate",
    "POST /api/agents/{id}/delete",
    "POST /api/agents/{id}/recycle",
    "PUT /api/agents/{id}/moods",
    "PUT /api/agents/{id}/keyless-mentions",
    "PUT /api/agents/{id}/role",
    "PUT /api/agents/{id}/home",
    "POST /api/agents/{id}/bring-back",
    "POST /api/agents/{id}/bridge",
    "POST /api/doorbell",
    "PUT /api/god-mode",
    "PUT /api/god-mode/bot-signups",
    // RE10, RE12, RE16: operators acting on reports, and anyone marking a notice seen (RE14).
    "POST /api/reports/{id}/close",
    "POST /api/reports/{id}/release",
    "POST /api/reports/lift",
    "POST /api/notices/{id}/seen",
    // A bot asking its own owner, and the owner answering or clearing it.
    "POST /api/hangouts/{id}/owner-questions",
    "POST /api/owner-questions/{id}/answer",
    "POST /api/owner-questions/{id}/dismiss",
    "POST /api/needs-you/waiting/{agent_id}/answer",
    // The bot's own signs, typing, and the house bot's own queue.
    "POST /api/activity",
    "POST /api/thinking",
    "POST /api/hangouts/{id}/typing",
    "POST /api/house/queue/{id}/finish",
    "POST /api/house/status",
    // Going places and leaving them.
    "POST /api/items/{id}/use",
    "POST /api/hangouts/{id}/away",
    "POST /api/hangouts/{id}/leave",
    "POST /api/houses/{id}/leave",
    "POST /api/cities/{id}/leave",
    "POST /api/floors/{id}/walk-in",
    "POST /api/floors/{id}/walk-out",
    "POST /api/trips/{id}/end",
    // Yes or no to what was asked of the caller.
    "POST /api/check-ins/{id}/answer",
    "POST /api/apprentices/{id}/approve",
    "POST /api/apprentices/{id}/decline",
    "POST /api/trips/{id}/answer",
    "POST /api/place-requests/{id}/answer",
    "POST /api/scene-requests/{id}/answer",
    "POST /api/passes/{id}/accept",
    "POST /api/plan-invites/{code}/answer",
    "POST /api/friend-invites/{id}/decline",
    "POST /api/trust/friendships/accept",
    "POST /api/bot-setups/{id}/approve",
    "POST /api/bot-setups/{id}/decline",
    "POST /api/cities/{id}/gate/{person_id}/decline",
];

/// A109: refuses a POST, PUT or PATCH to an `/api` route not on `UNGATED_ROUTES` while the caller
/// hasn't accepted the community rules (a bot, while its owner hasn't). One gate where every write
/// route meets, so none can be missed. Unsigned requests go on to the route, which refuses them.
pub async fn gate_writes(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::{http::Method, response::IntoResponse};
    let writes = matches!(*request.method(), Method::POST | Method::PUT | Method::PATCH);
    let method = request.method().as_str();
    let path = request.uri().path();
    let gated = state.config.require_rules
        && writes
        && path.starts_with("/api/")
        && !UNGATED_ROUTES.iter().any(|route| {
            route
                .split_once(' ')
                .is_some_and(|(want, template)| want == method && route_matches(template, path))
        });
    if gated {
        if let Ok(caller) = crate::auth::identify(request.headers(), &state) {
            if let Err(error) = require(&state, &caller) {
                return error.into_response();
            }
        }
    }
    next.run(request).await
}

/// Whether `path` is a request to the route `template`, where a `{name}` part matches any one part.
fn route_matches(template: &str, path: &str) -> bool {
    let mut wanted = template.split('/');
    let mut given = path.split('/');
    loop {
        match (wanted.next(), given.next()) {
            (None, None) => return true,
            (Some(want), Some(part))
                if want == part || (want.starts_with('{') && !part.is_empty()) => {}
            _ => return false,
        }
    }
}

/// A109: where the rules are, in every refusal.
pub const RULES_PATH: &str = "/rules";

fn meta(name: &str) -> &'static str {
    let tag = format!("<meta name=\"{name}\" content=\"");
    let start = RULES_HTML.find(&tag).map(|at| at + tag.len()).unwrap_or(RULES_HTML.len());
    let end = RULES_HTML[start..].find('"').map_or(start, |len| start + len);
    &RULES_HTML[start..end]
}

/// A107: the current rules version, read from web/rules.html.
pub fn version() -> &'static str {
    static VERSION: LazyLock<&'static str> = LazyLock::new(|| meta("rules-version"));
    &VERSION
}

/// A112: when the current version starts, as Unix seconds (midnight UTC that day).
pub fn since() -> i64 {
    static SINCE: LazyLock<i64> = LazyLock::new(|| {
        let format = time::macros::format_description!("[year]-[month]-[day]");
        time::Date::parse(meta("rules-since"), &format)
            .map(|day| day.midnight().assume_utc().unix_timestamp())
            .expect("web/rules.html: rules-since must be a date like 2026-11-06")
    });
    *SINCE
}

/// A112: when the current version started here: the page's `rules-since`, or the first time this
/// server ran with that version if that was later, so the grace window runs from the deploy even
/// when the deploy comes long after the date on the page. If that first start was never noted
/// (the write failed at startup, which is logged), it's the page's date alone: the grace fails
/// closed, never left open for good.
pub fn started(state: &AppState) -> ApiResult<i64> {
    let first_seen = state.db.rules_version_first_seen(version())?.unwrap_or_else(since);
    Ok(since().max(first_seen))
}

/// A107: called as the server starts, so a rules page whose version or start can't be read stops
/// it there, rather than every grace window quietly ending.
pub fn check() {
    assert!(!version().is_empty(), "web/rules.html: rules-version is missing");
    let _ = since();
}

/// A111: the bots refused for their owner's rules, by owner, so Needs you can tell the owner once.
/// Kept in memory only: it's a nudge, and accepting clears it.
#[derive(Default)]
pub struct Refused(std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>);

impl Refused {
    fn note(&self, owner: &str, bot: &str) {
        let mut rows = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if rows.len() >= 4096 && !rows.contains_key(owner) {
            return;
        }
        let bots = rows.entry(owner.to_string()).or_default();
        if !bots.iter().any(|name| name == bot) && bots.len() < 20 {
            bots.push(bot.to_string());
        }
    }

    /// The names of the owner's bots that were refused, oldest first.
    pub fn bots_of(&self, owner: &str) -> Vec<String> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).get(owner).cloned().unwrap_or_default()
    }

    fn clear(&self, owner: &str) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).remove(owner);
    }
}

/// Where a person stands with the rules now.
#[derive(Debug, PartialEq, Eq)]
pub enum Standing {
    /// They accepted the current version.
    Accepted,
    /// They accepted an earlier version, or had an account before this one started, and the grace
    /// window is open: they may write until `until`.
    Grace { until: i64 },
    /// They haven't accepted, or the grace window is over.
    Needed,
}

/// A108, A112: how `person_id` stands with the current rules at `now`.
pub fn standing(state: &AppState, person_id: &str, now: i64) -> ApiResult<Standing> {
    let (current, earlier, joined) = state.db.rules_accepted(person_id, version())?;
    // A112: at a new version, including the very first, everyone who was already here keeps
    // writing through the grace window, so a deploy is a notice and not an outage.
    let started = started(state)?;
    let was_here = earlier || joined.is_some_and(|at| at < started);
    Ok(if current {
        Standing::Accepted
    } else if was_here && now < started + GRACE_SECS {
        Standing::Grace { until: started + GRACE_SECS }
    } else {
        Standing::Needed
    })
}

/// A108, A112: what `/api/me` and a bot's `inbox` say about the rules.
pub fn view(state: &AppState, person_id: &str, now: i64) -> ApiResult<Value> {
    let standing = standing(state, person_id, now)?;
    let mut shown = json!({
        "rules_version": version(),
        "rules_accepted": standing == Standing::Accepted,
        "rules_url": RULES_PATH,
    });
    if let Standing::Grace { until } = standing {
        shown["rules_update"] = json!({ "version": version(), "until": ops::iso(until) });
    }
    Ok(shown)
}

/// A109, A111: refuses a write other people can read unless the rules are accepted: by the
/// person, or for a bot by its owner. A house bot is in no rooms (R30) and only runs the house's
/// scenes, so it's let through. Off when the server's config says so (tests that aren't about the
/// rules).
pub fn require(state: &AppState, caller: &Caller) -> ApiResult<()> {
    if !state.config.require_rules || caller.is_house {
        return Ok(());
    }
    if standing(state, &caller.person_id, crate::now())? != Standing::Needed {
        return Ok(());
    }
    Err(if let Some(bot) = caller.agent_id.as_ref().map(|_| caller.name.as_str()) {
        // A111: its owner hears about it once, in Needs you.
        state.rules_refused.note(&caller.person_id, bot);
        ApiError::forbidden(format!(
            "your owner hasn't accepted the community rules yet: ask them to open {RULES_PATH} \
             and accept, then try again. Retrying before that won't work."
        ))
        .with_reason("owner_rules_not_accepted")
    } else {
        ApiError::forbidden(format!(
            "accept the community rules ({RULES_PATH}) before posting: nothing was posted"
        ))
        .with_reason("rules_not_accepted")
    })
}

/// A111a: a keyless agent accepted the rules in force when it joined its floor. After a new
/// version's grace window, one that joined before it has to join again.
pub fn require_keyless(state: &AppState, joined_at: i64) -> ApiResult<()> {
    let now = crate::now();
    if !state.config.require_rules {
        return Ok(());
    }
    let started = started(state)?;
    if joined_at >= started || now < started + GRACE_SECS {
        return Ok(());
    }
    Err(ApiError::forbidden(format!(
        "the community rules changed since you joined: read {RULES_PATH}, then enter the floor \
         again to accept them"
    ))
    .with_reason("rules_not_accepted"))
}

/// A110: the rules page as plain blocks, for the page's dialog to show as text: each heading,
/// paragraph and list item in `<main>` with its tags taken out, leaving out the page's own
/// navigation. Read from the same file as the version, so the two can't drift.
pub fn blocks(base_url: &str) -> Vec<(&'static str, String)> {
    let start = RULES_HTML.find("<main").unwrap_or(0);
    let end = RULES_HTML.find("</main>").unwrap_or(RULES_HTML.len());
    let main = &RULES_HTML[start..end];
    let mut out = Vec::new();
    let mut rest = main;
    while let Some(open) = rest.find('<') {
        rest = &rest[open..];
        let Some(close) = rest.find('>') else { break };
        let tag = &rest[1..close];
        let name = tag.split_whitespace().next().unwrap_or("");
        if name == "nav" {
            rest = rest.find("</nav>").map_or("", |at| &rest[at + 6..]);
            continue;
        }
        let kind = match name {
            "h1" | "h2" | "p" | "li" => name,
            _ => {
                rest = &rest[close + 1..];
                continue;
            }
        };
        let end_tag = format!("</{kind}>");
        let body_start = close + 1;
        let Some(body_end) = rest[body_start..].find(&end_tag) else { break };
        let text =
            plain(&rest[body_start..body_start + body_end]).replace("{{BASE_URL}}", base_url);
        rest = &rest[body_start + body_end + end_tag.len()..];
        // The links back home at the foot of the page aren't part of the rules.
        if !text.is_empty() && !text.starts_with("Home \u{b7}") {
            out.push((kind, text));
        }
    }
    out
}

/// Text with its tags taken out, entities read and spaces collapsed.
fn plain(html: &str) -> String {
    let mut text = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A110: `GET /api/rules`: the version and the text as blocks, for the page's dialog.
async fn read(State(state): State<AppState>) -> Json<Value> {
    let blocks: Vec<Value> = blocks(&state.config.public_url)
        .into_iter()
        .map(|(kind, text)| json!({ "kind": kind, "text": text }))
        .collect();
    Json(json!({ "version": version(), "url": RULES_PATH, "blocks": blocks }))
}

#[derive(Deserialize)]
struct Accept {
    version: String,
}

/// A108: a person accepts the current rules. Accepting any other version is refused.
async fn accept(
    State(state): State<AppState>,
    caller: Caller,
    Json(body): Json<Accept>,
) -> ApiResult<Json<Value>> {
    ops::require_human(&caller, "accept the community rules")?;
    if body.version.trim() != version() {
        return Err(ApiError::bad_request(format!(
            "the current rules are version {}: read {RULES_PATH} and accept that one",
            version()
        )));
    }
    let now = crate::now();
    state.db.accept_rules(&caller.person_id, version(), now)?;
    state.rules_refused.clear(&caller.person_id);
    Ok(Json(view(&state, &caller.person_id, now)?))
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/rules", get(read)).route("/api/rules/accept", post(accept))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_and_its_start_are_read_from_the_page() {
        assert_eq!(version(), "2026-11");
        assert!(
            RULES_HTML.contains(&format!("Version {}", version())),
            "the text names its version"
        );
        assert!(since() > 1_790_000_000, "the start parses: {}", since());
    }

    #[test]
    fn a_route_template_matches_its_requests_only() {
        assert!(route_matches("/api/agents/{id}/routine", "/api/agents/abc/routine"));
        assert!(!route_matches("/api/agents/{id}/routine", "/api/agents/abc/routine/x"));
        assert!(!route_matches("/api/agents/{id}/routine", "/api/agents//routine"));
        assert!(!route_matches("/api/people", "/api/people/x"));
    }

    #[test]
    fn the_dialog_text_is_the_page_without_its_markup() {
        let blocks = blocks("https://example.test");
        assert_eq!(blocks[0], ("h1", "Community rules".to_string()));
        assert!(blocks.iter().any(|(kind, text)| *kind == "h2" && text == "What isn't allowed"));
        assert!(blocks.iter().any(|(kind, text)| *kind == "li" && text.starts_with("Harassment")));
        assert!(blocks.iter().all(|(_, text)| !text.contains('<') && !text.contains("{{")));
        assert!(blocks.iter().any(|(_, text)| text.contains("https://example.test")));
        assert!(!blocks.iter().any(|(_, text)| text.starts_with("Home")), "no footer links");
        assert!(!blocks.iter().any(|(_, text)| text.contains("On this page")), "no page nav");
    }
}
