// The bot tools are one long `json!`, and that macro goes a level deeper for each thing in the list.
#![recursion_limit = "512"]

pub mod account;
pub mod agent_door;
pub mod api;
pub mod auth;
pub mod blocks;
pub mod boards;
pub mod bonds;
pub mod bot_profile;
pub mod bot_setup;
pub mod bot_trust;
pub mod brand;
pub mod bridge;
pub mod cadence;
pub mod camera;
pub mod city;
pub mod clock;
pub mod closeness;
pub mod closing;
pub mod db;
pub mod discord;
pub mod docs;
pub mod doorbell;
pub mod economy;
pub mod elections;
pub mod emotes;
pub mod error;
pub mod events;
pub mod friends;
pub mod github_display;
pub mod god_mode;
pub mod house_editing;
pub mod households;
pub mod how_to_play;
pub mod ical;
pub mod items;
pub mod jobs;
pub mod keyless;
pub mod lific;
pub mod looks;
pub mod mcp;
pub mod meetups;
pub mod memories;
pub mod needs_you;
pub mod notify;
pub mod oauth;
pub mod ops;
pub mod outside;
pub mod passkey;
pub mod pins;
pub mod projects;
pub mod public_mcp;
pub mod publishing;
pub mod pulse;
pub mod push;
pub mod recycling;
pub mod referrals;
pub mod reports;
pub mod research_labs;
pub mod research_work;
pub mod review;
pub mod room_mentions;
pub mod routines;
pub mod rules;
pub mod sign_in;
pub mod spaces;
pub mod travel;
pub mod trust;
pub mod tutorial;
pub mod typing;
pub mod usage;
pub mod votes;
pub mod web;
pub mod world;
pub mod worlds;

use std::{
    collections::HashMap,
    ops::Deref,
    sync::{Arc, Mutex},
};

use axum::{extract::DefaultBodyLimit, middleware, routing::post, Router};
use tokio::sync::{broadcast, watch};

use db::Db;

/// Bots are chatty, not bulky: nothing an agent sends here should come close to this.
const MAX_BODY_BYTES: usize = 64 * 1024;

pub fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[derive(Clone, Debug)]
pub struct HubEvent {
    pub hangout_id: String,
    pub kind: &'static str,
    pub data: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// The address people and bots reach this server on, with no trailing slash.
    pub public_url: String,
    /// When set, starting a new hangout requires this code. Joining by invite never does.
    pub create_code: Option<String>,
    /// Lets notifications reach local addresses. For tests only: it has no environment setting,
    /// so a deployed server can't turn it on.
    pub allow_private_webhooks: bool,
    /// The person who looks after The Commons and can remove a player from it (W18): their id,
    /// from `AGENTHANGOUT_KEEPER`. Nobody is, when it isn't set.
    pub keeper_id: Option<String>,
    /// The people who have the God mode switch (W75): their ids, from `AGENTHANGOUT_GODS`.
    pub god_ids: Vec<String>,
    /// A107-A112: writing what others read takes the community rules accepted. Always on for a
    /// deployed server (main.rs). Off only in tests that aren't about the rules, which build their
    /// own Config.
    pub require_rules: bool,
}

/// The apps registered elsewhere that prove someone's account there. A server has none by default.
#[derive(Default)]
pub struct Apps {
    pub discord: Option<discord::App>,
    pub github: Option<sign_in::GitHubApp>,
}

pub struct Shared {
    pub db: Db,
    pub config: Config,
    pub hub: broadcast::Sender<HubEvent>,
    /// Tells a bot's open `events/stream`, or a waiting `events_since`, that its log grew.
    pub bot_events: broadcast::Sender<events::Ping>,
    /// E44: set when the server is asked to stop, so a waiting poll answers instead of being cut off.
    pub shutting_down: watch::Sender<bool>,
    pub(crate) oauth_events: events::oauth::Runtime,
    /// Passkeys, built from `config.public_url`. `None` when that address is an IP number, which
    /// a passkey can't belong to, so the passkey routes refuse.
    pub webauthn: Option<webauthn_rs::Webauthn>,
    /// Passkey ceremonies that are waiting on a browser, and the cap on new sign-ups.
    pub ceremonies: passkey::Ceremonies,
    /// The Discord app that proves a linked account. `None` turns linking off (A25).
    pub discord: Option<discord::App>,
    /// The GitHub app that proves a linked account. `None` turns GitHub off (A58).
    pub github: Option<sign_in::GitHubApp>,
    /// The public worlds' memory: land, built views, limits, and who is passing through.
    pub worlds: worlds::Runtime,
    /// The limits on plan links and on cities started without a code.
    pub referrals: referrals::Limits,
    /// The limit on asking when people are free.
    pub closeness: closeness::Limits,
    /// What the server did, counted for the people who run it and for a bot's owner (U1 to U12).
    pub usage: usage::Usage,
    /// Who has a floor open. In memory only, and it never leaves the floor (S41 to S46).
    pub presence: spaces::Presence,
    pub typing: typing::Runtime,
    pub emotes: emotes::Runtime,
    /// A111: bots refused for their owner's community rules, for Needs you.
    pub rules_refused: rules::Refused,
    pub camera_slots: Arc<tokio::sync::Semaphore>,
    pub push: std::sync::OnceLock<Arc<dyn push::Transport>>,
    pub github_refresh_slots: Arc<tokio::sync::Semaphore>,
    pub push_slots: Arc<tokio::sync::Semaphore>,
    pub push_deliveries: Arc<tokio::sync::Semaphore>,
    /// I124: where a Lific board may point, from `AGENTHANGOUT_LIFIC_ORIGINS` and `_LOCAL`.
    pub lific: lific::Shared,
    /// KA4, KA9, KA11, KA16: serialize keyless checks and writes in this single-process server.
    pub(crate) keyless_actions: Mutex<()>,
    write_windows: Mutex<HashMap<String, Window>>,
    write_clock: Arc<dyn Fn() -> i64 + Send + Sync>,
}

/// One key's writes: the minute being counted, its count, and the count of the minute before.
#[derive(Clone, Copy, Default)]
struct Window {
    minute: i64,
    count: u32,
    previous: u32,
}

impl Window {
    /// Moves on to `minute`, keeping the last count only if it was for the minute just before.
    fn roll(&mut self, minute: i64) {
        if self.minute != minute {
            self.previous = if self.minute == minute - 1 { self.count } else { 0 };
            self.minute = minute;
            self.count = 0;
        }
    }

    /// The writes in the last 60 seconds, estimated by weighing the minute before by how much of
    /// it still falls inside them, rounded up.
    fn last_sixty_seconds(&self, second: i64) -> u64 {
        let weight = (60 - second.clamp(0, 59)) as u64;
        (u64::from(self.previous) * weight).div_ceil(60) + u64::from(self.count)
    }
}

#[derive(Clone)]
pub struct AppState(Arc<Shared>);

impl Deref for AppState {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl AppState {
    /// E44: the server is stopping. Waiting polls answer now.
    pub fn begin_shutdown(&self) {
        self.shutting_down.send_replace(true);
    }

    pub fn new(db: Db, config: Config) -> Self {
        Self::with_apps(db, config, Apps::default())
    }

    pub fn with_apps(db: Db, config: Config, apps: Apps) -> Self {
        Self::with_apps_and_write_clock(db, config, apps, now)
    }

    /// Builds an isolated state with a controlled Unix-seconds clock for write-limit tests.
    /// Only write windows use this clock; authentication, events and other time stay real.
    /// Normal constructors always use the wall clock.
    #[doc(hidden)]
    pub fn with_write_clock(
        db: Db,
        config: Config,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self::with_apps_and_write_clock(db, config, Apps::default(), clock)
    }

    fn with_apps_and_write_clock(
        db: Db,
        config: Config,
        apps: Apps,
        clock: impl Fn() -> i64 + Send + Sync + 'static,
    ) -> Self {
        let (hub, _) = broadcast::channel(256);
        let (bot_events, _) = broadcast::channel(1024);
        let (shutting_down, _) = watch::channel(false);
        let webauthn = passkey::webauthn_for(&config.public_url);
        // A112: the grace for a rules version runs from the first start with it, at the latest.
        if let Err(error) = db.note_rules_version(rules::version(), now()) {
            tracing::error!(%error, "couldn't note the rules version");
        }
        Self(Arc::new(Shared {
            db,
            config,
            hub,
            bot_events,
            shutting_down,
            oauth_events: events::oauth::Runtime::default(),
            webauthn,
            ceremonies: passkey::Ceremonies::default(),
            discord: apps.discord,
            github: apps.github,
            worlds: worlds::Runtime::new(),
            referrals: referrals::Limits::new(),
            closeness: closeness::Limits::new(),
            usage: usage::Usage::new(mcp::tool_names()),
            presence: spaces::Presence::default(),
            typing: typing::Runtime::default(),
            emotes: emotes::Runtime::default(),
            rules_refused: rules::Refused::default(),
            camera_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            push: std::sync::OnceLock::new(),
            github_refresh_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            push_slots: Arc::new(tokio::sync::Semaphore::new(64)),
            push_deliveries: Arc::new(tokio::sync::Semaphore::new(8)),
            lific: lific::Shared::default(),
            keyless_actions: Mutex::new(()),
            write_windows: Mutex::new(HashMap::new()),
            write_clock: Arc::new(clock),
        }))
    }

    pub fn is_https(&self) -> bool {
        self.config.public_url.starts_with("https://")
    }

    /// Counts a write against this bot's one-minute window. Two bots replying to each other
    /// forever is the runaway to expect here, and this caps what it can cost.
    pub fn allow_agent_write(&self, agent_id: &str) -> bool {
        self.allow_writes(agent_id, ops::MAX_AGENT_WRITES_PER_MINUTE)
    }

    /// Counts one more against this key's one-minute window, and says if it's within `limit`.
    pub fn allow_writes(&self, key: &str, limit: u32) -> bool {
        self.count_write(key, |window, _| window.count <= limit)
    }

    /// Like `allow_writes`, but counts over the last 60 seconds rather than the clock's minute,
    /// so a burst either side of a minute boundary can't reach twice `limit`.
    pub fn allow_writes_sliding(&self, key: &str, limit: u32) -> bool {
        self.count_write(key, |window, second| {
            window.last_sixty_seconds(second) <= u64::from(limit)
        })
    }

    fn count_write(&self, key: &str, within: impl Fn(&Window, i64) -> bool) -> bool {
        let now = (self.write_clock)();
        let minute = now / 60;
        let mut windows =
            self.write_windows.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        windows.retain(|_, window| window.minute >= minute - 1);
        let window = windows.entry(key.to_string()).or_default();
        window.roll(minute);
        window.count += 1;
        within(window, now % 60)
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .merge(api::routes())
        .merge(bridge::routes())
        .merge(keyless::routes())
        .merge(camera::routes())
        .merge(usage::routes())
        .merge(oauth::routes())
        .merge(web::routes())
        .route("/mcp", post(mcp::handle))
        .route("/public/mcp", post(public_mcp::handle))
        .fallback(web::not_found)
        // A109: every /api write route takes the community rules accepted, but the ungated ones.
        .layer(middleware::from_fn_with_state(state.clone(), rules::gate_writes))
        // RE11: and every /api request meets the operators' holds, beside the rules gate.
        .layer(middleware::from_fn_with_state(state.clone(), review::gate))
        // Inside everything but the rules gate, so it sees what was answered and the route (U1).
        .layer(middleware::from_fn_with_state(state.clone(), usage::observe))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), web::security_headers))
        // Outside everything that can refuse a request, so it reads what they leave (W107).
        .layer(middleware::from_fn(web::read_what_was_left))
        // Outermost, so it sees every request. It only watches (W38).
        .layer(middleware::from_fn_with_state(state.clone(), worlds::observe))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::Window;

    #[test]
    fn a_sliding_window_carries_the_last_minute_into_this_one() {
        let mut window = Window::default();
        window.roll(100);
        window.count = 30;
        window.roll(101);
        assert_eq!((window.previous, window.count), (30, 0));
        // At the boundary all of the last minute still counts, halfway through half of it does.
        assert_eq!(window.last_sixty_seconds(0), 30);
        assert_eq!(window.last_sixty_seconds(30), 15);
        assert_eq!(window.last_sixty_seconds(59), 1);
        // 30 just before the boundary and one just after is over a limit of 30.
        window.count = 1;
        assert_eq!(window.last_sixty_seconds(0), 31);
        // A gap of more than a minute forgets it.
        window.roll(103);
        assert_eq!((window.previous, window.count), (0, 0));
    }
}
