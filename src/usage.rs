//! Usage monitoring (docs/usage.md, U1 to U21): counts of what the server did, for the people who run
//! it and for a bot's owner. Counts only. Nothing a person said, and nobody's history, goes in.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{BuildHasher, RandomState},
    sync::{Arc, Mutex, PoisonError},
};

use axum::{
    extract::{MatchedPath, Path, Query, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    auth::Caller,
    error::{ApiError, ApiResult},
    god_mode, now, AppState,
};

/// U8: how long counts are kept.
pub const KEEP_DAYS: i64 = 30;
/// U3: a call this slow (milliseconds) is counted as slow.
const SLOW_MILLIS: u64 = 1000;
/// The most distinct buckets held between two flushes, so nothing can grow memory without end.
const MAX_BUCKETS: usize = 20_000;
/// The most distinct people or bots held for one day.
const MAX_ACTIVE: usize = 500_000;
const UNKNOWN_TOOL: &str = "unknown_tool";
/// U16: the upper edge of each response-time bucket, in milliseconds. A response over the last edge
/// falls in one more bucket, so the list is fixed and the memory for it is too.
pub const EDGES_MS: [u64; 13] = [1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10_000];
/// How many buckets a histogram has: one for each edge and one for everything slower.
pub const BUCKETS: usize = EDGES_MS.len() + 1;
/// U17: the counts that are not a tool or a route.
/// I113: the waiting push's sent, accepted (a 2xx) and acknowledged (the bot called back) pulses.
pub const LINES: [&str; 7] = [
    "events_delivered",
    "long_polls",
    "webhooks_sent",
    "webhooks_failed",
    "pulses_sent",
    "pulses_accepted",
    "pulses_acknowledged",
];

/// U16: which bucket a response time falls in.
pub fn bucket_of(millis: u64) -> usize {
    EDGES_MS.iter().position(|edge| millis <= *edge).unwrap_or(EDGES_MS.len())
}

/// U16: the response time that `share` (0 to 1) of the responses were at or under, as the upper edge of
/// the bucket it falls in. It is never lower than the real figure, and never off by more than the
/// bucket's width. The last bucket answers with the last edge. `None` when nothing was timed.
pub fn percentile(hist: &[i64; BUCKETS], share: f64) -> Option<u64> {
    let total: i64 = hist.iter().sum();
    if total <= 0 {
        return None;
    }
    let need = ((total as f64) * share).ceil().max(1.0) as i64;
    let mut seen = 0;
    for (index, n) in hist.iter().enumerate() {
        seen += n;
        if seen >= need {
            return Some(EDGES_MS[index.min(EDGES_MS.len() - 1)]);
        }
    }
    Some(EDGES_MS[EDGES_MS.len() - 1])
}

/// U13: what a count is split by, for telling one release from the next. The binary doesn't know its
/// release number (the tag is made after it is built), so this is the commit it was built from when
/// the build says so, and `local` for a build on someone's machine.
pub fn build() -> &'static str {
    match option_env!("GITHUB_SHA") {
        Some(sha) if sha.len() >= 12 => &sha[..12],
        _ => "local",
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/usage", get(server_usage))
        .route("/api/agents/{id}/usage", get(agent_usage))
}

/// Who made a call. It is used to count and then dropped (U5, U6).
#[derive(Clone, Copy)]
pub enum Who<'a> {
    Person(&'a str),
    Bot(&'a str),
    Nobody,
}

impl Who<'_> {
    fn label(self) -> &'static str {
        match self {
            Who::Person(_) => "person",
            Who::Bot(_) => "bot",
            Who::Nobody => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Outcome {
    Ok,
    Refused,
    Error,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Refused => "refused",
            Outcome::Error => "error",
        }
    }
}

/// U2: the reason for a refusal, from the status and never from the words.
fn reason_of(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 | 422 => "bad_request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        413 => "too_large",
        429 => "rate_limited",
        _ => "other",
    }
}

fn outcome_of(status: StatusCode) -> (Outcome, &'static str) {
    if status.is_server_error() {
        (Outcome::Error, "server_error")
    } else if status.is_client_error() {
        (Outcome::Refused, reason_of(status))
    } else {
        (Outcome::Ok, "")
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Key {
    hour: i64,
    surface: &'static str,
    name: String,
    caller: &'static str,
    outcome: Outcome,
    reason: &'static str,
}

#[derive(Default, Clone, Copy)]
struct Count {
    calls: i64,
    millis: i64,
    timed: i64,
    slow: i64,
}

/// Hour, surface and tool or route.
type LatencyKey = (i64, &'static str, String);
type BotKey = (String, i64, Outcome, &'static str);

#[derive(Default)]
struct Inner {
    hours: HashMap<Key, Count>,
    /// A bot's own totals for a day (U7, U11): agent, day, outcome and reason.
    bots: HashMap<BotKey, i64>,
    /// U16: response times by hour, tool or route and surface, as a fixed list of bucket counts.
    latency: HashMap<LatencyKey, [i64; BUCKETS]>,
    /// U17: the counts that are not a tool or a route, by hour.
    lines: HashMap<(i64, &'static str), i64>,
    /// U22: events handed to each bot, by bot and day.
    bot_events: HashMap<(String, i64), i64>,
    /// U26: characters of tool results sent to each bot, by bot and day.
    bot_chars: HashMap<(String, i64), i64>,
    day: i64,
    people: HashSet<u64>,
    bots_seen: HashSet<u64>,
    /// U5: made new each day, and never written anywhere.
    salt: RandomState,
    /// Days that ended before they were written: the day and the two numbers.
    finished: Vec<(i64, i64, i64)>,
    /// U15: the same for the hour, with a value of its own that is replaced each hour.
    hour: i64,
    hour_people: HashSet<u64>,
    hour_bots: HashSet<u64>,
    hour_salt: RandomState,
    finished_hours: Vec<(i64, i64, i64)>,
}

/// What is waiting to be written, taken out of memory in one go.
#[derive(Default)]
pub struct Pending {
    hours: Vec<(Key, Count)>,
    bots: Vec<(BotKey, i64)>,
    days: Vec<(i64, i64, i64)>,
    active_hours: Vec<(i64, i64, i64)>,
    lines: Vec<((i64, &'static str), i64)>,
    latency: Vec<(LatencyKey, [i64; BUCKETS])>,
    bot_events: Vec<((String, i64), i64)>,
    bot_chars: Vec<((String, i64), i64)>,
}

/// U5: the hash that stands for a person for one day is made with a value kept only here.
pub struct Usage {
    inner: Mutex<Inner>,
    names: Vec<String>,
}

impl Usage {
    pub fn new(names: Vec<String>) -> Self {
        Self { inner: Mutex::new(Inner::default()), names }
    }

    /// U1: a tool name that is listed, and `unknown_tool` for anything else.
    pub fn tool_name(&self, name: &str) -> String {
        if self.names.iter().any(|known| known == name) {
            name.to_string()
        } else {
            UNKNOWN_TOOL.to_string()
        }
    }

    /// A request refused before it named a tool: bad credentials, another site's page (U1, U2).
    pub fn record_rejected(&self, at: i64, surface: &'static str, who: Who, status: StatusCode) {
        let (outcome, reason) = outcome_of(status);
        self.record_outcome(at, surface, "rejected_request", who, outcome, reason, None);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// U4: one call, added in memory. `millis` is `None` for a wait that is meant to stay open (U3).
    pub fn record(
        &self,
        at: i64,
        surface: &'static str,
        name: &str,
        who: Who,
        status: StatusCode,
        millis: Option<u64>,
    ) {
        let (outcome, reason) = outcome_of(status);
        self.record_outcome(at, surface, name, who, outcome, reason, millis);
    }

    /// The same for a tool, whose error goes to the model as a result with a status behind it.
    pub fn record_tool(
        &self,
        at: i64,
        surface: &'static str,
        name: &str,
        who: Who,
        failure: Option<StatusCode>,
        millis: Option<u64>,
    ) {
        let name = self.tool_name(name);
        let (outcome, reason) = match failure.map(outcome_of) {
            None => (Outcome::Ok, ""),
            // A tool that returned an error is a refusal even when the status behind it is odd.
            Some((Outcome::Ok, _)) => (Outcome::Refused, "other"),
            Some(other) => other,
        };
        self.record_outcome(at, surface, &name, who, outcome, reason, millis);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_outcome(
        &self,
        at: i64,
        surface: &'static str,
        name: &str,
        who: Who,
        outcome: Outcome,
        reason: &'static str,
        millis: Option<u64>,
    ) {
        let day = at.div_euclid(86_400);
        let mut inner = self.lock();
        Self::roll_inner(&mut inner, at);
        match who {
            Who::Person(id) => {
                if inner.people.len() < MAX_ACTIVE {
                    let hashed = inner.salt.hash_one(id);
                    inner.people.insert(hashed);
                }
                if inner.hour_people.len() < MAX_ACTIVE {
                    let hashed = inner.hour_salt.hash_one(id);
                    inner.hour_people.insert(hashed);
                }
            }
            Who::Bot(id) => {
                if inner.bots_seen.len() < MAX_ACTIVE {
                    let hashed = inner.salt.hash_one(id);
                    inner.bots_seen.insert(hashed);
                }
                if inner.hour_bots.len() < MAX_ACTIVE {
                    let hashed = inner.hour_salt.hash_one(id);
                    inner.hour_bots.insert(hashed);
                }
            }
            Who::Nobody => {}
        }
        if let Some(millis) = millis {
            // U16: the response time goes into a bucket of a fixed list, never kept as a sample.
            let key = (at.div_euclid(3600), surface, name.to_string());
            if inner.latency.len() < MAX_BUCKETS || inner.latency.contains_key(&key) {
                inner.latency.entry(key).or_insert([0; BUCKETS])[bucket_of(millis)] += 1;
            }
        }
        let key = Key {
            hour: at.div_euclid(3600),
            surface,
            name: name.to_string(),
            caller: who.label(),
            outcome,
            reason,
        };
        if inner.hours.len() < MAX_BUCKETS || inner.hours.contains_key(&key) {
            let count = inner.hours.entry(key).or_default();
            count.calls += 1;
            if let Some(millis) = millis {
                count.millis += millis.min(3_600_000) as i64;
                count.timed += 1;
                count.slow += i64::from(millis >= SLOW_MILLIS);
            }
        }
        if let Who::Bot(id) = who {
            let key = (id.to_string(), day, outcome, reason);
            if inner.bots.len() < MAX_BUCKETS || inner.bots.contains_key(&key) {
                *inner.bots.entry(key).or_default() += 1;
            }
        }
    }

    /// U5, U15: a new day writes down the old day's two numbers, and a new hour the old hour's. Each
    /// empties its sets and changes its salt.
    fn roll_inner(inner: &mut Inner, at: i64) {
        let day = at.div_euclid(86_400);
        let hour = at.div_euclid(3600);
        if inner.hour != hour {
            if inner.hour != 0 && (!inner.hour_people.is_empty() || !inner.hour_bots.is_empty()) {
                let finished =
                    (inner.hour, inner.hour_people.len() as i64, inner.hour_bots.len() as i64);
                inner.finished_hours.push(finished);
            }
            inner.hour = hour;
            inner.hour_people = HashSet::new();
            inner.hour_bots = HashSet::new();
            inner.hour_salt = RandomState::new();
        }
        if inner.day == day {
            return;
        }
        if inner.day != 0 && (!inner.people.is_empty() || !inner.bots_seen.is_empty()) {
            let finished = (inner.day, inner.people.len() as i64, inner.bots_seen.len() as i64);
            inner.finished.push(finished);
        }
        inner.day = day;
        inner.people = HashSet::new();
        inner.bots_seen = HashSet::new();
        inner.salt = RandomState::new();
    }

    /// U5, U15: rolls the day and the hour over at a time with no request in it, so an idle server
    /// doesn't hold the people of a period that is over.
    pub fn roll(&self, at: i64) {
        Self::roll_inner(&mut self.lock(), at);
    }

    /// U17: `n` more of something that is not a tool or a route.
    pub fn note_line(&self, at: i64, line: &'static str, n: i64) {
        debug_assert!(LINES.contains(&line));
        if n <= 0 {
            return;
        }
        let mut inner = self.lock();
        *inner.lines.entry((at.div_euclid(3600), line)).or_default() += n;
    }

    /// U17, U22: `n` events handed to a bot, counted for the server and for the bot's own day.
    pub fn note_events(&self, at: i64, agent_id: &str, n: i64) {
        self.note_line(at, "events_delivered", n);
        if n <= 0 {
            return;
        }
        let key = (agent_id.to_string(), at.div_euclid(86_400));
        let mut inner = self.lock();
        if inner.bot_events.len() < MAX_BUCKETS || inner.bot_events.contains_key(&key) {
            *inner.bot_events.entry(key).or_default() += n;
        }
    }

    /// U26: `n` characters of tool results sent to a bot, for its own day. Capped as U22 is.
    pub fn note_chars(&self, at: i64, agent_id: &str, n: i64) {
        if n <= 0 {
            return;
        }
        let key = (agent_id.to_string(), at.div_euclid(86_400));
        let mut inner = self.lock();
        if inner.bot_chars.len() < MAX_BUCKETS || inner.bot_chars.contains_key(&key) {
            *inner.bot_chars.entry(key).or_default() += n;
        }
    }

    /// How many bot-days of characters are waiting in memory.
    pub fn held_bot_chars(&self) -> usize {
        self.lock().bot_chars.len()
    }

    /// How many bot-days of events are waiting in memory.
    pub fn held_bot_events(&self) -> usize {
        self.lock().bot_events.len()
    }

    /// How many people and bots are held for the current hour in memory.
    pub fn held_hour(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.hour_people.len(), inner.hour_bots.len())
    }

    /// How many histograms and line counts are waiting in memory.
    pub fn held_counts(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.latency.len(), inner.lines.len())
    }

    /// How many people and bots are held for the day in memory.
    pub fn held(&self) -> (usize, usize) {
        let inner = self.lock();
        (inner.people.len(), inner.bots_seen.len())
    }

    /// Takes what has been counted and not yet written.
    fn take(&self) -> Pending {
        let mut inner = self.lock();
        let mut days = std::mem::take(&mut inner.finished);
        if !inner.people.is_empty() || !inner.bots_seen.is_empty() {
            days.push((inner.day, inner.people.len() as i64, inner.bots_seen.len() as i64));
        }
        let mut active_hours = std::mem::take(&mut inner.finished_hours);
        if !inner.hour_people.is_empty() || !inner.hour_bots.is_empty() {
            active_hours.push((
                inner.hour,
                inner.hour_people.len() as i64,
                inner.hour_bots.len() as i64,
            ));
        }
        Pending {
            active_hours,
            lines: std::mem::take(&mut inner.lines).into_iter().collect(),
            latency: std::mem::take(&mut inner.latency).into_iter().collect(),
            hours: std::mem::take(&mut inner.hours).into_iter().collect(),
            bots: std::mem::take(&mut inner.bots).into_iter().collect(),
            bot_events: std::mem::take(&mut inner.bot_events).into_iter().collect(),
            bot_chars: std::mem::take(&mut inner.bot_chars).into_iter().collect(),
            days,
        }
    }

    /// U4: writes what has been counted. If the database refuses, those counts are lost.
    pub fn flush(&self, state: &AppState) {
        self.roll(now());
        let pending = self.take();
        if pending.hours.is_empty()
            && pending.bots.is_empty()
            && pending.days.is_empty()
            && pending.active_hours.is_empty()
            && pending.lines.is_empty()
            && pending.latency.is_empty()
            && pending.bot_events.is_empty()
            && pending.bot_chars.is_empty()
        {
            return;
        }
        let rows = UsageRows {
            version: build(),
            hours: pending
                .hours
                .iter()
                .map(|(k, c)| HourRow {
                    hour: k.hour,
                    surface: k.surface,
                    name: &k.name,
                    caller: k.caller,
                    outcome: k.outcome.label(),
                    reason: k.reason,
                    calls: c.calls,
                    millis: c.millis,
                    timed: c.timed,
                    slow: c.slow,
                })
                .collect(),
            bots: pending
                .bots
                .iter()
                .map(|((agent, day, outcome, reason), calls)| {
                    (agent.as_str(), *day, outcome.label(), *reason, *calls)
                })
                .collect(),
            days: &pending.days,
            active_hours: &pending.active_hours,
            lines: pending.lines.iter().map(|((hour, line), n)| (*hour, *line, *n)).collect(),
            latency: pending
                .latency
                .iter()
                .map(|((hour, surface, name), hist)| (*hour, *surface, name.as_str(), *hist))
                .collect(),
            bot_events: pending
                .bot_events
                .iter()
                .map(|((agent, day), events)| (agent.as_str(), *day, *events))
                .collect(),
            bot_chars: pending
                .bot_chars
                .iter()
                .map(|((agent, day), chars)| (agent.as_str(), *day, *chars))
                .collect(),
        };
        if let Err(error) = state.db.write_usage(&rows) {
            tracing::error!(%error, "usage counts could not be written");
        }
    }
}

pub struct HourRow<'a> {
    pub hour: i64,
    pub surface: &'a str,
    pub name: &'a str,
    pub caller: &'a str,
    pub outcome: &'a str,
    pub reason: &'a str,
    pub calls: i64,
    pub millis: i64,
    pub timed: i64,
    pub slow: i64,
}

/// What `Db::write_usage` takes: rows that borrow from the counts.
#[derive(Default)]
pub struct UsageRows<'a> {
    pub version: &'a str,
    pub hours: Vec<HourRow<'a>>,
    /// `(agent, day, outcome, reason, calls)`
    pub bots: Vec<(&'a str, i64, &'a str, &'a str, i64)>,
    /// `(day, people, bots)`
    pub days: &'a [(i64, i64, i64)],
    /// U15: `(hour, people, bots)`
    pub active_hours: &'a [(i64, i64, i64)],
    /// U17: `(hour, line, count)`
    pub lines: Vec<(i64, &'a str, i64)>,
    /// U16: `(hour, surface, name, buckets)`
    pub latency: Vec<(i64, &'a str, &'a str, [i64; BUCKETS])>,
    /// U22: `(agent, day, events)`
    pub bot_events: Vec<(&'a str, i64, i64)>,
    /// U26: `(agent, day, chars)`
    pub bot_chars: Vec<(&'a str, i64, i64)>,
}

/// U16: one hour's response-time buckets for a tool or route.
pub struct StoredLatency {
    pub hour: i64,
    pub surface: String,
    pub name: String,
    pub version: String,
    pub hist: [i64; BUCKETS],
}

/// One row read back for the report, summed over its hours.
pub struct StoredHour {
    pub day: i64,
    pub surface: String,
    pub name: String,
    pub version: String,
    pub outcome: String,
    pub reason: String,
    pub calls: i64,
    pub millis: i64,
    pub timed: i64,
    pub slow: i64,
}

/// Run at startup and every minute (U4, U8), as `camera::run_cleanup` is.
pub async fn run(state: AppState) {
    let mut ticks = tokio::time::interval(std::time::Duration::from_secs(60));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let worker = state.clone();
        let done = tokio::task::spawn_blocking(move || {
            worker.usage.flush(&worker);
            // RE16: and closed reports' copies past their 90 days.
            if let Err(error) = crate::review::drop_old_copies(&worker) {
                tracing::error!(status = %error.status, "old report copies could not be dropped");
            }
            worker.db.sweep_usage(now())
        })
        .await;
        match done {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::error!(%error, "old usage counts could not be deleted"),
            Err(error) => tracing::error!(%error, "usage worker failed"),
        }
    }
}

/// Filled by the `Caller` extractor, so the middleware learns who asked without asking again.
#[derive(Clone, Default)]
pub struct Seen(Arc<Mutex<Option<(bool, String)>>>);

impl Seen {
    pub fn set(&self, is_bot: bool, id: &str) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some((is_bot, id.to_string()));
    }
}

/// U1, U3: counts every `/api/` request once, with its route pattern and the answer's status.
pub async fn observe(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    if !request.uri().path().starts_with("/api/") {
        return next.run(request).await;
    }
    let name = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_string(), |path| path.as_str().to_string());
    let seen = Seen::default();
    request.extensions_mut().insert(seen.clone());
    let started = std::time::Instant::now();
    let response = next.run(request).await;
    let waits = name.ends_with("/stream") || name == "/api/events";
    let millis = (!waits).then(|| started.elapsed().as_millis() as u64);
    let who = seen.0.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let who = match &who {
        Some((true, id)) => Who::Bot(id),
        Some((false, id)) => Who::Person(id),
        None => Who::Nobody,
    };
    state.usage.record(now(), "rest", &name, who, response.status(), millis);
    response
}

#[derive(Deserialize)]
struct Window {
    days: Option<i64>,
}

/// A day number as `YYYY-MM-DD`.
fn date(day: i64) -> String {
    let z = day + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}

#[derive(Default)]
struct Totals {
    calls: i64,
    refused: i64,
    errors: i64,
    millis: i64,
    timed: i64,
    slow: i64,
    reasons: HashMap<String, i64>,
    /// U16: the response-time buckets that go with these calls.
    hist: [i64; BUCKETS],
}

impl Totals {
    fn add(&mut self, row: &StoredHour) {
        self.calls += row.calls;
        match row.outcome.as_str() {
            "refused" => {
                self.refused += row.calls;
                *self.reasons.entry(row.reason.clone()).or_default() += row.calls;
            }
            "error" => self.errors += row.calls,
            _ => {}
        }
        self.millis += row.millis;
        self.timed += row.timed;
        self.slow += row.slow;
    }

    fn add_hist(&mut self, hist: &[i64; BUCKETS]) {
        for (slot, n) in self.hist.iter_mut().zip(hist) {
            *slot += n;
        }
    }

    fn json(&self) -> Value {
        // The reason with the most refusals, and the first alphabetically when they tie.
        let top = self
            .reasons
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map(|(reason, _)| reason.as_str());
        json!({
            "calls": self.calls,
            "refused": self.refused,
            "errors": self.errors,
            "avg_ms": (self.timed > 0).then(|| self.millis / self.timed),
            "slow": self.slow,
            "p50_ms": percentile(&self.hist, 0.5),
            "p90_ms": percentile(&self.hist, 0.9),
            "top_refusal": top,
        })
    }
}

/// U9, U10: the totals for the people who run the server.
async fn server_usage(
    State(state): State<AppState>,
    caller: Caller,
    Query(window): Query<Window>,
) -> ApiResult<Json<Value>> {
    if caller.is_agent() || !god_mode::may_use(&state, &caller.person_id) {
        return Err(ApiError::not_found());
    }
    let days = match window.days {
        None | Some(7) => 7,
        Some(30) => 30,
        Some(_) => return Err(ApiError::bad_request("days is 7 or 30")),
    };
    state.usage.flush(&state);
    let today = now().div_euclid(86_400);
    let since = today - days + 1;
    let rows = state.db.usage_hours(since)?;
    let active = state.db.usage_days(since)?;
    let since_hour = since * 24;
    let latency = state.db.usage_latency(since_hour)?;
    let line_rows = state.db.usage_lines(since_hour)?;
    let hour_calls = state.db.usage_hour_calls(since_hour)?;
    let active_hours = state.db.usage_active_hours(since_hour)?;
    let mut tools: BTreeMap<(String, String), Totals> = BTreeMap::new();
    let mut by_day: BTreeMap<i64, Totals> = BTreeMap::new();
    let mut versions: BTreeMap<String, Totals> = BTreeMap::new();
    for row in &rows {
        tools.entry((row.surface.clone(), row.name.clone())).or_default().add(row);
        by_day.entry(row.day).or_default().add(row);
        versions.entry(row.version.clone()).or_default().add(row);
    }
    // U15, U16: every hour of the window, one entry for each, with nothing in the hours with no calls.
    let this_hour = now().div_euclid(3600);
    let first_hour = since_hour;
    let mut by_hour: BTreeMap<i64, Totals> = BTreeMap::new();
    for (hour, outcome, calls) in &hour_calls {
        if *hour >= first_hour {
            let totals = by_hour.entry(*hour).or_default();
            totals.calls += calls;
            match outcome.as_str() {
                "refused" => totals.refused += calls,
                "error" => totals.errors += calls,
                _ => {}
            }
        }
    }
    for row in &latency {
        tools.entry((row.surface.clone(), row.name.clone())).or_default().add_hist(&row.hist);
        by_day.entry(row.hour.div_euclid(24)).or_default().add_hist(&row.hist);
        versions.entry(row.version.clone()).or_default().add_hist(&row.hist);
        if row.hour >= first_hour {
            by_hour.entry(row.hour).or_default().add_hist(&row.hist);
        }
    }
    let active_hours: HashMap<i64, (i64, i64)> =
        active_hours.into_iter().map(|(h, p, b)| (h, (p, b))).collect();
    // U17: events, long-polls and webhooks, by day and in all.
    let mut line_days: BTreeMap<i64, [i64; LINES.len()]> = BTreeMap::new();
    let mut line_all = [0i64; LINES.len()];
    for (hour, line, count) in &line_rows {
        if let Some(index) = LINES.iter().position(|known| known == line) {
            line_days.entry(hour.div_euclid(24)).or_default()[index] += count;
            line_all[index] += count;
        }
    }
    let lines_json = |counts: &[i64; LINES.len()]| {
        let mut object = serde_json::Map::new();
        for (name, count) in LINES.iter().zip(counts) {
            object.insert((*name).to_string(), json!(count));
        }
        Value::Object(object)
    };
    let hour_list: Vec<Value> = (first_hour..=this_hour)
        .map(|hour| {
            let totals = by_hour.remove(&hour).unwrap_or_default();
            let (people, bots) = active_hours.get(&hour).copied().unwrap_or((0, 0));
            let mut value = totals.json();
            value["hour"] =
                json!(format!("{}T{:02}:00Z", date(hour.div_euclid(24)), hour.rem_euclid(24)));
            value["active_people"] = json!(people);
            value["active_bots"] = json!(bots);
            value
        })
        .collect();
    let active: HashMap<i64, (i64, i64)> =
        active.into_iter().map(|(d, p, b)| (d, (p, b))).collect();
    let mut tool_list: Vec<Value> = tools
        .iter()
        .map(|((surface, name), totals)| {
            let mut value = totals.json();
            value["surface"] = json!(surface);
            value["name"] = json!(name);
            value
        })
        .collect();
    tool_list.sort_by(|a, b| b["calls"].as_i64().cmp(&a["calls"].as_i64()));
    let day_list: Vec<Value> = (since..=today)
        .map(|day| {
            let totals = by_day.remove(&day).unwrap_or_default();
            let (people, bots) = active.get(&day).copied().unwrap_or((0, 0));
            let mut value = totals.json();
            value["day"] = json!(date(day));
            value["active_people"] = json!(people);
            value["active_bots"] = json!(bots);
            value["lines"] = lines_json(&line_days.remove(&day).unwrap_or_default());
            value
        })
        .collect();
    let version_list: Vec<Value> = versions
        .iter()
        .map(|(version, totals)| {
            let mut value = totals.json();
            value["version"] = json!(version);
            value
        })
        .collect();
    Ok(Json(json!({
        "days": days,
        "running": build(),
        "by_tool": tool_list,
        "by_day": day_list,
        "by_version": version_list,
        "by_hour": hour_list,
        "lines": lines_json(&line_all),
    })))
}

/// U11: a bot's own numbers, for its owner and for nobody else.
async fn agent_usage(
    State(state): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    if caller.is_agent() {
        return Err(ApiError::not_found());
    }
    let agent = state.db.agent(&id, now())?.ok_or_else(ApiError::not_found)?;
    if agent.owner_id != caller.person_id {
        return Err(ApiError::not_found());
    }
    state.usage.flush(&state);
    let today = now().div_euclid(86_400);
    let since = today - KEEP_DAYS + 1;
    let rows = state.db.usage_bot_days(&id, since)?;
    let events: BTreeMap<i64, i64> = state.db.usage_bot_events(&id, since)?.into_iter().collect();
    let chars: BTreeMap<i64, i64> = state.db.usage_bot_chars(&id, since)?.into_iter().collect();
    let costs: BTreeMap<i64, ReportedCost> =
        state.db.usage_bot_cost(&id, since)?.into_iter().collect();
    let mut per_day: BTreeMap<i64, [i64; 3]> = BTreeMap::new();
    let mut reasons: BTreeMap<String, i64> = BTreeMap::new();
    for (day, outcome, reason, calls) in rows {
        let slot = per_day.entry(day).or_default();
        match outcome.as_str() {
            "refused" => {
                slot[1] += calls;
                *reasons.entry(reason).or_default() += calls;
            }
            "error" => slot[2] += calls,
            _ => slot[0] += calls,
        }
    }
    let days: Vec<Value> = (since..=today)
        .map(|day| {
            let [ok, refused, errors] = per_day.get(&day).copied().unwrap_or_default();
            let events = events.get(&day).copied().unwrap_or_default();
            // U24: what the bot said it cost that day, or null when it said nothing.
            let cost = costs.get(&day);
            // U26: what the bot was sent that day, in characters of tool results.
            let sent = chars.get(&day).copied().unwrap_or_default();
            json!({"day": date(day), "calls": ok + refused + errors, "refused": refused, "errors": errors, "events_delivered": events,
                "reply_chars": sent,
                "reported_cost_cents": cost.map(|c| c.cost_cents),
                "reported_input_tokens": cost.and_then(|c| c.input_tokens),
                "reported_output_tokens": cost.and_then(|c| c.output_tokens)})
        })
        .collect();
    let last = agent.last_active_at.max(agent.last_seen_at);
    // U24: each day is at most MAX_COST_CENTS, so 30 of them can't overflow.
    let total = (!costs.is_empty()).then(|| costs.values().map(|c| c.cost_cents).sum::<i64>());
    Ok(Json(json!({
        "days": days,
        "refusals": reasons,
        "last_activity_at": last,
        "reported_cost_cents_30d": total,
        "cost_note": COST_NOTE,
    })))
}

/// U23: what a bot says its own model cost and tokens were on one day.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReportedCost {
    pub cost_cents: i64,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
}

/// U23: the most a bot may report for one day, in cents ($1,000), and in tokens of each kind.
pub const MAX_COST_CENTS: i64 = 100_000;
pub const MAX_TOKENS: i64 = 10_000_000_000;
/// U24: said with every cost, since the server never sees the bot's model or its bill.
pub const COST_NOTE: &str =
    "Cost and tokens are reported by the bot itself. The server can't check them.";

/// U23: `report_usage`. A bot reports its running cost and tokens for today, or for yesterday, in
/// UTC; the latest report for the day replaces the earlier ones. Only a bot may, and only for itself.
pub fn report(state: &AppState, caller: &Caller, args: &Value) -> ApiResult<Value> {
    let agent_id = caller
        .agent_id
        .as_deref()
        .ok_or_else(|| ApiError::forbidden("only a bot reports its usage"))?;
    let whole = |key: &str, most: i64| -> ApiResult<Option<i64>> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => match value.as_i64() {
                Some(n) if (0..=most).contains(&n) => Ok(Some(n)),
                _ => {
                    Err(ApiError::bad_request(format!("{key} is a whole number from 0 to {most}")))
                }
            },
        }
    };
    let cost_cents = whole("cost_cents", MAX_COST_CENTS)?
        .ok_or_else(|| ApiError::bad_request("cost_cents is required"))?;
    let cost = ReportedCost {
        cost_cents,
        input_tokens: whole("input_tokens", MAX_TOKENS)?,
        output_tokens: whole("output_tokens", MAX_TOKENS)?,
    };
    let at = now();
    let today = at.div_euclid(86_400);
    let day = match args.get("day") {
        None | Some(Value::Null) => today,
        Some(value) => match value.as_str() {
            Some(text) if text == date(today) => today,
            Some(text) if text == date(today - 1) => today - 1,
            _ => {
                return Err(ApiError::bad_request(
                    "day is today or yesterday in UTC, as YYYY-MM-DD",
                ))
            }
        },
    };
    state.db.set_usage_bot_cost(agent_id, day, &cost, at)?;
    Ok(json!({ "ok": true, "day": date(day), "cost_cents": cost.cost_cents, "note": COST_NOTE }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_come_out_right() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(19_723), "2024-01-01");
        assert_eq!(date(19_782), "2024-02-29");
    }

    #[test]
    fn reasons_come_from_the_status() {
        assert_eq!(outcome_of(StatusCode::OK).0, Outcome::Ok);
        assert_eq!(outcome_of(StatusCode::FORBIDDEN), (Outcome::Refused, "forbidden"));
        assert_eq!(outcome_of(StatusCode::IM_A_TEAPOT), (Outcome::Refused, "other"));
        assert_eq!(outcome_of(StatusCode::BAD_GATEWAY).0, Outcome::Error);
    }
}
