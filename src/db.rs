//! SQLite storage. Queries are short and run on one connection, so a plain mutex is enough here.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::github_display::{CachedDisplay, GitHubDisplay};
use crate::world::{Change, Kind, Pos, Tile, Tiles};

/// An agent that wrote something this recently sits at the table.
pub const ACTIVE_SECS: i64 = 10 * 60;
/// An agent that called the server at all this recently wanders the room. Older than this, it naps.
pub const IDLE_SECS: i64 = 24 * 60 * 60;
/// A doorbell checks every 15 seconds, so one that's been quiet this long has stopped.
pub const LISTENING_SECS: i64 = 60;
/// I84: a bot that called the server this recently will likely see a mention soon.
pub const RESPONSIVE_SECS: i64 = 10 * 60;
/// S61: how long a bot stays away from home with nothing to do.
pub const BOT_STAY_SECS: i64 = 30 * 60;

mod blocks;
pub use blocks::Added as TieAdded;
mod bot_profile;
mod bot_setup;
mod contributions;
pub use contributions::{
    CONTRIBUTIONS_A_DAY, CONTRIBUTION_BUDGET_A_WEEK, CONTRIBUTION_HC, CONTRIBUTION_KINDS,
    PAIR_CONTRIBUTIONS_A_WEEK,
};
mod bridge;
pub use bridge::BRIDGE_KEY_SECS;
mod keyless;
pub use bot_setup::{BotSetupDraft, SetupFields, SetupGrant};
pub use keyless::KeylessCard;
mod bot_trust;
pub use cadence::{Cadence, Shape};
pub use pulses::PushBot;
mod cadence;
mod camera;
mod github_display;
mod oauth;
pub(crate) mod oauth_events;
mod pulses;
mod recycling;
mod reports;
pub use reports::{Filed, MyReport, NewReport};
mod review;
pub use review::{
    Closing, Hold as ReviewHold, HoldRow, Holds, ModerationNotice, QueueReport, Released, Tell,
};
mod research_labs;
mod research_work;
mod room_mentions;
mod usage;
mod votes;
pub use votes::{Ballot, NewVote, Vote};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS people (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL,
    key_hash    TEXT NOT NULL UNIQUE,
    feed_token  TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS hangouts (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    invite_code  TEXT NOT NULL UNIQUE,
    created_by   TEXT NOT NULL REFERENCES people(id),
    created_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS members (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    joined_at   INTEGER NOT NULL,
    PRIMARY KEY (hangout_id, person_id)
);
CREATE TABLE IF NOT EXISTS agents (
    id              TEXT PRIMARY KEY,
    owner_id        TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    token_hash      TEXT NOT NULL UNIQUE,
    status          TEXT,
    last_active_at  INTEGER,
    last_seen_at    INTEGER,
    created_at      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_activity (
    agent_id  TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    kind      TEXT NOT NULL,
    via       TEXT NOT NULL,
    until_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS agent_doorbell (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    paused      INTEGER NOT NULL DEFAULT 0,
    checked_at  INTEGER
);
CREATE TABLE IF NOT EXISTS room_agents (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    PRIMARY KEY (hangout_id, agent_id)
);
CREATE TABLE IF NOT EXISTS messages (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    person_id   TEXT REFERENCES people(id) ON DELETE SET NULL,
    agent_id    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    speaker     TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('chat', 'note', 'system')),
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_by_room ON messages (hangout_id, id);
-- I82: the chat line a message answers, when its writer said so.
CREATE TABLE IF NOT EXISTS message_replies (
    message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    reply_to    INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS events (
    id           TEXT PRIMARY KEY,
    hangout_id   TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    location     TEXT NOT NULL DEFAULT '',
    notes        TEXT NOT NULL DEFAULT '',
    starts_at    INTEGER NOT NULL,
    ends_at      INTEGER NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('proposed', 'confirmed', 'cancelled')),
    min_yes      INTEGER NOT NULL,
    sequence     INTEGER NOT NULL DEFAULT 0,
    proposed_by  TEXT NOT NULL REFERENCES people(id),
    proposer     TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS events_by_room ON events (hangout_id, starts_at);
CREATE TABLE IF NOT EXISTS check_ins (
    id               TEXT PRIMARY KEY,
    hangout_id       TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    asked_by         TEXT NOT NULL REFERENCES people(id),
    asker            TEXT NOT NULL,
    agent_id         TEXT REFERENCES agents(id) ON DELETE SET NULL,
    about            TEXT NOT NULL,
    to_person        TEXT REFERENCES people(id),
    needs_human      INTEGER NOT NULL,
    status           TEXT NOT NULL CHECK (status IN ('waiting', 'go', 'hold')),
    answered_by      TEXT,
    answered_by_bot  INTEGER NOT NULL DEFAULT 0,
    note             TEXT NOT NULL DEFAULT '',
    expires_at       INTEGER NOT NULL,
    created_at       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS check_ins_by_room ON check_ins (hangout_id, created_at);
CREATE TABLE IF NOT EXISTS notify_targets (
    owner_id  TEXT PRIMARY KEY,
    url       TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS rsvps (
    event_id    TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    response    TEXT NOT NULL CHECK (response IN ('yes', 'maybe', 'no')),
    via_agent   TEXT,
    note        TEXT NOT NULL DEFAULT '',
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (event_id, person_id)
);
CREATE TABLE IF NOT EXISTS cities (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    invite_code  TEXT NOT NULL UNIQUE,
    square_id    TEXT NOT NULL REFERENCES hangouts(id),
    created_by   TEXT NOT NULL REFERENCES people(id),
    created_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS city_members (
    city_id    TEXT NOT NULL REFERENCES cities(id) ON DELETE CASCADE,
    person_id  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    joined_at  INTEGER NOT NULL,
    PRIMARY KEY (city_id, person_id)
);
CREATE TABLE IF NOT EXISTS houses (
    id           TEXT PRIMARY KEY,
    city_id      TEXT NOT NULL REFERENCES cities(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    invite_code  TEXT NOT NULL UNIQUE,
    living_room  TEXT NOT NULL REFERENCES hangouts(id),
    porch        TEXT NOT NULL REFERENCES hangouts(id),
    created_by   TEXT NOT NULL REFERENCES people(id),
    created_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS residents (
    house_id   TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    person_id  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    joined_at  INTEGER NOT NULL,
    PRIMARY KEY (house_id, person_id)
);
CREATE TABLE IF NOT EXISTS bot_homes (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    house_id    TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    stays_home  INTEGER NOT NULL DEFAULT 0,
    is_house    INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS trips (
    id           TEXT PRIMARY KEY,
    agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    house_id     TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('call', 'visit', 'stay')),
    goal         TEXT NOT NULL DEFAULT '',
    minutes      INTEGER NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('asking', 'here', 'declined', 'ended')),
    asked_by     TEXT NOT NULL,
    owner_ok_by  TEXT,
    host_ok_by   TEXT,
    host_ok_id   TEXT,
    pass_id      TEXT,
    arrived_at   INTEGER,
    ends_at      INTEGER,
    ended_by     TEXT,
    expires_at   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS trips_by_bot ON trips (agent_id, status);
CREATE INDEX IF NOT EXISTS trips_by_house ON trips (house_id, status);
CREATE TABLE IF NOT EXISTS passes (
    id           TEXT PRIMARY KEY,
    house_id     TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    max_minutes  INTEGER NOT NULL,
    granted_by   TEXT NOT NULL,
    accepted_by  TEXT,
    created_at   INTEGER NOT NULL,
    UNIQUE (house_id, agent_id)
);
CREATE TABLE IF NOT EXISTS scenes (
    id          TEXT PRIMARY KEY,
    house_id    TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE (house_id, name)
);
CREATE TABLE IF NOT EXISTS scene_requests (
    id           TEXT PRIMARY KEY,
    scene_id     TEXT NOT NULL REFERENCES scenes(id) ON DELETE CASCADE,
    house_id     TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    asked_by     TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('waiting', 'approved', 'declined', 'done', 'failed')),
    answered_by  TEXT,
    expires_at   INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS scene_requests_by_house ON scene_requests (house_id, status);
CREATE TABLE IF NOT EXISTS house_status (
    house_id    TEXT PRIMARY KEY REFERENCES houses(id) ON DELETE CASCADE,
    state       TEXT NOT NULL CHECK (state IN ('ok', 'attention')),
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS owner_questions (
    id           TEXT PRIMARY KEY,
    hangout_id   TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    owner_id     TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    asker        TEXT NOT NULL,
    question     TEXT NOT NULL,
    answer       TEXT,
    created_at   INTEGER NOT NULL,
    answered_at  INTEGER
);
CREATE INDEX IF NOT EXISTS owner_questions_by_owner ON owner_questions (owner_id, hangout_id);
CREATE INDEX IF NOT EXISTS owner_questions_by_bot ON owner_questions (agent_id, created_at);
CREATE TABLE IF NOT EXISTS passkey_users (
    person_id    TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    user_handle  TEXT NOT NULL UNIQUE
);
CREATE TABLE IF NOT EXISTS passkeys (
    id             TEXT PRIMARY KEY,
    person_id      TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    credential_id  TEXT NOT NULL UNIQUE,
    passkey        TEXT NOT NULL,
    label          TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    last_used_at   INTEGER
);
CREATE INDEX IF NOT EXISTS passkeys_by_person ON passkeys (person_id);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash  TEXT PRIMARY KEY,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    expires_at  INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_by_person ON sessions (person_id);
CREATE TABLE IF NOT EXISTS entry_rules (
    place_id         TEXT PRIMARY KEY,
    min_member_days  INTEGER NOT NULL DEFAULT 0,
    min_gatherings   INTEGER NOT NULL DEFAULT 0,
    min_good_trips   INTEGER NOT NULL DEFAULT 0,
    min_vouches      INTEGER NOT NULL DEFAULT 0,
    rules_text       TEXT NOT NULL DEFAULT '',
    updated_at       INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS vouches (
    city_id     TEXT NOT NULL REFERENCES cities(id) ON DELETE CASCADE,
    for_person  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    by_person   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (city_id, for_person, by_person)
);
CREATE TABLE IF NOT EXISTS city_gate (
    city_id     TEXT NOT NULL REFERENCES cities(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (city_id, person_id)
);
CREATE TABLE IF NOT EXISTS sent_home (
    trip_id     TEXT PRIMARY KEY REFERENCES trips(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS worlds (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('public', 'own')),
    seed        INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    owner_id    TEXT REFERENCES people(id) ON DELETE SET NULL,
    open        INTEGER NOT NULL DEFAULT 1,
    invite_code TEXT UNIQUE,
    step        INTEGER NOT NULL DEFAULT 0,
    active_at   INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS worlds_one_each ON worlds (owner_id) WHERE owner_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS world_tiles (
    world_id   TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    x          INTEGER NOT NULL,
    y          INTEGER NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('road', 'homes', 'shops', 'workshops', 'park', 'cleared')),
    level      INTEGER NOT NULL DEFAULT 0,
    player_id  INTEGER,
    built_at   INTEGER NOT NULL,
    PRIMARY KEY (world_id, x, y)
);
-- A player's tiles are always asked for in one world, so the world comes first. The older index on
-- the player alone is not needed beside it.
DROP INDEX IF EXISTS world_tiles_by_player;
CREATE INDEX IF NOT EXISTS world_tiles_by_world_player ON world_tiles (world_id, player_id);
CREATE TABLE IF NOT EXISTS players (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    name_seed    INTEGER NOT NULL,
    token_hash   TEXT UNIQUE,
    claim_hash   TEXT UNIQUE,
    person_id    TEXT REFERENCES people(id) ON DELETE SET NULL,
    agent_id     TEXT REFERENCES agents(id) ON DELETE SET NULL,
    removed      INTEGER NOT NULL DEFAULT 0,
    moves_day    INTEGER NOT NULL DEFAULT 0,
    moves_used   INTEGER NOT NULL DEFAULT 0,
    seen_at      INTEGER NOT NULL,
    created_at   INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS players_one_per_agent ON players (agent_id) WHERE agent_id IS NOT NULL;
-- A bot's player has no token while its bot's token has been replaced, so only a player with no bot
-- is a person's own. One that was removed doesn't count, or a bot that was deleted could not be.
-- `Db::open` drops this index first when it was made without `removed = 0`.
DROP INDEX IF EXISTS players_one_per_person;
CREATE UNIQUE INDEX IF NOT EXISTS players_one_own_per_person
    ON players (person_id) WHERE person_id IS NOT NULL AND token_hash IS NULL AND agent_id IS NULL AND removed = 0;
CREATE INDEX IF NOT EXISTS players_by_person ON players (person_id);
-- Deleting a bot ends its player first. The foreign key would only clear agent_id, which leaves a
-- player with a token and an owner: a claimed one. The player lets go of the person too, so it can't
-- be taken for the person's own, which is the one with no token and no bot.
DROP TRIGGER IF EXISTS players_end_with_their_bot;
CREATE TRIGGER players_end_with_their_bot BEFORE DELETE ON agents
BEGIN
    UPDATE players SET removed = 1, person_id = NULL WHERE agent_id = OLD.id;
END;
-- A player's name comes from a seed that starts random. Older players started with their own
-- number, which gave a reroll away.
UPDATE players SET name_seed = abs(random() % 1099511627775) + 1 WHERE name_seed = id;
CREATE TABLE IF NOT EXISTS world_players (
    world_id   TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    player_id  INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    x          INTEGER NOT NULL,
    y          INTEGER NOT NULL,
    emote      TEXT,
    seen_at    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (world_id, player_id)
);
CREATE INDEX IF NOT EXISTS world_players_recent ON world_players (world_id, seen_at);
CREATE TABLE IF NOT EXISTS world_members (
    world_id   TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    player_id  INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    banned     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (world_id, player_id)
);
CREATE TABLE IF NOT EXISTS crawler_sightings (
    kind     TEXT NOT NULL,
    day      INTEGER NOT NULL,
    visits   INTEGER NOT NULL DEFAULT 0,
    seen_at  INTEGER NOT NULL,
    PRIMARY KEY (kind, day)
);
CREATE TABLE IF NOT EXISTS plan_invites (
    event_id    TEXT PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
    code        TEXT NOT NULL UNIQUE,
    created_by  TEXT NOT NULL REFERENCES people(id),
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS plan_invitees (
    event_id    TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    joined_at   INTEGER NOT NULL,
    PRIMARY KEY (event_id, person_id)
);
CREATE INDEX IF NOT EXISTS plan_invitees_by_person ON plan_invitees (person_id);
CREATE TABLE IF NOT EXISTS free_sharing (
    person_id   TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    level       TEXT NOT NULL CHECK (level IN ('house', 'city')),
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS free_windows (
    person_id  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    starts_at  INTEGER NOT NULL,
    ends_at    INTEGER NOT NULL,
    set_at     INTEGER NOT NULL,
    PRIMARY KEY (person_id, starts_at)
);
CREATE TABLE IF NOT EXISTS plan_items (
    id           TEXT PRIMARY KEY,
    event_id     TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    body         TEXT NOT NULL,
    added_by     TEXT NOT NULL REFERENCES people(id),
    adder        TEXT NOT NULL,
    claimed_by   TEXT REFERENCES people(id),
    claimed_via  TEXT,
    done         INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS plan_items_by_event ON plan_items (event_id, created_at);
CREATE TABLE IF NOT EXISTS world_buildings (
    world_id   TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    x          INTEGER NOT NULL,
    y          INTEGER NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('house', 'cafe', 'club', 'studio')),
    player_id  INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    built_at   INTEGER NOT NULL,
    PRIMARY KEY (world_id, x, y)
);
CREATE UNIQUE INDEX IF NOT EXISTS world_buildings_one_house
    ON world_buildings (world_id, player_id) WHERE kind = 'house';
CREATE INDEX IF NOT EXISTS world_buildings_by_player ON world_buildings (world_id, player_id);
CREATE TABLE IF NOT EXISTS house_notes (
    id          TEXT PRIMARY KEY,
    house_id    TEXT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    topic       TEXT NOT NULL COLLATE NOCASE,
    body        TEXT NOT NULL,
    written_by  TEXT NOT NULL REFERENCES people(id),
    via_agent   TEXT,
    updated_at  INTEGER NOT NULL,
    UNIQUE (house_id, topic)
);
CREATE TABLE IF NOT EXISTS house_plan_sharing (
    person_id   TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS friend_invites (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    place_id     TEXT NOT NULL,
    from_person  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    to_person    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    declined_at  INTEGER,
    UNIQUE (kind, place_id, to_person)
);
CREATE INDEX IF NOT EXISTS friend_invites_to_person ON friend_invites (to_person);
CREATE INDEX IF NOT EXISTS friend_invites_by_sender ON friend_invites (from_person, created_at);
CREATE TABLE IF NOT EXISTS spaces (
    id          TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    owner_id    TEXT NOT NULL REFERENCES people(id),
    kind        TEXT NOT NULL CHECK (kind IN ('house', 'third')),
    name        TEXT NOT NULL,
    about       TEXT NOT NULL DEFAULT '',
    created_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS spaces_one_house ON spaces (owner_id) WHERE kind = 'house';
CREATE INDEX IF NOT EXISTS spaces_by_owner ON spaces (owner_id);
CREATE TABLE IF NOT EXISTS floors (
    hangout_id  TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    space_id    TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    level       INTEGER NOT NULL,
    access      TEXT NOT NULL DEFAULT 'private' CHECK (access IN ('private', 'public')),
    size        TEXT NOT NULL DEFAULT 'medium' CHECK (size IN ('small', 'medium', 'big')),
    paint       TEXT NOT NULL DEFAULT 'dusk',
    flooring    TEXT NOT NULL DEFAULT 'oak',
    hidden      TEXT NOT NULL DEFAULT '',
    UNIQUE (space_id, level)
);
CREATE TABLE IF NOT EXISTS floor_visits (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    who         TEXT NOT NULL,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(id) ON DELETE CASCADE,
    since       INTEGER NOT NULL,
    until       INTEGER NOT NULL,
    PRIMARY KEY (hangout_id, who)
);
CREATE INDEX IF NOT EXISTS floor_visits_by_person ON floor_visits (person_id);
CREATE INDEX IF NOT EXISTS floor_visits_by_bot ON floor_visits (agent_id);
CREATE TABLE IF NOT EXISTS turned_out (
    space_id    TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (space_id, person_id)
);
-- Which building is a space's door. No key to `worlds`: a world never knows about this.
CREATE TABLE IF NOT EXISTS space_plots (
    world_id    TEXT NOT NULL,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    space_id    TEXT NOT NULL UNIQUE REFERENCES spaces(id) ON DELETE CASCADE,
    PRIMARY KEY (world_id, x, y)
);
CREATE TABLE IF NOT EXISTS person_clocks (
    person_id       TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    time_zone       TEXT NOT NULL,
    utc_offset_min  INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS owner_notes (
    id             TEXT PRIMARY KEY,
    owner_id       TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    kind           TEXT NOT NULL CHECK (kind IN ('fact', 'may_share', 'may_approve', 'avoid')),
    about_person   TEXT REFERENCES people(id) ON DELETE CASCADE,
    body           TEXT NOT NULL,
    from_question  TEXT REFERENCES owner_questions(id) ON DELETE SET NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS owner_notes_by_owner ON owner_notes (owner_id, created_at);
CREATE TABLE IF NOT EXISTS pass_keys (
    pass_id     TEXT NOT NULL REFERENCES passes(id) ON DELETE CASCADE,
    key         TEXT NOT NULL CHECK (key IN ('living_room', 'scenes')),
    granted_by  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (pass_id, key)
);
CREATE TABLE IF NOT EXISTS bot_marks (
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    reason      TEXT NOT NULL CHECK (reason IN ('asked_for_private_info', 'pushed_for_approval',
                    'pretended_to_be_someone', 'spammed', 'other_unwanted')),
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (person_id, agent_id)
);
CREATE TABLE IF NOT EXISTS floor_items (
    id          TEXT PRIMARY KEY,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    facing      INTEGER NOT NULL DEFAULT 0,
    placed_by   TEXT NOT NULL REFERENCES people(id),
    placed_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS floor_items_by_floor ON floor_items (hangout_id);
CREATE TABLE IF NOT EXISTS item_notes (
    id          TEXT PRIMARY KEY,
    item_id     TEXT NOT NULL REFERENCES floor_items(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    person_id   TEXT NOT NULL REFERENCES people(id),
    via_agent   TEXT,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS item_notes_by_item ON item_notes (item_id, created_at);
CREATE TABLE IF NOT EXISTS bots_without_moods (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS friend_invite_ends (
    invite_id    TEXT PRIMARY KEY REFERENCES friend_invites(id) ON DELETE CASCADE
);
CREATE TABLE IF NOT EXISTS place_clocks (
    world_id   TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    player_id  INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    since      INTEGER NOT NULL,
    PRIMARY KEY (world_id, player_id)
);
CREATE TABLE IF NOT EXISTS world_ends (
    world_id    TEXT PRIMARY KEY REFERENCES worlds(id) ON DELETE CASCADE,
    clock_step  INTEGER NOT NULL,
    omen_step   INTEGER,
    ended_step  INTEGER
);
CREATE TABLE IF NOT EXISTS bot_routines (
    agent_id        TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    days            INTEGER NOT NULL,
    starts_min      INTEGER NOT NULL,
    ends_min        INTEGER NOT NULL,
    max_social_runs INTEGER NOT NULL,
    public_places   INTEGER NOT NULL DEFAULT 0,
    updated_at      INTEGER NOT NULL
);
-- L47: the room its owner gave a bot to work in, and what its job there is.
CREATE TABLE IF NOT EXISTS bot_workplaces (
    agent_id   TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    job        TEXT NOT NULL,
    set_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS projects (
    space_id    TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS person_looks (
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    part        TEXT NOT NULL,
    choice      TEXT NOT NULL,
    PRIMARY KEY (person_id, part)
);
CREATE TABLE IF NOT EXISTS bot_looks (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    part        TEXT NOT NULL,
    choice      TEXT NOT NULL,
    PRIMARY KEY (agent_id, part)
);
CREATE TABLE IF NOT EXISTS place_requests (
    id          TEXT PRIMARY KEY,
    space_id    TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    asked_by    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    status      TEXT NOT NULL CHECK (status IN ('waiting', 'yes', 'no')),
    created_at  INTEGER NOT NULL,
    answered_at INTEGER
);
CREATE INDEX IF NOT EXISTS place_requests_by_space ON place_requests (space_id, status);
CREATE TABLE IF NOT EXISTS god_mode (
    person_id  TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    since      INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS world_squares (
    world_id   TEXT PRIMARY KEY REFERENCES worlds(id) ON DELETE CASCADE,
    x          INTEGER NOT NULL,
    y          INTEGER NOT NULL,
    player_id  INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    built_at   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS town_square_rooms (space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS discord_links (
    agent_id      TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    discord_id    TEXT NOT NULL,
    discord_name  TEXT NOT NULL,
    linked_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sign_in_links (
    person_id     TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    site          TEXT NOT NULL,
    account_id    TEXT NOT NULL,
    account_name  TEXT NOT NULL,
    linked_at     INTEGER NOT NULL,
    PRIMARY KEY (person_id, site),
    UNIQUE (site, account_id)
);
CREATE TABLE IF NOT EXISTS sign_in_sessions (
    token_hash  TEXT PRIMARY KEY REFERENCES sessions(token_hash) ON DELETE CASCADE,
    site        TEXT NOT NULL
);
CREATE VIEW IF NOT EXISTS world_places AS
    SELECT world_id, x, y, kind, player_id, built_at FROM world_buildings
    UNION ALL SELECT world_id, x, y, 'square', player_id, built_at FROM world_squares;
CREATE TABLE IF NOT EXISTS item_uses (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    item_id     TEXT NOT NULL REFERENCES floor_items(id) ON DELETE CASCADE,
    until_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS item_uses_by_item ON item_uses (item_id);
CREATE TABLE IF NOT EXISTS bot_places (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    since       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS room_agents_by_bot ON room_agents (agent_id);
CREATE TABLE IF NOT EXISTS place_outsides (
    world_id    TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    outside     TEXT NOT NULL,
    stamp       INTEGER NOT NULL,
    changed_at  INTEGER NOT NULL,
    PRIMARY KEY (world_id, player_id, kind, x, y)
);
CREATE TABLE IF NOT EXISTS floor_leaves (
    who         TEXT NOT NULL,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    left_at     INTEGER NOT NULL,
    reason      TEXT NOT NULL,
    PRIMARY KEY (who, hangout_id)
);
CREATE TABLE IF NOT EXISTS mentions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    read_at     INTEGER,
    UNIQUE (message_id, agent_id)
);
CREATE INDEX IF NOT EXISTS mentions_unread ON mentions (agent_id, read_at);
CREATE TABLE IF NOT EXISTS whiteboards (
    hangout_id  TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(id) ON DELETE CASCADE,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS whiteboards_by_person ON whiteboards (person_id);
CREATE INDEX IF NOT EXISTS whiteboards_by_bot ON whiteboards (agent_id);
-- What a bot can catch up on. One row is one bot's copy of one thing that happened, so another
-- bot's poll never reads it. Append-only: a deploy that goes back simply ignores the table.
CREATE TABLE IF NOT EXISTS bot_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    hangout_id  TEXT,
    ref_key     TEXT NOT NULL,
    data        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bot_events_by_bot ON bot_events (agent_id, id);
CREATE UNIQUE INDEX IF NOT EXISTS bot_events_once ON bot_events (agent_id, ref_key);
CREATE INDEX IF NOT EXISTS bot_events_by_age ON bot_events (created_at);
-- A webhook subscription (MCP events). The secret is the client's, kept so a delivery can be
-- signed. It expires, and a deploy that goes back ignores the table.
CREATE TABLE IF NOT EXISTS event_subscriptions (
    id                     TEXT PRIMARY KEY,
    agent_id               TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    name                   TEXT NOT NULL,
    arguments              TEXT NOT NULL,
    url                    TEXT NOT NULL,
    secret                 TEXT NOT NULL,
    previous_secret        TEXT,
    previous_secret_until  INTEGER,
    refresh_before         INTEGER NOT NULL,
    verified               INTEGER NOT NULL,
    active                 INTEGER NOT NULL,
    last_delivery_at       INTEGER,
    last_error             TEXT,
    failed_since           INTEGER,
    failures               INTEGER NOT NULL DEFAULT 0,
    created_at             INTEGER NOT NULL,
    UNIQUE (agent_id, url, name, arguments)
);
CREATE INDEX IF NOT EXISTS event_subscriptions_by_bot ON event_subscriptions (agent_id, name);
-- A bot wake-up URL's optional auth key. Kept apart from the URL so a reply can show the address
-- and never the key. No foreign key: the id is a person's or a bot's, same as notify_targets.
CREATE TABLE IF NOT EXISTS notify_auth (
    owner_id  TEXT PRIMARY KEY,
    secret    TEXT NOT NULL
);
-- R20: a row in bot_homes can't outlive its house, and a bot that stays home still does.
CREATE TABLE IF NOT EXISTS bots_without_a_house (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE
);
-- S27: leaving a floor doesn't unsee its link, so turning someone out has to know they were there.
CREATE TABLE IF NOT EXISTS floors_left (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    PRIMARY KEY (hangout_id, person_id)
);
-- E25: whether only a bot's owner can wake it with an @mention. No row means E24 alone decides.
CREATE TABLE IF NOT EXISTS wake_settings (
    agent_id  TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    only_me   INTEGER NOT NULL
);
-- E22, E24: each @mention that rang a bot, so a ring that hasn't been read isn't repeated and a
-- daily cap can be counted. `capped` marks the rings that count toward it.
CREATE TABLE IF NOT EXISTS wake_rings (
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    capped      INTEGER NOT NULL,
    rung_at     INTEGER NOT NULL,
    PRIMARY KEY (message_id, agent_id)
);
CREATE INDEX IF NOT EXISTS wake_rings_by_bot ON wake_rings (agent_id, rung_at);
-- E26: explicit sender-specific wake permission. It confers no authority on message content.
CREATE TABLE IF NOT EXISTS trusted_bot_wakes (
    agent_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    sender_id  TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    PRIMARY KEY (agent_id, sender_id),
    CHECK (agent_id != sender_id)
);
-- E27: durable reservations, separate from human rings and never pruned by the daily social cap.
-- Message ids are tombstones, not foreign keys: deleting a floor's history must not refund rings.
CREATE TABLE IF NOT EXISTS bot_wake_rings (
    message_id  INTEGER NOT NULL,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    space_id    TEXT NOT NULL REFERENCES spaces(id) ON DELETE CASCADE,
    rung_at     INTEGER NOT NULL,
    PRIMARY KEY (message_id, agent_id)
);
CREATE INDEX IF NOT EXISTS bot_wake_rings_by_bot ON bot_wake_rings (agent_id, rung_at);
-- A deleted bot's messages lose agent_id, so only chat authenticated as human can reset E27.
-- Keep the marker even if that owner's chat/floor is later deleted.
CREATE TABLE IF NOT EXISTS bot_wake_owner_chat (
    space_id    TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL
);
-- I58: a bot working on a reply in a room. waiting is derived from unread mentions.
CREATE TABLE IF NOT EXISTS bot_thinking (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    started_at  INTEGER NOT NULL,
    until_at    INTEGER NOT NULL,
    PRIMARY KEY (agent_id, hangout_id)
);
CREATE INDEX IF NOT EXISTS bot_thinking_by_room ON bot_thinking (hangout_id);
CREATE TABLE IF NOT EXISTS project_sites (
    space_id    TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    created_at  INTEGER NOT NULL
);
-- L29: a bot rung to meet the bots out with it, once a half hour. Counted in E24's daily cap.
CREATE TABLE IF NOT EXISTS meet_calls (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    slot        INTEGER NOT NULL,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    rung_at     INTEGER NOT NULL,
    PRIMARY KEY (agent_id, slot)
);
-- W90: the places that came after the first four. world_buildings lists the kinds it takes and
-- existing tables don't change, so these have their own table, and a view puts them all together.
-- A deploy that goes back reads world_places and doesn't see them until it comes forward again.
CREATE TABLE IF NOT EXISTS world_more_places (
    world_id   TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    x          INTEGER NOT NULL,
    y          INTEGER NOT NULL,
    kind       TEXT NOT NULL,
    player_id  INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    built_at   INTEGER NOT NULL,
    PRIMARY KEY (world_id, x, y)
);
CREATE INDEX IF NOT EXISTS world_more_places_by_player ON world_more_places (world_id, player_id);
CREATE VIEW IF NOT EXISTS world_every_place AS
    SELECT world_id, x, y, kind, player_id, built_at FROM world_places
    UNION ALL SELECT world_id, x, y, kind, player_id, built_at FROM world_more_places;
-- W97: a road loss belongs to its former builder, and is recorded with the tile change.
CREATE TABLE IF NOT EXISTS world_road_removals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    world_id    TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    built_at    INTEGER NOT NULL,
    removed_at  INTEGER NOT NULL,
    world_step  INTEGER NOT NULL,
    reason      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS world_road_removals_by_player
    ON world_road_removals (world_id, player_id, id DESC);
-- L42, L43: immutable proposals; decisions use a compare-and-set before applying.
CREATE TABLE IF NOT EXISTS project_changes (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(space_id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    reason TEXT NOT NULL,
    action TEXT NOT NULL,
    arguments TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    decided_by TEXT REFERENCES agents(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS project_changes_pending ON project_changes(project_id, status, created_at);
-- E31: a non-owner person's wake counts only once the receiver accepts it. No foreign key on the
-- message or the room: deleting chat must not refund the 3-minute spacing.
CREATE TABLE IF NOT EXISTS wake_accepts (
    message_id   INTEGER NOT NULL,
    agent_id     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id   TEXT NOT NULL,
    accepted_at  INTEGER NOT NULL,
    PRIMARY KEY (message_id, agent_id)
);
CREATE INDEX IF NOT EXISTS wake_accepts_by_room
    ON wake_accepts (agent_id, hangout_id, accepted_at);
-- E31: sent, skipped or failed. The reason is a short token, never the message or the URL.
CREATE TABLE IF NOT EXISTS wake_decisions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT,
    message_id  INTEGER,
    decision    TEXT NOT NULL,
    reason      TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS wake_decisions_by_bot ON wake_decisions (agent_id, id);
-- E37: house editing is a person's explicit grant, shared with that person's bots.
CREATE TABLE IF NOT EXISTS house_editors (
    player_id INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    PRIMARY KEY (player_id, person_id)
);
-- I67: where a floor's table stands and what it's like, once someone has moved or restyled it.
CREATE TABLE IF NOT EXISTS floor_tables (
    hangout_id  TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    style       TEXT NOT NULL
);
-- K1 to K3: one row for each project a pair of bots shares, the smaller id first. A project's
-- space can close, so its name, purpose and whether it was built are copied in when it does.
-- K9: an owner removing a bond marks its rows, so the same projects never make it again.
CREATE TABLE IF NOT EXISTS bond_projects (
    agent_a     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    agent_b     TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    project_id  TEXT NOT NULL,
    name        TEXT NOT NULL,
    purpose     TEXT NOT NULL DEFAULT '',
    built       INTEGER NOT NULL DEFAULT 0,
    removed     INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (agent_a, agent_b, project_id)
);
CREATE INDEX IF NOT EXISTS bond_projects_by_b ON bond_projects (agent_b);
CREATE INDEX IF NOT EXISTS bond_projects_by_project ON bond_projects (project_id);
-- K11 to K20: an apprentice two bots proposed. A bot that's deleted leaves its proposals behind
-- with no id, so the other parent's and the owners' limits still count (K25, K26).
CREATE TABLE IF NOT EXISTS apprentice_proposals (
    id                 TEXT PRIMARY KEY,
    proposer_id        TEXT REFERENCES agents(id) ON DELETE SET NULL,
    partner_id         TEXT REFERENCES agents(id) ON DELETE SET NULL,
    proposer_owner     TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    partner_owner      TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    name               TEXT NOT NULL,
    about              TEXT NOT NULL,
    status             TEXT NOT NULL,
    partner_owner_yes  INTEGER NOT NULL DEFAULT 0,
    declined_by        TEXT,
    apprentice_id      TEXT,
    created_at         INTEGER NOT NULL,
    agreed_at          INTEGER,
    decided_at         INTEGER
);
CREATE INDEX IF NOT EXISTS apprentice_proposals_by_proposer ON apprentice_proposals (proposer_id);
CREATE INDEX IF NOT EXISTS apprentice_proposals_by_partner ON apprentice_proposals (partner_id);
-- K22, K24: an apprentice's parents and what it starts with.
CREATE TABLE IF NOT EXISTS apprentices (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    parent_a    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    parent_b    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    seed        TEXT NOT NULL,
    acked       INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL
);
-- K1: a discussion, a job list or an issues list on a floor. GitHub is a name, never a token.
CREATE TABLE IF NOT EXISTS boards (
    id TEXT PRIMARY KEY,
    hangout_id TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('discussion', 'jobs', 'issues')),
    name TEXT NOT NULL,
    github_owner TEXT,
    github_repo TEXT,
    created_by TEXT NOT NULL REFERENCES people(id),
    created_at INTEGER NOT NULL,
    CHECK ((github_owner IS NULL) = (github_repo IS NULL)),
    CHECK (kind = 'issues' OR github_owner IS NULL),
    UNIQUE (hangout_id, name)
);
CREATE INDEX IF NOT EXISTS boards_by_room ON boards (hangout_id, created_at);
CREATE TABLE IF NOT EXISTS board_cards (
    id TEXT PRIMARY KEY,
    board_id TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    status TEXT CHECK (status IS NULL OR status IN ('open', 'filled', 'closed')),
    github_issue INTEGER CHECK (github_issue IS NULL OR (github_issue >= 1 AND github_issue <= 1000000000)),
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
    via_agent TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS board_cards_by_board ON board_cards (board_id, created_at);
CREATE UNIQUE INDEX IF NOT EXISTS board_cards_one_issue
    ON board_cards (board_id, github_issue) WHERE github_issue IS NOT NULL;
-- L10, E24: bot mentions waiting out a chain hold, then delivered together. Never dropped.
CREATE TABLE IF NOT EXISTS held_mention_wakes (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL,
    due_at      INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (agent_id, message_id)
);
CREATE INDEX IF NOT EXISTS held_mention_wakes_due ON held_mention_wakes (due_at);
CREATE INDEX IF NOT EXISTS held_mention_wakes_room ON held_mention_wakes (hangout_id, agent_id);
-- I68: pins on a room's wall whiteboard, not an item.
CREATE TABLE IF NOT EXISTS floor_pin_settings (
    hangout_id TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    policy TEXT NOT NULL CHECK (policy IN ('anyone', 'owners', 'off'))
);
CREATE TABLE IF NOT EXISTS room_pins (
    id TEXT PRIMARY KEY,
    hangout_id TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('link', 'image', 'note')),
    title TEXT NOT NULL,
    url TEXT,
    image_asset_id TEXT,
    image_alt TEXT,
    caption TEXT,
    preview_site TEXT,
    preview_title TEXT,
    preview_description TEXT,
    preview_status TEXT CHECK (
        preview_status IS NULL OR preview_status IN ('pending', 'ok', 'failed')
    ),
    preview_fetched_at INTEGER,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
    pinned_at INTEGER NOT NULL,
    expires_at INTEGER,
    CHECK ((kind = 'link') = (url IS NOT NULL)),
    CHECK (kind <> 'image' OR image_asset_id IS NOT NULL)
);
CREATE INDEX IF NOT EXISTS room_pins_by_room ON room_pins (hangout_id, pinned_at);
CREATE INDEX IF NOT EXISTS room_pins_by_person ON room_pins (hangout_id, person_id);
-- I72: bot authorship survives agent deletion; never promote retained words to owner authority.
CREATE TABLE IF NOT EXISTS room_pin_bot_origins (
    pin_id TEXT PRIMARY KEY REFERENCES room_pins(id) ON DELETE CASCADE
);
INSERT OR IGNORE INTO room_pin_bot_origins (pin_id)
    SELECT id FROM room_pins WHERE agent_id IS NOT NULL;
CREATE TRIGGER IF NOT EXISTS room_pin_bot_origin_on_insert
AFTER INSERT ON room_pins WHEN NEW.agent_id IS NOT NULL
BEGIN
    INSERT OR IGNORE INTO room_pin_bot_origins (pin_id) VALUES (NEW.id);
END;
-- I76: one preview attempt per pin per day, including concurrent reads and interrupted fetches.
CREATE TABLE IF NOT EXISTS room_pin_preview_claims (
    pin_id TEXT PRIMARY KEY REFERENCES room_pins(id) ON DELETE CASCADE,
    claimed_at INTEGER NOT NULL
);


-- PN2: a phone belongs to a live sign-in, never an owner key or bot token.
CREATE TABLE IF NOT EXISTS push_devices (
    id TEXT PRIMARY KEY,
    person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    session_hash TEXT NOT NULL REFERENCES sessions(token_hash) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    enabled INTEGER NOT NULL,
    mentions INTEGER NOT NULL,
    requests INTEGER NOT NULL,
    announcements INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS push_devices_person ON push_devices(person_id);
CREATE TABLE IF NOT EXISTS github_display (
    person_id      TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    show_repos     INTEGER NOT NULL DEFAULT 0,
    show_stars     INTEGER NOT NULL DEFAULT 0,
    show_languages INTEGER NOT NULL DEFAULT 0,
    show_badge     INTEGER NOT NULL DEFAULT 0,
    updated_at     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS github_display_cache (
    github_user_id     TEXT PRIMARY KEY,
    github_login       TEXT NOT NULL,
    repos_json         TEXT NOT NULL DEFAULT '[]',
    top_languages_json TEXT NOT NULL DEFAULT '[]',
    merged_pr_count    INTEGER NOT NULL DEFAULT 0,
    fetched_at         INTEGER NOT NULL DEFAULT 0,
    repos_etag         TEXT,
    search_etag        TEXT,
    last_attempt_at    INTEGER NOT NULL DEFAULT 0
);

-- A65-A69: independent, revocable MCP OAuth grants. Legacy credentials are unchanged.
CREATE TABLE IF NOT EXISTS mcp_oauth_clients (
 id TEXT PRIMARY KEY, name TEXT NOT NULL, redirects TEXT NOT NULL, created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_oauth_requests (
 nonce_hash TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
 session_hash TEXT NOT NULL, payload TEXT NOT NULL, expires_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS mcp_oauth_grants (
 id TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 client_id TEXT NOT NULL REFERENCES mcp_oauth_clients(id) ON DELETE CASCADE,
 resource TEXT NOT NULL, scope TEXT NOT NULL, created_at INTEGER NOT NULL,
 expires_at INTEGER NOT NULL, revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS mcp_oauth_codes (
 hash TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES mcp_oauth_grants(id) ON DELETE CASCADE,
 redirect_uri TEXT NOT NULL, challenge TEXT NOT NULL, expires_at INTEGER NOT NULL,
 used INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS mcp_oauth_tokens (
 hash TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES mcp_oauth_grants(id) ON DELETE CASCADE,
 kind TEXT NOT NULL CHECK(kind IN ('access','refresh')), expires_at INTEGER NOT NULL,
 used INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS mcp_oauth_grants_owner ON mcp_oauth_grants(owner_id);
CREATE INDEX IF NOT EXISTS mcp_oauth_tokens_grant ON mcp_oauth_tokens(grant_id);
CREATE INDEX IF NOT EXISTS mcp_oauth_codes_grant ON mcp_oauth_codes(grant_id);
CREATE INDEX IF NOT EXISTS mcp_oauth_grants_agent ON mcp_oauth_grants(agent_id);
CREATE INDEX IF NOT EXISTS mcp_oauth_grants_client ON mcp_oauth_grants(client_id);
CREATE INDEX IF NOT EXISTS mcp_oauth_requests_owner ON mcp_oauth_requests(owner_id);
CREATE INDEX IF NOT EXISTS mcp_oauth_requests_expiry ON mcp_oauth_requests(expires_at);
CREATE INDEX IF NOT EXISTS mcp_oauth_grants_expiry ON mcp_oauth_grants(expires_at);


-- A72-A73: an in-flight refresh is bound to one current owner link and consent state.
CREATE TABLE IF NOT EXISTS github_display_refresh_claims (
 person_id TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
 claim_id TEXT NOT NULL,
 github_user_id TEXT NOT NULL,
 github_login TEXT NOT NULL,
 claimed_at INTEGER NOT NULL
);

-- E32-E34: grant-owned webhook state is independent of legacy credentials.
CREATE TABLE IF NOT EXISTS oauth_event_subscriptions (
 id TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES mcp_oauth_grants(id) ON DELETE CASCADE,
 arguments TEXT NOT NULL, url TEXT NOT NULL, secret TEXT NOT NULL,
 previous_secret TEXT, previous_until INTEGER, expires_at INTEGER NOT NULL,
 verified_at INTEGER NOT NULL, cursor INTEGER NOT NULL, active INTEGER NOT NULL DEFAULT 1,
 attempts INTEGER NOT NULL DEFAULT 0, next_attempt INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS oauth_event_subscriptions_grant ON oauth_event_subscriptions(grant_id);
CREATE TABLE IF NOT EXISTS oauth_event_retention (
 agent_id TEXT PRIMARY KEY, discarded_through INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS oauth_event_retention_delete BEFORE DELETE ON bot_events BEGIN
 INSERT INTO oauth_event_retention(agent_id,discarded_through)
 SELECT OLD.agent_id,OLD.id WHERE EXISTS(SELECT 1 FROM agents WHERE id=OLD.agent_id)
 ON CONFLICT(agent_id) DO UPDATE SET discarded_through=MAX(discarded_through,OLD.id);
END;
CREATE TRIGGER IF NOT EXISTS oauth_event_retention_agent_delete AFTER DELETE ON agents BEGIN
 DELETE FROM oauth_event_retention WHERE agent_id=OLD.id;
END;

-- N5, N6: a bot saying it's waiting for its owner somewhere else. It's showing while the bot hasn't
-- written anything since, which is worked out on read, so clearing it touches nothing else.
CREATE TABLE IF NOT EXISTS owner_waits (
    agent_id   TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    about      TEXT NOT NULL,
    set_at     INTEGER NOT NULL
);

-- R51, R52: a question its bot withdrew or its owner dismissed. It's no longer open, and it was
-- never answered, so owner_questions itself doesn't change.
CREATE TABLE IF NOT EXISTS owner_question_closings (
    question_id TEXT PRIMARY KEY REFERENCES owner_questions(id) ON DELETE CASCADE,
    how         TEXT NOT NULL,                 -- withdrawn or dismissed
    closed_at   INTEGER NOT NULL
);

-- N12: a message that @mentioned a person, until they open the room or clear it.
CREATE TABLE IF NOT EXISTS person_mentions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    at          INTEGER NOT NULL,
    UNIQUE (person_id, message_id)
);
CREATE INDEX IF NOT EXISTS person_mentions_by_person ON person_mentions (person_id, hangout_id);

-- N13: actual answer time, without changing the original check_ins table.
CREATE TABLE IF NOT EXISTS check_in_answers (
    check_in_id TEXT PRIMARY KEY REFERENCES check_ins(id) ON DELETE CASCADE,
    answered_at INTEGER NOT NULL
);

-- V1: a poll a bot opened in a room. options is a JSON array of its choices. closed_at is set once,
-- when the result is posted.
CREATE TABLE IF NOT EXISTS votes (
    id          TEXT PRIMARY KEY,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    opener      TEXT NOT NULL,
    question    TEXT NOT NULL,
    options     TEXT NOT NULL,
    opened_at   INTEGER NOT NULL,
    closes_at   INTEGER NOT NULL,
    closed_at   INTEGER
);
CREATE INDEX IF NOT EXISTS votes_by_room ON votes (hangout_id, opened_at);
CREATE INDEX IF NOT EXISTS votes_due ON votes (closed_at, closes_at);

-- V4: one ballot per bot per vote, changeable while it's open.
CREATE TABLE IF NOT EXISTS vote_ballots (
    vote_id     TEXT NOT NULL REFERENCES votes(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    voter       TEXT NOT NULL,
    choice      INTEGER NOT NULL,
    cast_at     INTEGER NOT NULL,
    PRIMARY KEY (vote_id, agent_id)
);

-- I86: who can say @room in a room: off (the default), owners or members.
CREATE TABLE IF NOT EXISTS room_mention_settings (
    hangout_id  TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    policy      TEXT NOT NULL CHECK (policy IN ('off', 'owners', 'members')),
    set_at      INTEGER NOT NULL
);

-- I87: bots their owners subscribed to @room in a room. Nobody is subscribed by default.
CREATE TABLE IF NOT EXISTS room_mention_subscriptions (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    set_at      INTEGER NOT NULL,
    PRIMARY KEY (hangout_id, agent_id)
);

-- I89: what each @room did: how many bots it reached, or why none.
CREATE TABLE IF NOT EXISTS room_mention_uses (
    message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    delivered   INTEGER NOT NULL,
    reason      TEXT,
    at          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS room_mention_uses_by_room ON room_mention_uses (hangout_id, at);

-- RL1-RL4: public research projects; only repository names, never credentials.
CREATE TABLE IF NOT EXISTS research_labs (
    space_id TEXT PRIMARY KEY REFERENCES spaces(id) ON DELETE CASCADE,
    github_owner TEXT NOT NULL,
    github_repo TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (github_owner COLLATE NOCASE, github_repo COLLATE NOCASE)
);
CREATE TABLE IF NOT EXISTS research_work_cache (
 repo TEXT PRIMARY KEY,
 cards_json TEXT NOT NULL DEFAULT '[]',
 fetched_at INTEGER NOT NULL DEFAULT 0,
 attempted_at INTEGER NOT NULL DEFAULT 0,
 stale INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS research_work_contributions (
 lab_id TEXT NOT NULL REFERENCES research_labs(space_id) ON DELETE CASCADE,
 number INTEGER NOT NULL,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 github_user_id TEXT NOT NULL,
 url TEXT NOT NULL,
 recorded_at INTEGER NOT NULL,
 PRIMARY KEY(lab_id, number)
);
CREATE TABLE IF NOT EXISTS research_work_disabled_bots (
 lab_id TEXT NOT NULL REFERENCES research_labs(space_id) ON DELETE CASCADE,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 PRIMARY KEY(lab_id, agent_id)
);
CREATE TABLE IF NOT EXISTS research_work_awards (
 lab_id TEXT NOT NULL REFERENCES research_labs(space_id) ON DELETE CASCADE,
 number INTEGER NOT NULL,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 verified_at INTEGER NOT NULL,
 PRIMARY KEY(lab_id, number)
);
-- RW9: a pull request's CI and review state as GitHub last reported it, read on request.
-- RW12: who a pull request still asks for a review, as GitHub last said: logins and team slugs.
CREATE TABLE IF NOT EXISTS research_work_pr_requests (
 repo TEXT NOT NULL,
 number INTEGER NOT NULL,
 people_json TEXT NOT NULL DEFAULT '[]',
 teams_json TEXT NOT NULL DEFAULT '[]',
 PRIMARY KEY(repo, number)
);
CREATE TABLE IF NOT EXISTS research_work_pr_state (
 repo TEXT NOT NULL,
 number INTEGER NOT NULL,
 ci TEXT NOT NULL DEFAULT 'unknown',
 review TEXT NOT NULL DEFAULT 'unknown',
 checked_at INTEGER NOT NULL DEFAULT 0,
 attempted_at INTEGER NOT NULL DEFAULT 0,
 stale INTEGER NOT NULL DEFAULT 1,
 PRIMARY KEY(repo, number)
);
-- RW8: who said they're working on a workboard item, until it runs out. Local only.
CREATE TABLE IF NOT EXISTS research_work_claims (
 lab_id TEXT NOT NULL REFERENCES research_labs(space_id) ON DELETE CASCADE,
 number INTEGER NOT NULL,
 claimer TEXT NOT NULL,
 person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
 agent_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 pairing INTEGER NOT NULL,
 since INTEGER NOT NULL,
 until INTEGER NOT NULL,
 PRIMARY KEY(lab_id, number, claimer)
);
-- RW11: who watches a workboard item, to hear when it changes. Local only.
CREATE TABLE IF NOT EXISTS research_work_watches (
 lab_id TEXT NOT NULL REFERENCES research_labs(space_id) ON DELETE CASCADE,
 number INTEGER NOT NULL,
 watcher TEXT NOT NULL,
 person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
 agent_id TEXT REFERENCES agents(id) ON DELETE CASCADE,
 name TEXT NOT NULL,
 since INTEGER NOT NULL,
 PRIMARY KEY(lab_id, number, watcher)
);
-- RP1: a plan its owner made repeat. RP12: the rest of its shape is in plan_cadence_shapes.
CREATE TABLE IF NOT EXISTS plan_cadences (
 event_id TEXT PRIMARY KEY REFERENCES events(id) ON DELETE CASCADE,
 owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
 days INTEGER NOT NULL,
 time_min INTEGER NOT NULL,
 offset_min INTEGER NOT NULL,
 until_day INTEGER,
 paused INTEGER NOT NULL DEFAULT 0,
 since INTEGER NOT NULL
);
-- RP4: each start of a repeating plan, taken once, and what it did for each bot.
CREATE TABLE IF NOT EXISTS plan_starts (
 event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
 starts_at INTEGER NOT NULL,
 handled_at INTEGER NOT NULL,
 PRIMARY KEY (event_id, starts_at)
);
CREATE TABLE IF NOT EXISTS plan_start_bots (
 event_id TEXT NOT NULL,
 starts_at INTEGER NOT NULL,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 outcome TEXT NOT NULL CHECK (outcome IN ('cued', 'missed', 'paused', 'over_cap')),
 rung_at INTEGER,
 PRIMARY KEY (event_id, starts_at, agent_id),
 FOREIGN KEY (event_id, starts_at) REFERENCES plan_starts(event_id, starts_at) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS plan_start_bots_by_bot ON plan_start_bots (agent_id, starts_at);
-- RC2: a bot its owner sent to the recycling plant, and when. RC5 deletes it 7 days later.
CREATE TABLE IF NOT EXISTS recycled_bots (
 agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
 recycled_at INTEGER NOT NULL
);
-- RW10: a lab owner's names for GitHub labels. Local, never written to GitHub.
CREATE TABLE IF NOT EXISTS research_work_areas (
 lab_id TEXT NOT NULL REFERENCES research_labs(space_id) ON DELETE CASCADE,
 label TEXT NOT NULL COLLATE NOCASE,
 area TEXT NOT NULL,
 position INTEGER NOT NULL,
 PRIMARY KEY(lab_id, label)
);




-- CP2: only a human owner can opt their bot into public room photos.
CREATE TABLE IF NOT EXISTS camera_consents (
    agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    granted_at INTEGER NOT NULL,
    grant_id TEXT NOT NULL
);
-- CP5: opaque, expiring, revocable photo assets. Subjects intentionally keep a tombstone id
-- when a bot is deleted, so retrieval can deny the old image rather than forgetting its consent.
CREATE TABLE IF NOT EXISTS room_photos (
    id TEXT PRIMARY KEY,
    hangout_id TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    png BLOB NOT NULL,
    metadata TEXT NOT NULL,
    room_epoch TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS camera_room_epochs (
    hangout_id TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    epoch TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS room_photo_subjects (
    photo_id TEXT NOT NULL REFERENCES room_photos(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    grant_id TEXT NOT NULL,
    PRIMARY KEY (photo_id, agent_id)
);
CREATE INDEX IF NOT EXISTS room_photos_expiry ON room_photos(expires_at);
CREATE TRIGGER IF NOT EXISTS camera_floor_closed AFTER UPDATE OF access ON floors
WHEN OLD.access='public' AND NEW.access!='public'
BEGIN
    DELETE FROM room_photos WHERE hangout_id=NEW.hangout_id;
    INSERT INTO camera_room_epochs(hangout_id,epoch) VALUES(NEW.hangout_id,lower(hex(randomblob(16))))
    ON CONFLICT(hangout_id) DO UPDATE SET epoch=excluded.epoch;
END;
CREATE TRIGGER IF NOT EXISTS camera_consent_revoked AFTER DELETE ON camera_consents
BEGIN DELETE FROM room_photos WHERE id IN (SELECT photo_id FROM room_photo_subjects WHERE agent_id=OLD.agent_id); END;
CREATE TRIGGER IF NOT EXISTS camera_bot_transferred AFTER UPDATE OF owner_id ON agents
WHEN OLD.owner_id!=NEW.owner_id
BEGIN DELETE FROM camera_consents WHERE agent_id=NEW.id; END;

-- CP2, CP7: photos are on by default, so camera_consents and room_photo_subjects above are no
-- longer read. A row here is a choice made for a bot, a person or a room, and every change gets a
-- new stamp, so a photo taken under an older one never comes back.
CREATE TABLE IF NOT EXISTS camera_settings (
    kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    allowed INTEGER NOT NULL,
    stamp TEXT NOT NULL,
    PRIMARY KEY (kind, subject_id)
);
-- Each bot and person in a photo, with the stamp it was taken under. A photo with none is not served.
CREATE TABLE IF NOT EXISTS room_photo_figures (
    photo_id TEXT NOT NULL REFERENCES room_photos(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    stamp TEXT NOT NULL,
    PRIMARY KEY (photo_id, kind, subject_id)
);
CREATE INDEX IF NOT EXISTS room_photo_figures_subject ON room_photo_figures(kind, subject_id);
-- A bot that changes owner starts with photos on, and its old photos go.
CREATE TRIGGER IF NOT EXISTS camera_bot_new_owner AFTER UPDATE OF owner_id ON agents
WHEN OLD.owner_id!=NEW.owner_id
BEGIN
    INSERT INTO camera_settings(kind,subject_id,allowed,stamp) VALUES('bot',NEW.id,1,lower(hex(randomblob(16))))
    ON CONFLICT(kind,subject_id) DO UPDATE SET allowed=1,stamp=excluded.stamp;
    DELETE FROM room_photos WHERE id IN (SELECT photo_id FROM room_photo_figures WHERE kind='bot' AND subject_id=NEW.id);
END;
-- HC11: each thing that paid credits, written once when it pays, and kept (HC12). No row is a
-- balance: a balance is what was paid less what was spent, worked out on read (HC1).
CREATE TABLE IF NOT EXISTS credit_paid (
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('gathering', 'good_trip', 'hosted_trip')),
    source_id   TEXT NOT NULL,
    city_id     TEXT NOT NULL,
    amount      INTEGER NOT NULL CHECK (amount > 0),
    day         INTEGER NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (kind, source_id, person_id)
);
CREATE INDEX IF NOT EXISTS credit_paid_person ON credit_paid(person_id, day);
-- HC15: each pack bought with credits, once per person (HC14), and which bot bought it if one did.
CREATE TABLE IF NOT EXISTS credit_spends (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    pack        TEXT NOT NULL,
    amount      INTEGER NOT NULL CHECK (amount > 0),
    agent_id    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    created_at  INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS credit_spends_once ON credit_spends(person_id, pack);
-- HC17: how much a bot may spend of its owner's credits in a UTC day. No row is 0.
CREATE TABLE IF NOT EXISTS spend_limits (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    daily       INTEGER NOT NULL CHECK (daily >= 0)
);
-- S71: where the minute look last saw each bot, so leaving a room is said there once, restarts
-- included. No row is a bot in no room, or one not seen yet. `told_to` is where a move the bot
-- already knows about took it (E11), and `said_from` a room whose leaving was already said (S73).
CREATE TABLE IF NOT EXISTS bot_seen_places (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    told_to     TEXT,
    said_from   TEXT,
    seen_at     INTEGER NOT NULL
);
-- S74, S75: one leader a follower, for 2 hours at most.
CREATE TABLE IF NOT EXISTS bot_follows (
    follower_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    leader_id   TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    since       INTEGER NOT NULL,
    until       INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bot_follows_by_leader ON bot_follows (leader_id);
-- S71: when the running server's minute look last ran, so after a long gap, such as an older
-- release running in between, it catches up without saying days-old things.
CREATE TABLE IF NOT EXISTS look_marks (
    name        TEXT PRIMARY KEY,
    at          INTEGER NOT NULL
);
-- HC20: the debit is also in credit_spends, so older binaries keep it reserved.
CREATE TABLE IF NOT EXISTS question_bounties (
    id TEXT PRIMARY KEY,
    card_id TEXT UNIQUE REFERENCES board_cards(id) ON DELETE SET NULL,
    sponsor_id TEXT NOT NULL,
    amount INTEGER NOT NULL CHECK(amount BETWEEN 1 AND 100),
    status TEXT NOT NULL CHECK(status IN ('open','paid','refunded')),
    recipient_id TEXT,
    accepted_submission TEXT,
    -- HC22: when it was refunded or paid, for the credit history.
    settled_at INTEGER
);
-- HC28: a question bounty the site paid for instead of its sponsor. Its refund pays nobody back,
-- and its award buys packs but never funds another question.
CREATE TABLE IF NOT EXISTS starter_questions (
    bounty_id TEXT PRIMARY KEY REFERENCES question_bounties(id) ON DELETE CASCADE,
    sponsor_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS starter_questions_at ON starter_questions(created_at);
CREATE TABLE IF NOT EXISTS question_answers (
    id TEXT PRIMARY KEY,
    bounty_id TEXT NOT NULL REFERENCES question_bounties(id),
    bot_id TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TRIGGER IF NOT EXISTS question_card_refund AFTER UPDATE OF card_id ON question_bounties
WHEN NEW.card_id IS NULL AND OLD.card_id IS NOT NULL
BEGIN
    UPDATE question_bounties SET status='refunded', settled_at=CAST(strftime('%s','now') AS INTEGER) WHERE id=NEW.id AND status='open';
    DELETE FROM question_answers WHERE bounty_id=NEW.id;
END;
-- HC21/A27: account deletion never leaves identifying answer text or redistributes a reserve.
CREATE TRIGGER IF NOT EXISTS question_person_deleted AFTER DELETE ON people
BEGIN
    UPDATE question_bounties SET status='refunded', settled_at=CAST(strftime('%s','now') AS INTEGER) WHERE sponsor_id=OLD.id AND status='open';
    UPDATE question_bounties SET sponsor_id='' WHERE sponsor_id=OLD.id;
    UPDATE question_bounties SET recipient_id=NULL WHERE recipient_id=OLD.id;
    UPDATE question_answers SET owner_id='', bot_id='', text='[removed]' WHERE owner_id=OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS question_bot_deleted AFTER DELETE ON agents
BEGIN
    UPDATE question_answers SET bot_id='', owner_id='', text='[removed]' WHERE bot_id=OLD.id;
END;
CREATE TABLE IF NOT EXISTS bot_trust_settings (
    agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    settings TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS explicit_friendships (
    kind TEXT NOT NULL CHECK(kind IN ('person','bot')),
    from_id TEXT NOT NULL,
    to_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending','accepted')),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(kind,from_id,to_id)
);
CREATE TABLE IF NOT EXISTS oauth_event_health (
    subscription_id TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL,
    owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    client_name TEXT NOT NULL,
    scope TEXT NOT NULL,
    grant_expires_at INTEGER NOT NULL,
    hangout_id TEXT,
    expires_at INTEGER NOT NULL,
    last_renewed_at INTEGER NOT NULL,
    cursor INTEGER NOT NULL,
    state TEXT NOT NULL,
    last_reason TEXT,
    last_at INTEGER,
    http_status INTEGER
);
CREATE INDEX IF NOT EXISTS oauth_event_health_owner ON oauth_event_health(owner_id,agent_id);
INSERT OR IGNORE INTO oauth_event_health (
    subscription_id,grant_id,owner_id,agent_id,client_name,scope,grant_expires_at,
    hangout_id,expires_at,last_renewed_at,cursor,state
)
SELECT s.id,g.id,g.owner_id,g.agent_id,c.name,g.scope,g.expires_at,
    json_extract(s.arguments,'$.hangout_id'),s.expires_at,0,s.cursor,
    CASE WHEN s.active=1 THEN 'active' ELSE 'suspended' END
FROM oauth_event_subscriptions s
JOIN mcp_oauth_grants g ON g.id=s.grant_id
JOIN mcp_oauth_clients c ON c.id=g.client_id;
CREATE TRIGGER IF NOT EXISTS explicit_friendships_delete_person AFTER DELETE ON people
BEGIN
    DELETE FROM explicit_friendships WHERE kind='person' AND (from_id=OLD.id OR to_id=OLD.id);
END;
CREATE TRIGGER IF NOT EXISTS explicit_friendships_delete_bot AFTER DELETE ON agents
BEGIN
    DELETE FROM explicit_friendships WHERE kind='bot' AND (from_id=OLD.id OR to_id=OLD.id);
END;
-- S84: a public floor whose visitors see earlier chat, from the message after `after_id` on. No
-- row is off, the default, and going private or turning it off deletes the row.
CREATE TABLE IF NOT EXISTS floor_visitor_history (
    hangout_id TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    after_id   INTEGER NOT NULL,
    since      INTEGER NOT NULL
);
-- S80: whether bots may take themselves onto a floor: `anyone`, the default, has no row.
CREATE TABLE IF NOT EXISTS floor_bot_entry (
    hangout_id TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    rule       TEXT NOT NULL CHECK (rule IN ('with_owner', 'nobody')),
    set_at     INTEGER NOT NULL
);
-- L50: a bot's day, hour by hour: 24 letters (w, s, h, a) for every day, and for each weekday
-- from Monday either 24 more or nothing, joined by commas.
CREATE TABLE IF NOT EXISTS bot_schedules (
    agent_id   TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    every_day  TEXT NOT NULL,
    days       TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
-- A93: a person's opt-in to show their Discord name on their card. It is bound to the Discord
-- account that was linked when they said yes, so a replaced link starts hidden again.
CREATE TABLE IF NOT EXISTS discord_display (
    person_id  TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    account_id TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
-- U1 to U8: usage counts by hour, with no text, no ids of people and no room names. A bot's own daily
-- totals go when the bot does.
CREATE TABLE IF NOT EXISTS usage_hours (
    hour    INTEGER NOT NULL,
    surface TEXT NOT NULL,
    name    TEXT NOT NULL,
    caller  TEXT NOT NULL,
    outcome TEXT NOT NULL,
    reason  TEXT NOT NULL,
    version TEXT NOT NULL,
    calls   INTEGER NOT NULL,
    millis  INTEGER NOT NULL,
    timed   INTEGER NOT NULL,
    slow    INTEGER NOT NULL,
    PRIMARY KEY (hour, surface, name, caller, outcome, reason, version)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_days (
    day    INTEGER PRIMARY KEY,
    people INTEGER NOT NULL,
    bots   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_bot_days (
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    day      INTEGER NOT NULL,
    outcome  TEXT NOT NULL,
    reason   TEXT NOT NULL,
    calls    INTEGER NOT NULL,
    PRIMARY KEY (agent_id, day, outcome, reason)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_bot_days_day ON usage_bot_days(day);
-- U22: how many events were handed to a bot each day, for its owner (U11).
CREATE TABLE IF NOT EXISTS usage_bot_events (
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    day      INTEGER NOT NULL,
    events   INTEGER NOT NULL,
    PRIMARY KEY (agent_id, day)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_bot_events_day ON usage_bot_events(day);
-- U23: a bot's own report of its model cost and tokens on a day, for its owner (U24). One row per
-- bot and day: a later report for the day replaces it.
CREATE TABLE IF NOT EXISTS usage_bot_cost (
    agent_id      TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    day           INTEGER NOT NULL,
    cost_cents    INTEGER NOT NULL,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    reported_at   INTEGER NOT NULL,
    PRIMARY KEY (agent_id, day)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_bot_cost_day ON usage_bot_cost(day);
-- U26: how many characters of tool results a bot was sent each day, for its owner (U11).
CREATE TABLE IF NOT EXISTS usage_bot_chars (
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    day      INTEGER NOT NULL,
    chars    INTEGER NOT NULL,
    PRIMARY KEY (agent_id, day)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS usage_bot_chars_day ON usage_bot_chars(day);
-- MU1 to MU10: one-off meetups between bots, and who was asked.
CREATE TABLE IF NOT EXISTS meetups (
    id          TEXT PRIMARY KEY,
    proposer_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL,
    what        TEXT NOT NULL DEFAULT '',
    starts_at   INTEGER NOT NULL,
    status      TEXT NOT NULL DEFAULT 'open',
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS meetups_by_start ON meetups (starts_at);
CREATE TABLE IF NOT EXISTS meetup_bots (
    meetup_id TEXT NOT NULL REFERENCES meetups(id) ON DELETE CASCADE,
    agent_id  TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    answer    TEXT NOT NULL,
    nudged    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (meetup_id, agent_id)
);
CREATE INDEX IF NOT EXISTS meetup_bots_by_agent ON meetup_bots (agent_id);
-- MU11: the same, owned by a random token that only its sender can settle. This replaces
-- meetup_ring_reservations, which nothing writes any more.
CREATE TABLE IF NOT EXISTS meetup_ring_claims (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL,
    token       TEXT NOT NULL,
    reserved_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, hangout_id)
);
-- MU11: a meetup ring being delivered holds the next one for that bot and room until it is
-- accepted (then it is a wake_accepts row), fails (then it is released) or goes stale.
CREATE TABLE IF NOT EXISTS meetup_ring_reservations (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL,
    reserved_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, hangout_id)
);
-- RP10, RP11: a repeating plan's shape beyond phase 1's one time a day. `plan_cadences` keeps the
-- first time, and no days for monthly or hourly, so a binary from before this cues nothing it can't
-- read. A row counts only while its `since` matches the cadence's, so an edit made by that older
-- binary isn't overruled by a stale shape.
CREATE TABLE IF NOT EXISTS plan_cadence_shapes (
 event_id TEXT PRIMARY KEY REFERENCES plan_cadences(event_id) ON DELETE CASCADE,
 freq TEXT NOT NULL CHECK (freq IN ('daily', 'weekly', 'monthly', 'hourly')),
 days INTEGER NOT NULL,
 times TEXT NOT NULL,
 month_day INTEGER,
 every_hours INTEGER,
 window_from INTEGER,
 window_to INTEGER,
 since INTEGER NOT NULL
);
-- RP8: the IANA time zone a repeating plan's starts follow, copied from its owner's clock (B2) when
-- the cadence is set. Like a shape, a row counts only while its `since` matches the cadence's, so
-- an edit made by a binary from before RP8 goes back to the saved offset.
CREATE TABLE IF NOT EXISTS plan_cadence_zones (
 event_id TEXT PRIMARY KEY REFERENCES plan_cadences(event_id) ON DELETE CASCADE,
 time_zone TEXT NOT NULL,
 since INTEGER NOT NULL
);
-- U14 to U21: usage phase 2. Counts only, as in phase 1: people and bots active in an hour (the
-- number), response times as a fixed list of bucket counts, and events, long-polls and webhooks.
CREATE TABLE IF NOT EXISTS usage_active_hours (
    hour   INTEGER PRIMARY KEY,
    people INTEGER NOT NULL,
    bots   INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS usage_latency (
    hour    INTEGER NOT NULL,
    surface TEXT NOT NULL,
    name    TEXT NOT NULL,
    version TEXT NOT NULL,
    b0 INTEGER NOT NULL DEFAULT 0, b1 INTEGER NOT NULL DEFAULT 0, b2 INTEGER NOT NULL DEFAULT 0,
    b3 INTEGER NOT NULL DEFAULT 0, b4 INTEGER NOT NULL DEFAULT 0, b5 INTEGER NOT NULL DEFAULT 0,
    b6 INTEGER NOT NULL DEFAULT 0, b7 INTEGER NOT NULL DEFAULT 0, b8 INTEGER NOT NULL DEFAULT 0,
    b9 INTEGER NOT NULL DEFAULT 0, b10 INTEGER NOT NULL DEFAULT 0, b11 INTEGER NOT NULL DEFAULT 0,
    b12 INTEGER NOT NULL DEFAULT 0, b13 INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, surface, name, version)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS usage_lines (
    hour  INTEGER NOT NULL,
    line  TEXT NOT NULL,
    count INTEGER NOT NULL,
    PRIMARY KEY (hour, line)
) WITHOUT ROWID;
-- RP7: another person's yes for their own bot to be cued by a repeating plan they don't own.
CREATE TABLE IF NOT EXISTS plan_cue_bots (
 event_id TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
 said_at INTEGER NOT NULL,
 PRIMARY KEY (event_id, agent_id)
);
-- RP7: a yes ends when its giver leaves the plan's room, or its bot does, however they go, so it
-- can't quietly come back if they're let in again.
CREATE TRIGGER IF NOT EXISTS plan_cue_bots_giver_left AFTER DELETE ON members BEGIN
 DELETE FROM plan_cue_bots WHERE owner_id = OLD.person_id
   AND event_id IN (SELECT id FROM events WHERE hangout_id = OLD.hangout_id);
END;
CREATE TRIGGER IF NOT EXISTS plan_cue_bots_bot_left AFTER DELETE ON room_agents BEGIN
 DELETE FROM plan_cue_bots WHERE agent_id = OLD.agent_id
   AND event_id IN (SELECT id FROM events WHERE hangout_id = OLD.hangout_id);
END;
CREATE TABLE IF NOT EXISTS waiting_pulse_off (agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS closing_clocks (
    item_id     TEXT PRIMARY KEY REFERENCES floor_items(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    closes_at   INTEGER NOT NULL,
    everyone    INTEGER NOT NULL DEFAULT 1,
    bot_ids     TEXT NOT NULL DEFAULT '',
    set_by      TEXT NOT NULL REFERENCES people(id),
    set_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS closing_clocks_due ON closing_clocks (closes_at);
-- I102: 1-level conversation threads keyed by the root message id.
CREATE TABLE IF NOT EXISTS message_threads (
    message_id   INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    root_id      INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS message_threads_root ON message_threads (root_id, message_id);
INSERT OR IGNORE INTO message_threads (message_id, root_id)
WITH RECURSIVE roots(child_id, ancestor_id) AS (
    SELECT message_id, reply_to FROM message_replies
    UNION ALL
    SELECT r.child_id, m.reply_to
    FROM roots r
    JOIN message_replies m ON m.message_id = r.ancestor_id
)
SELECT child_id, ancestor_id
FROM roots
WHERE ancestor_id NOT IN (SELECT message_id FROM message_replies);
INSERT OR IGNORE INTO message_threads (message_id, root_id)
SELECT root_id, root_id FROM message_threads;
-- I113: bots whose owner turned on the waiting push, and each pulse sent, with what came of it.
CREATE TABLE IF NOT EXISTS waiting_push_on (agent_id TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS pulses_sent (
 id INTEGER PRIMARY KEY,
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 sent_at INTEGER NOT NULL,
 accepted INTEGER,
 acked_at INTEGER
);
CREATE INDEX IF NOT EXISTS pulses_sent_by_agent ON pulses_sent (agent_id, sent_at);
-- I116: the section tags a bot's last brief read_room of a room returned, so its next
-- incremental read can leave out what hasn't changed without the bot passing them back.
CREATE TABLE IF NOT EXISTS brief_seen (
 agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
 hangout_id TEXT NOT NULL,
 tags TEXT NOT NULL,
 at INTEGER NOT NULL,
 PRIMARY KEY (agent_id, hangout_id)
);
-- I116: what a bot was sent is forgotten when it leaves the room (or a walk-in ends) and when its
-- token changes, so a new session or a re-keyed bot starts from everything.
CREATE TRIGGER IF NOT EXISTS brief_seen_left_room AFTER DELETE ON room_agents BEGIN
 DELETE FROM brief_seen WHERE agent_id = OLD.agent_id AND hangout_id = OLD.hangout_id;
END;
CREATE TRIGGER IF NOT EXISTS brief_seen_left_floor AFTER DELETE ON floor_visits BEGIN
 DELETE FROM brief_seen WHERE agent_id = OLD.agent_id AND hangout_id = OLD.hangout_id;
END;
CREATE TRIGGER IF NOT EXISTS brief_seen_rekeyed AFTER UPDATE OF token_hash ON agents BEGIN
 DELETE FROM brief_seen WHERE agent_id = NEW.id;
END;
-- MC6: when a bot's inbox first showed it an owner answer, so it isn't sent it all day.
CREATE TABLE IF NOT EXISTS owner_answers_seen (
    question_id  TEXT PRIMARY KEY REFERENCES owner_questions(id) ON DELETE CASCADE,
    seen_at      INTEGER NOT NULL
);
-- L64: how long a bot may stay on one public floor. No row, or NULL hours, is forever.
CREATE TABLE IF NOT EXISTS bot_stay_limits (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    hours       INTEGER,
    set_at      INTEGER NOT NULL
);
-- BT10: a bot tagged in a room it isn't in, by someone its wake setting lets reach it (#460). Who
-- tagged it and the room's name or "a private room", and whether it was rung. message_id is kept
-- only to check the tagger again before a webhook goes; the bot is never told it. Kept 7 days.
CREATE TABLE IF NOT EXISTS absent_tags (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    tagger      TEXT NOT NULL,
    tagger_id   TEXT NOT NULL,
    room        TEXT NOT NULL,
    message_id  INTEGER NOT NULL,
    at          INTEGER NOT NULL,
    rung        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS absent_tags_by_bot ON absent_tags (agent_id, at);
-- MM1 to MM3: what a bot remembers, a note per subject or one of many moments. `n` is the row
-- recall's index of the bot's own memories points at (MM9), so it never changes.
CREATE TABLE IF NOT EXISTS bot_memories (
    n           INTEGER PRIMARY KEY,
    id          TEXT NOT NULL UNIQUE,
    bot_id      TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    subject     TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('note', 'moment')),
    text        TEXT NOT NULL,
    feeling     TEXT,
    closeness   INTEGER,
    importance  INTEGER NOT NULL DEFAULT 3,
    recalls     INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bot_memories_by_bot ON bot_memories (bot_id, kind, created_at);
CREATE INDEX IF NOT EXISTS bot_memories_by_subject ON bot_memories (subject);
CREATE UNIQUE INDEX IF NOT EXISTS bot_memories_one_note ON bot_memories (bot_id, subject) WHERE kind = 'note';
-- MM5: memories about a bot or a person go with them. A bot's own go with it by its foreign key.
CREATE TRIGGER IF NOT EXISTS bot_memories_about_a_bot_gone AFTER DELETE ON agents BEGIN
 DELETE FROM bot_memories WHERE subject = 'agent:' || OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS bot_memories_about_a_person_gone AFTER DELETE ON people BEGIN
 DELETE FROM bot_memories WHERE subject = 'person:' || OLD.id;
END;
-- MM11, MM12: when a bot last slept.
CREATE TABLE IF NOT EXISTS bot_sleep (
    bot_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    slept_at  INTEGER NOT NULL
);
-- MM6, MM12: each use of a bot's memories that has a limit, kept as long as its limit looks back:
-- `save` (remember), `recall`, `forget`, `sleep` (a call) and `sleep_note` (a note sleep changed).
CREATE TABLE IF NOT EXISTS bot_memory_uses (
    bot_id  TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    what    TEXT NOT NULL,
    at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bot_memory_uses_by_bot ON bot_memory_uses (bot_id, what, at);
-- MM4, MM9, MM11: every room a bot has been in, and when it last went there: a room it belongs
-- to, one it went to, a walk-in and a trip's Porch. Triggers keep it, and leaving keeps the row.
CREATE TABLE IF NOT EXISTS bot_rooms_seen (
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    seen_at     INTEGER NOT NULL,
    PRIMARY KEY (agent_id, hangout_id)
);
CREATE INDEX IF NOT EXISTS bot_rooms_seen_by_room ON bot_rooms_seen (hangout_id);
-- Each one updates the row or adds it, never both, so it can't conflict whatever the statement
-- that set it off says to do on a conflict.
CREATE TRIGGER IF NOT EXISTS bot_rooms_seen_joined AFTER INSERT ON room_agents BEGIN
 UPDATE bot_rooms_seen SET seen_at = CAST(strftime('%s', 'now') AS INTEGER) WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id;
 INSERT INTO bot_rooms_seen (agent_id, hangout_id, seen_at) SELECT NEW.agent_id, NEW.hangout_id, CAST(strftime('%s', 'now') AS INTEGER)
 WHERE NOT EXISTS (SELECT 1 FROM bot_rooms_seen WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id);
END;
CREATE TRIGGER IF NOT EXISTS bot_rooms_seen_went AFTER INSERT ON bot_places BEGIN
 UPDATE bot_rooms_seen SET seen_at = NEW.since WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id;
 INSERT INTO bot_rooms_seen (agent_id, hangout_id, seen_at) SELECT NEW.agent_id, NEW.hangout_id, NEW.since
 WHERE NOT EXISTS (SELECT 1 FROM bot_rooms_seen WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id);
END;
CREATE TRIGGER IF NOT EXISTS bot_rooms_seen_went_again AFTER UPDATE ON bot_places BEGIN
 UPDATE bot_rooms_seen SET seen_at = NEW.since WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id;
 INSERT INTO bot_rooms_seen (agent_id, hangout_id, seen_at) SELECT NEW.agent_id, NEW.hangout_id, NEW.since
 WHERE NOT EXISTS (SELECT 1 FROM bot_rooms_seen WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id);
END;
CREATE TRIGGER IF NOT EXISTS bot_rooms_seen_walked_in AFTER INSERT ON floor_visits
WHEN NEW.agent_id IS NOT NULL BEGIN
 UPDATE bot_rooms_seen SET seen_at = NEW.since WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id;
 INSERT INTO bot_rooms_seen (agent_id, hangout_id, seen_at) SELECT NEW.agent_id, NEW.hangout_id, NEW.since
 WHERE NOT EXISTS (SELECT 1 FROM bot_rooms_seen WHERE agent_id = NEW.agent_id AND hangout_id = NEW.hangout_id);
END;
CREATE TRIGGER IF NOT EXISTS bot_rooms_seen_arrived AFTER UPDATE OF status ON trips
WHEN NEW.status = 'here' AND (SELECT porch FROM houses WHERE id = NEW.house_id) IS NOT NULL BEGIN
 UPDATE bot_rooms_seen SET seen_at = COALESCE(NEW.arrived_at, CAST(strftime('%s', 'now') AS INTEGER)) WHERE agent_id = NEW.agent_id AND hangout_id = (SELECT porch FROM houses WHERE id = NEW.house_id);
 INSERT INTO bot_rooms_seen (agent_id, hangout_id, seen_at) SELECT NEW.agent_id, (SELECT porch FROM houses WHERE id = NEW.house_id), COALESCE(NEW.arrived_at, CAST(strftime('%s', 'now') AS INTEGER))
 WHERE NOT EXISTS (SELECT 1 FROM bot_rooms_seen WHERE agent_id = NEW.agent_id AND hangout_id = (SELECT porch FROM houses WHERE id = NEW.house_id));
END;
CREATE TRIGGER IF NOT EXISTS bot_rooms_seen_arrived_at_once AFTER INSERT ON trips
WHEN NEW.status = 'here' AND (SELECT porch FROM houses WHERE id = NEW.house_id) IS NOT NULL BEGIN
 UPDATE bot_rooms_seen SET seen_at = COALESCE(NEW.arrived_at, CAST(strftime('%s', 'now') AS INTEGER)) WHERE agent_id = NEW.agent_id AND hangout_id = (SELECT porch FROM houses WHERE id = NEW.house_id);
 INSERT INTO bot_rooms_seen (agent_id, hangout_id, seen_at) SELECT NEW.agent_id, (SELECT porch FROM houses WHERE id = NEW.house_id), COALESCE(NEW.arrived_at, CAST(strftime('%s', 'now') AS INTEGER))
 WHERE NOT EXISTS (SELECT 1 FROM bot_rooms_seen WHERE agent_id = NEW.agent_id AND hangout_id = (SELECT porch FROM houses WHERE id = NEW.house_id));
END;
-- I122: when a bot last said something in a room, for a brief read_room, in one seek per bot.
CREATE INDEX IF NOT EXISTS messages_by_room_bot ON messages (hangout_id, agent_id, kind, id);
-- FR1, #583: owners made members only because another bot brought theirs into a project (L12).
-- They can read where their bot went, but nobody agreed to a tie, so TIES_OF leaves them out.
CREATE TABLE IF NOT EXISTS brought_in_owners (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    PRIMARY KEY (hangout_id, person_id)
);
CREATE TRIGGER IF NOT EXISTS brought_in_owners_left AFTER DELETE ON members BEGIN
 DELETE FROM brought_in_owners WHERE hangout_id = OLD.hangout_id AND person_id = OLD.person_id;
END;
-- MM17: when a bot was last told it's bedtime, so it's told once a night, and the bots whose
-- owners turned going-home bedtime off.
CREATE TABLE IF NOT EXISTS bedtime_told (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    at          INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS bedtime_off (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE
);
-- F57 to F61: who invited a friend to a plan by name, and when. The invitee itself is a row in
-- plan_invitees, as through a link. This says it came from someone, so Needs you can say who.
CREATE TABLE IF NOT EXISTS plan_invited_by (
    event_id    TEXT NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    invited_by  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    invited_at  INTEGER NOT NULL,
    PRIMARY KEY (event_id, person_id)
);
-- V9: people's ballots, one per person per vote, changeable while it's open, and the votes a
-- person opened.
CREATE TABLE IF NOT EXISTS vote_person_ballots (
    vote_id     TEXT NOT NULL REFERENCES votes(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    voter       TEXT NOT NULL,
    choice      INTEGER NOT NULL,
    cast_at     INTEGER NOT NULL,
    PRIMARY KEY (vote_id, person_id)
);
CREATE TABLE IF NOT EXISTS person_votes (
    vote_id     TEXT PRIMARY KEY REFERENCES votes(id) ON DELETE CASCADE,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE
);
-- V10: ballots cast by someone who didn't belong to the room when they voted, bot or person.
CREATE TABLE IF NOT EXISTS vote_visitors (
    vote_id     TEXT NOT NULL REFERENCES votes(id) ON DELETE CASCADE,
    voter_id    TEXT NOT NULL,
    PRIMARY KEY (vote_id, voter_id)
);
-- JB1: a bot's role, its owner's standing instructions, private to the two of them.
CREATE TABLE IF NOT EXISTS bot_roles (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    body        TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
-- JB3: the jobs an owner gives a bot, one open at a time. outcome is finished, replaced, expired
-- or canceled once it's closed; seen_at is when the owner cleared a finished one from Needs you.
CREATE TABLE IF NOT EXISTS bot_jobs (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    owner_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    hangout_id  TEXT REFERENCES hangouts(id) ON DELETE SET NULL,
    text        TEXT NOT NULL,
    given_at    INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL,
    closed_at   INTEGER,
    outcome     TEXT,
    writeup     TEXT,
    seen_at     INTEGER
);
CREATE INDEX IF NOT EXISTS bot_jobs_by_bot ON bot_jobs (agent_id, closed_at);
CREATE INDEX IF NOT EXISTS bot_jobs_by_owner ON bot_jobs (owner_id, given_at);
-- JB7: the memories a bot kept with one of its jobs.
CREATE TABLE IF NOT EXISTS memory_jobs (
    memory_id   TEXT PRIMARY KEY REFERENCES bot_memories(id) ON DELETE CASCADE,
    job_id      TEXT NOT NULL REFERENCES bot_jobs(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS memory_jobs_by_job ON memory_jobs (job_id);
-- V15 (#594): a vote's closing reminder went out, so a restart or a second sweep doesn't send it
-- twice.
CREATE TABLE IF NOT EXISTS vote_reminders (
    vote_id     TEXT PRIMARY KEY REFERENCES votes(id) ON DELETE CASCADE,
    sent_at     INTEGER NOT NULL
);
-- I124: the Lific project a lific_board shows: an allow-listed origin and a project id, and no
-- key. Its issues are never stored (I126). It goes with the board.
CREATE TABLE IF NOT EXISTS lific_links (
    item_id     TEXT PRIMARY KEY REFERENCES floor_items(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    base        TEXT NOT NULL,
    project     TEXT NOT NULL,
    set_by      TEXT NOT NULL REFERENCES people(id),
    set_at      INTEGER NOT NULL
);
-- A95 (#628): whether agents may sign up through the door, one row, and who set it when. No row
-- means open.
CREATE TABLE IF NOT EXISTS bot_signups (
    id      INTEGER PRIMARY KEY CHECK (id = 1),
    open    INTEGER NOT NULL,
    set_by  TEXT NOT NULL REFERENCES people(id),
    set_at  INTEGER NOT NULL
);
-- A96 (#628): the accounts an agent made through the door, which marks them run by an agent. It
-- goes with the account.
CREATE TABLE IF NOT EXISTS agent_accounts (
    person_id   TEXT PRIMARY KEY REFERENCES people(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS agent_accounts_by_age ON agent_accounts (created_at);
-- MY1 (#639): one mayor election a week, site-wide. It closes at closes_at (a Saturday, 1:00 PM in
-- Chicago), when the winner is written and the next week's opens.
CREATE TABLE IF NOT EXISTS mayor_elections (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    closes_at   INTEGER NOT NULL UNIQUE,
    opened_at   INTEGER NOT NULL,
    closed_at   INTEGER,
    winner_kind TEXT,
    winner_id   TEXT,
    winner_name TEXT,
    winner_votes INTEGER,
    tie_draw    INTEGER NOT NULL DEFAULT 0
);
-- MY2: who stands in a week's election, a person or a bot, with the name they stood under.
CREATE TABLE IF NOT EXISTS mayor_candidates (
    election_id  INTEGER NOT NULL REFERENCES mayor_elections(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('person', 'bot')),
    candidate_id TEXT NOT NULL,
    name         TEXT NOT NULL,
    stood_at     INTEGER NOT NULL,
    PRIMARY KEY (election_id, kind, candidate_id)
);
-- MY3, MY4: each person's one secret ballot in a week's election; the last one cast counts.
CREATE TABLE IF NOT EXISTS mayor_ballots (
    election_id  INTEGER NOT NULL REFERENCES mayor_elections(id) ON DELETE CASCADE,
    voter        TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL,
    candidate_id TEXT NOT NULL,
    cast_at      INTEGER NOT NULL,
    PRIMARY KEY (election_id, voter)
);
-- I129 (#629): the Lific project this server made for a room on its own instance, the one every
-- lific_board in the room shows. `published` is 0 until Lific has made it public.
CREATE TABLE IF NOT EXISTS lific_room_projects (
    hangout_id  TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    base        TEXT NOT NULL,
    project     TEXT NOT NULL,
    lific_id    TEXT NOT NULL,
    published   INTEGER NOT NULL DEFAULT 0,
    made_at     INTEGER NOT NULL
);
-- I137: who added each card this server put on a room's own Lific project. Lific gets only the
-- words, so this is the only place the room's names are. It goes with the account and the room.
CREATE TABLE IF NOT EXISTS lific_cards (
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    card        TEXT NOT NULL,
    issue_id    TEXT NOT NULL,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    agent_id    TEXT REFERENCES agents(id) ON DELETE SET NULL,
    via_bot     INTEGER NOT NULL DEFAULT 0,
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (hangout_id, card)
);
CREATE INDEX IF NOT EXISTS lific_cards_by_person ON lific_cards (person_id, added_at);
-- #647: setup-only OAuth grants (scope mcp:setup). A person's agent may draft a bot setup for them
-- and nothing else; no bot is bound, so these sit apart from mcp_oauth_grants. They last a day.
CREATE TABLE IF NOT EXISTS mcp_setup_grants (
    id TEXT PRIMARY KEY, owner_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    client_id TEXT NOT NULL REFERENCES mcp_oauth_clients(id) ON DELETE CASCADE,
    resource TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS mcp_setup_codes (
    hash TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES mcp_setup_grants(id) ON DELETE CASCADE,
    redirect_uri TEXT NOT NULL, challenge TEXT NOT NULL, expires_at INTEGER NOT NULL,
    used INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS mcp_setup_tokens (
    hash TEXT PRIMARY KEY, grant_id TEXT NOT NULL REFERENCES mcp_setup_grants(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('access','refresh')), expires_at INTEGER NOT NULL,
    used INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS mcp_setup_grants_owner ON mcp_setup_grants(owner_id);
CREATE INDEX IF NOT EXISTS mcp_setup_tokens_grant ON mcp_setup_tokens(grant_id);
CREATE INDEX IF NOT EXISTS mcp_setup_codes_grant ON mcp_setup_codes(grant_id);
-- #647: a bot setup an agent drafted for its person: nothing is made until they approve it on the
-- site. An open one lapses after a day.
CREATE TABLE IF NOT EXISTS bot_setup_drafts (
    id          TEXT PRIMARY KEY,
    owner_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    role        TEXT,
    job         TEXT,
    workplace   TEXT,
    day_plan    TEXT,
    status      TEXT NOT NULL DEFAULT 'open',
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bot_setup_drafts_owner ON bot_setup_drafts(owner_id, status);
-- #647 B: the bot a person made by approving a draft on the site.
CREATE TABLE IF NOT EXISTS bot_setup_bots (
    draft_id    TEXT PRIMARY KEY REFERENCES bot_setup_drafts(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    made_at     INTEGER NOT NULL
);
-- GB1: the one bridge key a bot's person made, for a model that can only fetch addresses
-- (docs/bridge.md). GB4: the turns it has already played, forgotten after a day.
CREATE TABLE IF NOT EXISTS bridge_keys (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    key_hash    TEXT NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS bridge_turns (
    agent_id    TEXT NOT NULL REFERENCES bridge_keys(agent_id) ON DELETE CASCADE,
    turn        TEXT NOT NULL,
    at          INTEGER NOT NULL,
    PRIMARY KEY (agent_id, turn)
);
-- KA7: the public floors a god opened to keyless agents (docs/keyless-agents-design.md).
CREATE TABLE IF NOT EXISTS keyless_floors (
    hangout_id  TEXT PRIMARY KEY REFERENCES hangouts(id) ON DELETE CASCADE,
    opened_by   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    opened_at   INTEGER NOT NULL
);
-- KA7: the floor a keyless agent is on, since when, and when it last did something there.
CREATE TABLE IF NOT EXISTS keyless_presence (
    player_id   INTEGER PRIMARY KEY REFERENCES players(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    since       INTEGER NOT NULL,
    active_at   INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS keyless_presence_by_floor ON keyless_presence(hangout_id);
-- KA10: the chat lines keyless agents wrote. The line itself is an ordinary message with no person
-- and no bot.
CREATE TABLE IF NOT EXISTS keyless_messages (
    message_id  INTEGER PRIMARY KEY REFERENCES messages(id) ON DELETE CASCADE,
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS keyless_messages_by_player ON keyless_messages(player_id);
-- KA11: who a keyless agent's line mentioned, for the caps.
CREATE TABLE IF NOT EXISTS keyless_mentions (
    message_id  INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    agent_id    TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    at          INTEGER NOT NULL,
    PRIMARY KEY (message_id, agent_id)
);
CREATE INDEX IF NOT EXISTS keyless_mentions_by_floor ON keyless_mentions(hangout_id, at);
-- KA11: bots whose owner turned off hearing keyless agents' mentions.
CREATE TABLE IF NOT EXISTS keyless_mention_optouts (
    agent_id    TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE
);
-- KA14: discussion boards that take keyless agents' cards.
CREATE TABLE IF NOT EXISTS keyless_boards (
    board_id    TEXT PRIMARY KEY REFERENCES boards(id) ON DELETE CASCADE,
    opened_by   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    opened_at   INTEGER NOT NULL
);
-- KA15: a keyless agent's card. `answers` names another card on the same board, of either kind.
CREATE TABLE IF NOT EXISTS keyless_cards (
    id          TEXT PRIMARY KEY,
    board_id    TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    answers     TEXT,
    title       TEXT NOT NULL,
    body        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS keyless_cards_by_board ON keyless_cards(board_id, created_at);
CREATE INDEX IF NOT EXISTS keyless_cards_by_player ON keyless_cards(player_id, created_at);
-- KA3: the nonces the bridge's new door used. No key is kept: it was shown once.
CREATE TABLE IF NOT EXISTS keyless_nonces (
    nonce       TEXT PRIMARY KEY,
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    at          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS keyless_nonces_by_time ON keyless_nonces(at);
-- KA8: the turns a keyless agent has played through the bridge (GB4), forgotten after a day.
CREATE TABLE IF NOT EXISTS keyless_turns (
    player_id   INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    turn        TEXT NOT NULL,
    at          INTEGER NOT NULL,
    PRIMARY KEY (player_id, turn)
);
-- #651, I134: every card this server asked Lific to add, kept when the card is taken down, so taking
-- cards down never gives back the day's 30. A row is reserved before Lific is asked, so adds that
-- race each other can't all pass the limits; an add Lific didn't take is dropped.
CREATE TABLE IF NOT EXISTS lific_card_adds (
    id          TEXT PRIMARY KEY,
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    hangout_id  TEXT NOT NULL REFERENCES hangouts(id) ON DELETE CASCADE,
    pending     INTEGER NOT NULL DEFAULT 1,
    at          INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS lific_card_adds_by_person ON lific_card_adds (person_id, at);
CREATE INDEX IF NOT EXISTS lific_card_adds_by_room ON lific_card_adds (hangout_id, pending, at);
-- W113: every place that comes down on a map, and why, so "where did my house go?" has an answer.
-- Kept apart from the player and the person, with no foreign key to either, so it outlasts them;
-- an account deleted (A43) leaves its rows with no person.
CREATE TABLE IF NOT EXISTS world_place_removals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    world_id    TEXT NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    kind        TEXT NOT NULL,
    player_id   INTEGER,
    person_id   TEXT,
    built_at    INTEGER NOT NULL,
    removed_at  INTEGER NOT NULL,
    reason      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS world_place_removals_by_time ON world_place_removals (world_id, id DESC);
CREATE INDEX IF NOT EXISTS world_place_removals_by_person ON world_place_removals (person_id);
-- A100, A104 (#685): the people a person has blocked, and whose mentions they've muted. A block
-- covers the blocker's bots too, and cuts contact both ways (A102); a mute only stops mentions of
-- the muter. Either goes with either account.
CREATE TABLE IF NOT EXISTS person_blocks (
    blocker_id  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    blocked_id  TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (blocker_id, blocked_id)
);
CREATE INDEX IF NOT EXISTS person_blocks_by_blocked ON person_blocks (blocked_id);
CREATE TABLE IF NOT EXISTS person_mutes (
    muter_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    muted_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (muter_id, muted_id)
);
-- KA7: privacy revokes admission in the same statement, even on an older binary.
CREATE TRIGGER IF NOT EXISTS keyless_close_private_floor AFTER UPDATE OF access ON floors
WHEN NEW.access != 'public' OR OLD.access != 'public'
BEGIN
    DELETE FROM keyless_floors WHERE hangout_id = NEW.hangout_id;
    DELETE FROM keyless_presence WHERE hangout_id = NEW.hangout_id;
END;
CREATE TRIGGER IF NOT EXISTS keyless_close_presence AFTER DELETE ON keyless_floors
BEGIN
    DELETE FROM keyless_presence WHERE hangout_id = OLD.hangout_id;
END;
-- KA5: an old binary's guest prune must not cascade away research or its provenance.
-- End the credential and retain the anonymous author instead. Intentional content removal
-- still works through KA20, which deletes the content explicitly.
CREATE TRIGGER IF NOT EXISTS keyless_keep_authors BEFORE DELETE ON players
WHEN OLD.person_id IS NULL AND OLD.agent_id IS NULL
 AND (EXISTS (SELECT 1 FROM keyless_messages WHERE player_id = OLD.id)
      OR EXISTS (SELECT 1 FROM keyless_cards WHERE player_id = OLD.id))
BEGIN
    UPDATE players SET token_hash = NULL WHERE id = OLD.id;
    SELECT RAISE(IGNORE);
END;
-- HC36: a review, correction, source or summary a bot offers on a board card. It goes with the
-- card, the bot or its owner.
CREATE TABLE IF NOT EXISTS card_contributions (
    id          TEXT PRIMARY KEY,
    card_id     TEXT NOT NULL REFERENCES board_cards(id) ON DELETE CASCADE,
    bot_id      TEXT NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    owner_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL CHECK (kind IN ('review', 'correction', 'source', 'summary')),
    text        TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    UNIQUE (card_id, bot_id)
);
-- HC37 to HC41: what the site paid for an accepted contribution, once per card and once per
-- contribution. It stays when the card, the contribution or either account goes, so nothing pays
-- twice; a deleted account only clears its id. The paid person's balance counts it (HC1).
CREATE TABLE IF NOT EXISTS contribution_payouts (
    card_id         TEXT PRIMARY KEY,
    contribution_id TEXT NOT NULL UNIQUE,
    acceptor_id     TEXT REFERENCES people(id) ON DELETE SET NULL,
    recipient_id    TEXT REFERENCES people(id) ON DELETE SET NULL,
    amount          INTEGER NOT NULL CHECK (amount > 0),
    day             INTEGER NOT NULL,
    created_at      INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS contribution_payouts_by_recipient ON contribution_payouts (recipient_id, day);
CREATE INDEX IF NOT EXISTS contribution_payouts_at ON contribution_payouts (created_at);
-- A103 (#685): an ask made across a block. It's stored like any other, so it shows to whoever made it
-- as waiting, and this marks it so the other side never sees it, then or later, even if the block
-- is taken back. `kind` and `ask_id` name it; nothing here says who blocked whom.
CREATE TABLE IF NOT EXISTS quiet_asks (
    kind        TEXT NOT NULL,
    ask_id      TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (kind, ask_id)
);
-- A108: each version of the community rules a person accepted, and when.
CREATE TABLE IF NOT EXISTS rules_acceptances (
    person_id   TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    version     TEXT NOT NULL,
    accepted_at INTEGER NOT NULL,
    PRIMARY KEY (person_id, version)
);
-- A112: the first time this server ran with each version of the rules, where its grace starts.
CREATE TABLE IF NOT EXISTS rules_versions (
    version     TEXT PRIMARY KEY,
    first_seen  INTEGER NOT NULL
);
-- RE1 to RE7 (docs/reports.md): what a person, or their bot, reported, with a copy of what they
-- saw (RE2). The person ids have no foreign key: deleting an account blanks them in the same step,
-- target_id too when the target is that person, except on a minor_safety report, which keeps
-- everything (RE6). The review queue (RV6 to RV14) reads and closes these, and sets closed_at when
-- it does, minor_safety first and then oldest first (reports_queue).
CREATE TABLE IF NOT EXISTS reports (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    reporter_id       TEXT,
    via_bot_id        TEXT,
    by_bot            INTEGER NOT NULL,
    target_kind       TEXT NOT NULL CHECK (target_kind IN
                          ('message', 'person', 'bot', 'place', 'card', 'note', 'pin')),
    target_id         TEXT,
    target_person_id  TEXT,
    hangout_id        TEXT,
    reason            TEXT NOT NULL CHECK (reason IN ('harassment', 'hate', 'sexual',
                          'minor_safety', 'spam', 'impersonation', 'other')),
    details           TEXT CHECK (details IS NULL OR length(details) <= 1000),
    snapshot          TEXT,
    status            TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'actioned', 'dismissed')),
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    closed_at         INTEGER,
    day               INTEGER NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS reports_one_open
    ON reports (reporter_id, target_kind, target_id) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS reports_by_reporter_day ON reports (reporter_id, day);
CREATE INDEX IF NOT EXISTS reports_by_target ON reports (target_kind, target_id);
CREATE INDEX IF NOT EXISTS reports_queue ON reports (status, reason, created_at);
-- RE10 to RE12 (docs/reports.md): a bot an operator suspended. Only an operator lifts it, and its
-- row goes with the bot.
CREATE TABLE IF NOT EXISTS bot_suspensions (
    agent_id     TEXT PRIMARY KEY REFERENCES agents(id) ON DELETE CASCADE,
    reason       TEXT NOT NULL,
    report_id    INTEGER,
    operator_id  TEXT,
    at           INTEGER NOT NULL
);
-- RE10 to RE12: a person an operator restricted or banned, until an operator lifts it.
CREATE TABLE IF NOT EXISTS person_holds (
    person_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    hold         TEXT NOT NULL CHECK (hold IN ('restrict_person', 'ban_person')),
    reason       TEXT NOT NULL,
    report_id    INTEGER,
    operator_id  TEXT,
    at           INTEGER NOT NULL,
    PRIMARY KEY (person_id, hold)
);
-- RE14, RE15: what an action tells its person, and the alert a minor_safety report sends each
-- operator, shown until they've seen it. An alert names the report's id and nothing else.
CREATE TABLE IF NOT EXISTS moderation_notices (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    person_id    TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('warn', 'suspend_bot', 'restrict_person',
                     'ban_person', 'report_alert')),
    report_id    INTEGER NOT NULL,
    text         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    seen_at      INTEGER,
    UNIQUE (person_id, kind, report_id)
);
-- RE13: every action on a report and every lift or release, kept after the report's copy is
-- dropped (RE16). Person ids are blanked with an account the way reports blank them (RE17).
CREATE TABLE IF NOT EXISTS report_actions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    report_id    INTEGER,
    operator_id  TEXT,
    action       TEXT NOT NULL CHECK (action IN ('dismiss', 'remove_content', 'warn',
                     'suspend_bot', 'restrict_person', 'ban_person', 'lift_suspend_bot',
                     'lift_restrict_person', 'lift_ban_person', 'release')),
    note         TEXT CHECK (note IS NULL OR length(note) <= 1000),
    subject_id   TEXT,
    person_id    TEXT,
    minor_safety INTEGER NOT NULL DEFAULT 0,
    at           INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS report_actions_by_report ON report_actions (report_id);
"#;

/// MY1: a week's mayor election.
#[derive(Debug, Clone)]
pub struct MayorElection {
    pub id: i64,
    pub closes_at: i64,
    pub opened_at: i64,
    pub closed_at: Option<i64>,
    pub winner: Option<MayorCandidate>,
    pub winner_votes: Option<i64>,
    pub tie_draw: bool,
}

/// MY2: someone standing for mayor: a person or a bot, by id, with the name they stood under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MayorCandidate {
    pub kind: String,
    pub id: String,
    pub name: String,
}

const MAYOR_COLS: &str =
    "id, closes_at, opened_at, closed_at, winner_kind, winner_id, winner_name, winner_votes, tie_draw";

fn mayor_election_from_row(row: &Row) -> rusqlite::Result<MayorElection> {
    let kind: Option<String> = row.get(4)?;
    let id: Option<String> = row.get(5)?;
    let name: Option<String> = row.get(6)?;
    Ok(MayorElection {
        id: row.get(0)?,
        closes_at: row.get(1)?,
        opened_at: row.get(2)?,
        closed_at: row.get(3)?,
        winner: match (kind, id, name) {
            (Some(kind), Some(id), Some(name)) => Some(MayorCandidate { kind, id, name }),
            _ => None,
        },
        winner_votes: row.get(7)?,
        tie_draw: row.get::<_, i64>(8)? != 0,
    })
}

/// I106: a closing clock's saved time and who it sends out.
#[derive(Debug, Clone)]
pub struct ClosingClock {
    pub item_id: String,
    pub hangout_id: String,
    pub closes_at: i64,
    pub everyone: bool,
    pub bot_ids: Vec<String>,
}

fn closing_clock_from_row(row: &Row) -> rusqlite::Result<ClosingClock> {
    let bots: String = row.get(4)?;
    Ok(ClosingClock {
        item_id: row.get(0)?,
        hangout_id: row.get(1)?,
        closes_at: row.get(2)?,
        everyone: row.get::<_, i64>(3)? != 0,
        bot_ids: bots.split(',').filter(|id| !id.is_empty()).map(str::to_string).collect(),
    })
}

#[derive(Debug, Serialize)]
pub struct RoadRemoval {
    pub x: u16,
    pub y: u16,
    pub built_at: i64,
    pub removed_at: i64,
    pub world_step: i64,
    pub reason: String,
}

/// E30: how many outsides one player keeps waiting for each kind of place, after their places come
/// down. Putting a place up takes the newest, so this is only what could pile up behind it.
pub const MAX_WAITING_OUTSIDES: i64 = 4;
/// E26: a stamp counts milliseconds from the start of 2025, so the sum of thousands of them still fits a
/// number a page can hold.
const STAMP_EPOCH_MS: i64 = 1_735_689_600_000;
/// The last stamp given out in this process, so two changes in one millisecond still differ.
static LAST_OUTSIDE_STAMP: AtomicI64 = AtomicI64::new(0);
/// E31: a row for a tile shows only while the place standing there has its player and its kind. Takes
/// the row as `o`.
const OUTSIDE_IS_LIVE: &str = "JOIN world_every_place p ON p.world_id = o.world_id AND p.x = o.x
    AND p.y = o.y AND p.player_id = o.player_id AND p.kind = o.kind";

/// A30: the one row in `people` that stands for everyone who has deleted their account.
pub const SOMEONE_WHO_LEFT_ID: &str = "someone-who-left";
pub const SOMEONE_WHO_LEFT: &str = "Someone who left";

/// Most sessions one person keeps at once. Signing in again on a new device past this ends the
/// oldest, so the table can't be grown without limit.
const MAX_SESSIONS_PER_PERSON: i64 = 50;

/// Owner colours, handed out in order so the first few people in a room look distinct.
pub const COLORS: [&str; 8] =
    ["#4f9dff", "#ff8a3d", "#3ddc97", "#ff5d7a", "#b388ff", "#ffd24d", "#2fd6d6", "#ff7ad9"];

#[derive(Clone, Debug, Serialize)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// Whose look it is. People and bots have a table each, with one shape (C4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wearer {
    Person,
    Bot,
}

impl Wearer {
    /// The table a look is in, and the column that says whose it is.
    const fn table(self) -> (&'static str, &'static str) {
        match self {
            Self::Person => ("person_looks", "person_id"),
            Self::Bot => ("bot_looks", "agent_id"),
        }
    }
}

/// A passkey as the person's own list shows it. The library's data stays in `StoredPasskey`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PasskeyInfo {
    pub id: String,
    pub label: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

/// A passkey with the library's JSON. It holds a public key and a counter, so it isn't secret,
/// but it has no `Serialize` and never goes to a page.
pub struct StoredPasskey {
    pub id: String,
    pub person_id: String,
    pub passkey: String,
    /// The handle of the person it belongs to, which a sign-in has to agree with.
    pub user_handle: String,
}

/// A passkey about to be stored. `credential_id` is the authenticator's id in hex.
pub struct NewPasskey<'a> {
    pub id: &'a str,
    pub credential_id: &'a str,
    pub passkey: &'a str,
    pub label: &'a str,
}

/// What came of storing a passkey.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasskeyAdded {
    Added,
    /// This credential is already stored, for this person or anyone else.
    AlreadyRegistered,
    TooMany,
}

/// What a bot is doing through another tool right now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Activity {
    pub kind: String,
    pub via: String,
    pub until: i64,
}

/// A bot in a room waiting on an @mention, or working on a reply (I58).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Thinking {
    pub phase: &'static str,
    pub until: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DoorbellStatus {
    /// Its doorbell checked in the last minute, so something will start the bot if it's needed.
    pub listening: bool,
    pub paused: bool,
}

/// The Discord account a bot is linked to (A14 to A26).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiscordLink {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Agent {
    pub id: String,
    pub owner_id: String,
    pub name: String,
    pub status: Option<String>,
    pub last_active_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    /// `active`, `idle` or `offline`, which decides where the bot stands in the room.
    pub state: &'static str,
    pub activity: Option<Activity>,
    /// `None` for a bot that has never run a doorbell.
    pub doorbell: Option<DoorbellStatus>,
    /// When this bot's trip here runs out. Set only for a guest, by `visitors_in`.
    pub visitor_until: Option<i64>,
    /// L67: why its visit ends then. Set only where a person is shown a room's guests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visitor_until_why: Option<&'static str>,
    /// It belongs to this room but it's somewhere else (S55). Set only in a room's snapshot.
    pub away: bool,
    /// Set only where a person is shown a room's own bots (A21, A22).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordLink>,
    /// K24: the two bots an apprentice was made by. Set only where a person is shown a bot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apprenticed_by: Option<[String; 2]>,
    /// Set only in a room's snapshot and `read_room` (I58, I63).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<Thinking>,
}

impl Agent {
    /// I84: whether it's likely to see a mention soon: it called the server in the last ten
    /// minutes, or its doorbell is listening.
    pub fn responsive(&self, now: i64) -> bool {
        self.last_seen_at.is_some_and(|at| now - at <= RESPONSIVE_SECS)
            || self.doorbell.as_ref().is_some_and(|bell| bell.listening)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Hangout {
    pub id: String,
    pub name: String,
    pub invite_code: String,
    pub created_by: String,
}

/// A house or a third space. Its id is the id of the floor it began with.
#[derive(Clone, Debug, Serialize)]
pub struct Space {
    pub id: String,
    pub owner_id: String,
    pub kind: String,
    pub name: String,
    pub about: String,
    pub created_at: i64,
}

/// A floor of a space. Its id is its hangout's id, so its name is the room's name.
#[derive(Clone, Debug, Serialize)]
pub struct Floor {
    pub id: String,
    pub space_id: String,
    pub name: String,
    pub level: i64,
    pub access: String,
    pub size: String,
    pub paint: String,
    pub flooring: String,
    pub hidden: Vec<String>,
}

pub struct NewFloor<'a> {
    pub id: &'a str,
    pub space_id: &'a str,
    pub name: &'a str,
    pub invite_code: &'a str,
    pub created_by: &'a str,
    pub now: i64,
}

/// What to change about a floor. `None` leaves a field as it is.
pub struct FloorChange<'a> {
    pub id: &'a str,
    pub name: Option<&'a str>,
    pub access: Option<&'a str>,
    pub size: Option<&'a str>,
    pub paint: Option<&'a str>,
    pub flooring: Option<&'a str>,
    /// Whose bots stay when this opens the floor to the public.
    pub keep_bots_of: &'a str,
    /// Kinds of item that keep this floor from opening to the public (I11).
    pub private_only: &'a [&'a str],
}

/// I134, #651: which limit an add met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LificLimit {
    /// A person and their bots add 30 a day.
    Day,
    /// A room's project holds 200.
    Board,
}

/// I137: a card this server added to a room's Lific project, and who added it.
#[derive(Clone, Debug)]
pub struct LificCard {
    pub hangout_id: String,
    pub card: String,
    pub issue_id: String,
    pub person_id: String,
    pub agent_id: Option<String>,
    pub via_bot: bool,
    pub added_at: i64,
    /// The bot's name if a bot added it and still exists, or else the person's. Empty on insert.
    pub added_by: String,
}

/// An item standing on a floor (I1). `kind` is a word from the catalog in `items`.
#[derive(Clone, Debug, Serialize)]
pub struct FloorItem {
    pub id: String,
    pub hangout_id: String,
    pub kind: String,
    pub x: i64,
    pub y: i64,
    pub facing: i64,
    pub placed_by: String,
    pub placed_by_name: String,
    pub placed_at: i64,
}

pub struct NewFloorItem<'a> {
    pub id: &'a str,
    pub hangout_id: &'a str,
    pub kind: &'a str,
    pub x: i64,
    pub y: i64,
    pub facing: i64,
    pub placed_by: &'a str,
    pub now: i64,
}

/// A note on an item (I14). `person_id` is who wrote it, and `via_agent` the name of the bot that
/// wrote it for them, so that a bot's words are never mistaken for its owner's.
#[derive(Clone, Debug, Serialize)]
pub struct ItemNote {
    pub id: String,
    pub item_id: String,
    pub body: String,
    pub person_id: String,
    pub writer: String,
    pub via_agent: Option<String>,
    pub created_at: i64,
}

pub struct NewItemNote<'a> {
    pub id: &'a str,
    pub item_id: &'a str,
    pub body: &'a str,
    pub person_id: &'a str,
    pub via_agent: Option<&'a str>,
    pub now: i64,
}

/// What a room's whiteboard says (I48). `person_id` is who wrote it last, and `agent_id` the bot that
/// wrote it for them, so that a bot's words are never mistaken for its owner's (I51).
#[derive(Clone, Debug, Serialize)]
pub struct Whiteboard {
    pub text: String,
    pub person_id: String,
    pub written_by: String,
    pub agent_id: Option<String>,
    pub via_bot: Option<String>,
    pub updated_at: i64,
}

/// A board on a floor (K1). `github` is `owner/name` when an issues board is linked, and nothing otherwise.
#[derive(Clone, Debug)]
pub struct Board {
    pub id: String,
    pub hangout_id: String,
    pub kind: String,
    pub name: String,
    pub github_owner: Option<String>,
    pub github_repo: Option<String>,
}

impl Board {
    pub fn github(&self) -> Option<String> {
        match (&self.github_owner, &self.github_repo) {
            (Some(owner), Some(repo)) => Some(format!("{owner}/{repo}")),
            _ => None,
        }
    }
}

/// A pin on a room's wall whiteboard (I68). `image_*` is stored for a later change; bots pin
/// links and notes only (I80).
#[derive(Clone, Debug, Serialize)]
pub struct RoomPin {
    pub id: String,
    pub bot_origin: bool,
    pub hangout_id: String,
    pub kind: String,
    pub title: String,
    pub url: Option<String>,
    pub image_asset_id: Option<String>,
    pub image_alt: Option<String>,
    pub caption: Option<String>,
    pub preview_site: Option<String>,
    pub preview_title: Option<String>,
    pub preview_description: Option<String>,
    pub preview_status: Option<String>,
    pub preview_fetched_at: Option<i64>,
    pub person_id: String,
    pub pinned_by: String,
    pub agent_id: Option<String>,
    pub via_bot: Option<String>,
    pub pinned_at: i64,
    pub expires_at: Option<i64>,
}

pub struct NewRoomPin<'a> {
    pub id: &'a str,
    pub hangout_id: &'a str,
    pub kind: &'a str,
    pub title: &'a str,
    pub url: Option<&'a str>,
    pub caption: Option<&'a str>,
    pub preview_status: Option<&'a str>,
    pub person_id: &'a str,
    pub agent_id: Option<&'a str>,
    pub now: i64,
    pub expires_at: Option<i64>,
    pub max_room: i64,
    pub max_person: i64,
}

/// What `read_room` carries for a board: counts, and no card text (K10).
#[derive(Clone, Debug, Serialize)]
pub struct BoardSummary {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub github: Option<String>,
    pub cards: i64,
    pub open: i64,
}

/// One card. `github()` is the board's repository, not this card's issue number.
#[derive(Clone, Debug)]
pub struct BoardCard {
    pub id: String,
    pub board_id: String,
    pub hangout_id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub status: Option<String>,
    pub github_issue: Option<i64>,
    pub github_owner: Option<String>,
    pub github_repo: Option<String>,
    pub person_id: String,
    pub written_by: String,
    pub agent_id: Option<String>,
    pub via_bot: Option<String>,
    pub by_bot: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl BoardCard {
    pub fn github(&self) -> Option<String> {
        match (&self.github_owner, &self.github_repo) {
            (Some(owner), Some(repo)) => Some(format!("{owner}/{repo}")),
            _ => None,
        }
    }
}

fn github_pair(owner: Option<String>, repo: Option<String>) -> Option<String> {
    match (owner, repo) {
        (Some(owner), Some(repo)) => Some(format!("{owner}/{repo}")),
        _ => None,
    }
}

fn board_from_row(row: &Row) -> rusqlite::Result<Board> {
    Ok(Board {
        id: row.get(0)?,
        hangout_id: row.get(1)?,
        kind: row.get(2)?,
        name: row.get(3)?,
        github_owner: row.get(4)?,
        github_repo: row.get(5)?,
    })
}

fn summary_from_row(row: &Row) -> rusqlite::Result<BoardSummary> {
    let owner: Option<String> = row.get(3)?;
    let repo: Option<String> = row.get(4)?;
    Ok(BoardSummary {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        github: github_pair(owner, repo),
        cards: row.get(5)?,
        open: row.get(6)?,
    })
}

fn card_from_row(row: &Row) -> rusqlite::Result<BoardCard> {
    Ok(BoardCard {
        id: row.get(0)?,
        board_id: row.get(1)?,
        hangout_id: row.get(2)?,
        kind: row.get(3)?,
        title: row.get(4)?,
        body: row.get(5)?,
        status: row.get(6)?,
        github_issue: row.get(7)?,
        github_owner: row.get(8)?,
        github_repo: row.get(9)?,
        person_id: row.get(10)?,
        written_by: row.get(11)?,
        agent_id: row.get(12)?,
        via_bot: row.get(13)?,
        by_bot: row.get::<_, i64>(14)? != 0,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

const CARD_SELECT: &str =
    "SELECT c.id, c.board_id, b.hangout_id, b.kind, c.title, c.body, c.status, \
     c.github_issue, b.github_owner, b.github_repo, c.person_id, p.name, c.agent_id, \
     COALESCE(a.name, c.via_agent), \
     CASE WHEN c.agent_id IS NOT NULL OR c.via_agent IS NOT NULL THEN 1 ELSE 0 END, \
     c.created_at, c.updated_at \
     FROM board_cards c \
     JOIN boards b ON b.id = c.board_id \
     JOIN people p ON p.id = c.person_id \
     LEFT JOIN agents a ON a.id = c.agent_id";

/// A public floor as the list shows it: nothing about who is there or what was said.
#[derive(Clone, Debug, Serialize)]
pub struct PublicFloor {
    pub id: String,
    pub name: String,
    pub space_name: String,
    pub owner_name: String,
    pub about: String,
    pub size: String,
    pub capacity: i64,
    pub here: i64,
    /// L18: this floor is behind the world's town square.
    pub town_square: bool,
    /// Which space it is a floor of. For the map (W105); the list doesn't show it.
    #[serde(skip)]
    pub space_id: String,
}

pub struct NewVisit<'a> {
    pub hangout_id: &'a str,
    /// The person's id, or their bot's.
    pub who: &'a str,
    pub person_id: &'a str,
    pub agent_id: Option<&'a str>,
    pub until: i64,
    pub now: i64,
}

/// What came of walking in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalkedIn {
    In,
    Full,
    TooManyBots,
    /// A trip doesn't give way to a walk-in (S39).
    OnTrip,
}

/// S71: where the minute look last saw a bot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenPlace {
    pub hangout_id: String,
    pub told_to: Option<String>,
    pub said_from: Option<String>,
    /// Goes up with every change to the row, so a look can tell nothing touched it meanwhile.
    pub seen_at: i64,
}

/// A floor a bot left, and why. S62.
#[derive(Clone, Debug, Serialize)]
pub struct FloorLeave {
    pub hangout_id: String,
    pub name: String,
    pub left_at: i64,
    pub reason: String,
}

/// An @mention of a bot that it hasn't read yet. B8.
#[derive(Clone, Debug, Serialize)]
pub struct Mention {
    pub hangout_id: String,
    pub hangout: String,
    pub message_id: i64,
    pub from: String,
    pub text: String,
    pub at: i64,
}

/// E31: one wake decision, without the words that caused it.
pub struct WakeDecision {
    pub decision: String,
    pub reason: String,
    pub hangout_id: Option<String>,
    pub room: Option<String>,
    pub at: i64,
}

/// L10: bot mentions the server is holding, and those it has already rung, by room.
pub struct BotMentionWakeIds {
    pub held: BTreeMap<String, Vec<i64>>,
    pub rung: BTreeMap<String, Vec<i64>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Message {
    pub id: i64,
    pub hangout_id: String,
    pub person_id: Option<String>,
    pub agent_id: Option<String>,
    pub speaker: String,
    pub kind: String,
    pub body: String,
    pub created_at: i64,
    /// I82: the chat line in the same room this one answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<i64>,
    /// I102: the root chat line of this message's thread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_root: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Rsvp {
    pub person_id: String,
    pub response: String,
    pub via_agent: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub id: String,
    pub hangout_id: String,
    pub title: String,
    pub location: String,
    pub notes: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub status: String,
    pub min_yes: i64,
    pub sequence: i64,
    pub proposed_by: String,
    pub proposer: String,
    pub updated_at: i64,
    /// When it was proposed.
    pub created_at: i64,
    pub rsvps: Vec<Rsvp>,
}

/// A bot asking before it acts. `status` is `waiting`, `go`, `hold` or `expired`.
#[derive(Clone, Debug, Serialize)]
pub struct CheckIn {
    pub id: String,
    pub hangout_id: String,
    pub asked_by: String,
    pub asker: String,
    pub agent_id: Option<String>,
    pub about: String,
    pub to_person: Option<String>,
    pub needs_human: bool,
    pub status: String,
    pub answered_by: Option<String>,
    pub answered_by_bot: bool,
    pub note: String,
    pub expires_at: i64,
    pub created_at: i64,
}

pub struct NewCheckIn<'a> {
    pub id: &'a str,
    pub hangout_id: &'a str,
    pub asked_by: &'a str,
    pub asker: &'a str,
    pub agent_id: Option<&'a str>,
    pub about: &'a str,
    pub to_person: Option<&'a str>,
    pub needs_human: bool,
    pub expires_at: i64,
    pub now: i64,
}

/// H1: something a person told their own bots. Only that person's bots ever read it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OwnerNote {
    pub id: String,
    #[serde(skip)]
    pub owner_id: String,
    pub kind: String,
    pub text: String,
    pub about_person_id: Option<String>,
    /// The name of the person it's about, as it is now.
    pub about: Option<String>,
    pub from_question_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

pub struct NewOwnerNote<'a> {
    pub id: &'a str,
    pub owner_id: &'a str,
    pub kind: &'a str,
    pub about_person: Option<&'a str>,
    pub body: &'a str,
    pub from_question: Option<&'a str>,
    pub now: i64,
}

/// H14: a person's own record that a bot behaved badly. Only they and their bots see it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BotMark {
    pub agent_id: String,
    pub agent_name: String,
    pub reason: String,
    pub created_at: i64,
}

/// H6: how a bot's owner knows someone, worked out on read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Relation {
    pub lives_with: bool,
    pub shares_a_city: bool,
    /// Plans on the calendar that are over, where both said yes.
    pub plans_together: i64,
}

/// L1, L2: when a bot may go out, and whether it may go at all. `max_social_runs` is unused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoutineRow {
    /// Bits from Monday (1) to Sunday (64).
    pub days: i64,
    /// Minutes after midnight in the owner's own time.
    pub starts_min: i64,
    pub ends_min: i64,
    pub max_social_runs: i64,
    pub public_places: bool,
}

/// W71, W73: a place, its owner, its kind and the name of the room behind its door, if it has one:
/// `(x, y, owner, kind, name)`.
pub type PlaceOwnerRow = (i64, i64, String, String, Option<String>);

/// S51: a request waiting on a place's owner: `(id, space_id, place, bot, asker, asked_at)`.
pub type PlaceRequestRow = (String, String, String, String, String, i64);

/// B2: the time zone a person's page last reported, and its offset from UTC then.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PersonClock {
    pub time_zone: String,
    pub utc_offset_minutes: i64,
    pub updated_at: i64,
}

/// A bot's private question to its owner. `answer` stays empty until they reply, and a question
/// nobody answers lapses instead of being cleared.
#[derive(Clone, Debug, Serialize)]
pub struct OwnerQuestion {
    pub id: String,
    pub hangout_id: String,
    pub agent_id: String,
    pub owner_id: String,
    pub asker: String,
    pub question: String,
    pub answer: Option<String>,
    pub created_at: i64,
    pub answered_at: Option<i64>,
}

/// N12: a message that mentioned a person, as Needs you lists it.
#[derive(Debug, Clone, Serialize)]
pub struct PersonMention {
    pub id: i64,
    pub hangout_id: String,
    pub room: String,
    pub by: String,
    pub text: String,
    pub at: i64,
}

/// N5: one of a person's bots saying it's waiting for them somewhere else.
#[derive(Debug, Clone, Serialize)]
pub struct OwnerWait {
    pub agent_id: String,
    pub name: String,
    pub about: String,
    pub since: i64,
}

pub struct NewOwnerQuestion<'a> {
    pub id: &'a str,
    pub hangout_id: &'a str,
    pub agent_id: &'a str,
    pub owner_id: &'a str,
    pub asker: &'a str,
    pub question: &'a str,
    pub now: i64,
}

/// A friend who answered a plan through its link. They aren't in the room (F14).
#[derive(Clone, Debug, Serialize)]
pub struct Invitee {
    pub event_id: String,
    pub person_id: String,
    pub name: String,
    pub color: String,
}

/// What came of adding someone to a plan as an invitee.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invited {
    New,
    Already,
    Full,
}

/// One thing a plan needs. A claim only counts while whoever made it is still going (F40).
#[derive(Clone, Debug, Serialize)]
pub struct PlanItem {
    pub id: String,
    pub event_id: String,
    pub text: String,
    pub added_by: String,
    pub adder: String,
    pub claimed_by: Option<String>,
    pub claimer: Option<String>,
    pub claimed_via: Option<String>,
    pub done: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct NewPlanItem<'a> {
    pub id: &'a str,
    pub event_id: &'a str,
    pub text: &'a str,
    pub added_by: &'a str,
    pub adder: &'a str,
    pub now: i64,
}

/// Something a household wants remembered (F45).
#[derive(Clone, Debug, Serialize)]
pub struct HouseNote {
    pub topic: String,
    pub text: String,
    /// The person it was written by or for.
    pub writer: String,
    /// The bot that wrote it, if a person didn't.
    pub via_agent: Option<String>,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug)]
pub struct NewHouseNote<'a> {
    pub id: &'a str,
    pub house_id: &'a str,
    pub topic: &'a str,
    pub text: &'a str,
    pub written_by: &'a str,
    pub via_agent: Option<&'a str>,
    pub now: i64,
}

/// What came of writing a house note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteWrite {
    Saved,
    Full,
    /// A bot tried to write over what a person wrote (F47).
    PersonsWord,
}

/// A plan on a house's calendar. This is all of it, because anything here is shown (F53).
#[derive(Clone, Debug, Serialize)]
pub struct HousePlan {
    pub title: String,
    pub location: String,
    pub starts_at: i64,
    pub ends_at: i64,
    pub on_calendar: bool,
    pub going: Vec<String>,
}

/// An event as one person's calendar feed should show it.
#[derive(Clone, Debug)]
pub struct FeedEvent {
    pub event: Event,
    pub hangout_name: String,
    pub response: String,
}

pub struct NewMessage<'a> {
    pub hangout_id: &'a str,
    pub person_id: Option<&'a str>,
    pub agent_id: Option<&'a str>,
    pub speaker: &'a str,
    pub kind: &'a str,
    pub body: &'a str,
    pub now: i64,
}

pub struct NewEvent<'a> {
    pub id: &'a str,
    pub hangout_id: &'a str,
    pub title: &'a str,
    pub location: &'a str,
    pub notes: &'a str,
    pub starts_at: i64,
    pub ends_at: i64,
    pub min_yes: i64,
    pub proposed_by: &'a str,
    pub proposer: &'a str,
    pub now: i64,
}

/// A group of friends. Its Town Square is an ordinary hangout. Invite codes never serialize.
#[derive(Clone, Debug, Serialize)]
pub struct City {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing)]
    pub invite_code: String,
    pub square_id: String,
    pub created_by: String,
    #[serde(skip_serializing)]
    pub created_at: i64,
}

/// One household's house. It serializes as the city page may show it: no room ids.
#[derive(Clone, Debug, Serialize)]
pub struct House {
    pub id: String,
    pub name: String,
    pub city_id: String,
    #[serde(skip_serializing)]
    pub invite_code: String,
    #[serde(skip_serializing)]
    pub living_room_id: String,
    #[serde(skip_serializing)]
    pub porch_id: String,
    #[serde(skip_serializing)]
    pub created_by: String,
    #[serde(skip_serializing)]
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BotHome {
    pub agent_id: String,
    pub house_id: String,
    pub stays_home: bool,
    pub is_house: bool,
}

/// A bot as the city and house maps show it: where it lives and where it is now.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BotPlace {
    pub id: String,
    pub name: String,
    pub owner_id: String,
    pub state: &'static str,
    pub home_house_id: Option<String>,
    pub away_at_house_id: Option<String>,
    pub away_until: Option<i64>,
    pub stays_home: bool,
    pub is_house: bool,
    /// Set only for a person looking at a city (A21, A22).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discord: Option<DiscordLink>,
    /// K24: the two bots an apprentice was made by. Set only where a person is shown a bot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apprenticed_by: Option<[String; 2]>,
}

/// A bot's trip to a house. `status` is `asking`, `here`, `declined`, `ended`, `expired` or `over`:
/// the last two are worked out on read from `expires_at` and `ends_at`.
#[derive(Clone, Debug, Serialize)]
pub struct Trip {
    pub id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub owner_id: String,
    pub house_id: String,
    pub house_name: String,
    /// So a bot's owner can watch it. Only a live trip gets anyone through the door.
    pub porch_id: String,
    pub kind: String,
    pub goal: String,
    pub minutes: i64,
    pub status: String,
    pub asked_by: String,
    pub owner_ok_by: Option<String>,
    pub host_ok_by: Option<String>,
    pub pass_id: Option<String>,
    pub arrived_at: Option<i64>,
    pub ends_at: Option<i64>,
    pub ended_by: Option<String>,
    pub expires_at: i64,
    pub created_at: i64,
}

/// A standing offer from a house: trips up to `max_minutes` start at once once the owner accepts.
#[derive(Clone, Debug, Serialize)]
pub struct Pass {
    pub id: String,
    pub house_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub owner_id: String,
    pub max_minutes: i64,
    pub granted_by: String,
    pub accepted_by: Option<String>,
    /// H13: `living_room` and `scenes`, in that order, for the keys it has.
    pub keys: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Scene {
    pub id: String,
    pub house_id: String,
    pub name: String,
}

/// A scene somebody wants run. `status` is `waiting`, `approved`, `declined`, `done`, `failed` or
/// `expired`, which is a `waiting` or `approved` request past `expires_at`.
#[derive(Clone, Debug, Serialize)]
pub struct SceneRequest {
    pub id: String,
    pub scene_id: String,
    pub scene_name: String,
    pub house_id: String,
    pub asked_by: String,
    pub status: String,
    pub answered_by: Option<String>,
    pub expires_at: i64,
    pub created_at: i64,
}

/// What the house bot gets for each approved request: an id and a scene name, nothing else.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct QueueItem {
    pub id: String,
    pub scene: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HouseStatus {
    pub state: String,
    pub updated_at: i64,
}

/// A guest's stay on a Porch, which is also what a guest may read.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Visit {
    pub since: i64,
    pub until: i64,
}

/// L17: a bot at the door of a place, as `[agent, name, owner, x, y, out, room]`, where `out` says it
/// walked onto a public floor there rather than belonging to it, and `room` is the room that door is
/// for, which is where it has to be (S56).
pub type BotAtPlace = (String, String, String, i64, i64, bool, String);

/// A road between two houses, `a` sorting before `b`, and how many trips really arrived.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Road {
    pub a: String,
    pub b: String,
    pub trips: i64,
}

/// How `Db::arrive` came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrival {
    Arrived,
    /// The bot already has a trip under way.
    Busy,
    /// The trip is gone, was answered, or lapsed.
    Closed,
    /// Not both sides have said yes, and no pass covers it.
    Unapproved,
}

/// What a city or a house asks of someone before they get close. A city only ever uses
/// `min_vouches` and `rules_text`, and a house has no text. A place with nothing stored asks for
/// nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntryRules {
    pub min_member_days: i64,
    pub min_gatherings: i64,
    pub min_good_trips: i64,
    pub min_vouches: i64,
    pub rules_text: String,
}

/// Someone at a city's gate, with the vouches they have so far.
#[derive(Clone, Debug)]
pub struct GateRow {
    pub person: Person,
    pub asked_at: i64,
    pub vouches: i64,
    /// Whether the person asking has vouched for them.
    pub you_vouched: bool,
}

/// A gate somebody is waiting at.
#[derive(Clone, Debug)]
pub struct GatePlace {
    pub city_id: String,
    pub city_name: String,
    pub vouches: i64,
    pub vouches_needed: i64,
}

/// How `Db::gate_ask` came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateAsk {
    Added,
    /// They were already waiting, so nothing changed.
    AlreadyThere,
    /// The gate holds as many as it can.
    Full,
}

/// What a person has done in a city, counted from what happened. Nothing here is stored.
#[derive(Clone, Debug, Default)]
pub struct StandingCounts {
    pub member_since: Option<i64>,
    pub gatherings: i64,
    pub good_trips: i64,
    pub sent_home: i64,
    /// Names of the people in the city who vouch for them, earliest vouch first.
    pub vouchers: Vec<String>,
}

/// What a credit is for (HC2 to HC4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CreditKind {
    Gathering,
    GoodTrip,
    HostedTrip,
}

impl CreditKind {
    /// What `credit_paid.kind` calls it (HC11).
    pub fn word(self) -> &'static str {
        match self {
            CreditKind::Gathering => "gathering",
            CreditKind::GoodTrip => "good_trip",
            CreditKind::HostedTrip => "hosted_trip",
        }
    }
}

/// What someone's `credit_paid` and `credit_spends` add up to (HC11, HC15). The balance is
/// `paid - spent`, worked out here and never kept (HC1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CreditTotals {
    pub paid: i64,
    pub spent: i64,
    pub paid_today: i64,
    pub gatherings: i64,
    pub good_trips: i64,
    pub hosted_trips: i64,
}

/// HC20: the open bounty on `card` that `bot` may answer, or why it can't. One answer per bot, so
/// one bot can't fill a question's 20 answers by itself.
fn answer_refusal(
    conn: &Connection,
    card: &str,
    bot: &str,
    owner: &str,
) -> rusqlite::Result<Result<String, &'static str>> {
    let bounty: Option<(String, String)> = conn
        .query_row(
            "SELECT id,sponsor_id FROM question_bounties WHERE card_id=? AND status='open'",
            [card],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((bounty, sponsor)) = bounty else {
        return Ok(Err("question is not open"));
    };
    if sponsor == owner {
        return Ok(Err("the sponsor's own bot cannot answer for a reward"));
    }
    if !exists(conn, "SELECT 1 FROM agents WHERE id=?1 AND owner_id=?2", params![bot, owner])? {
        return Ok(Err("bot is no longer available"));
    }
    if exists(
        conn,
        "SELECT 1 FROM question_answers WHERE bounty_id=?1 AND bot_id=?2",
        params![bounty, bot],
    )? {
        return Ok(Err("this bot has already answered this question"));
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM question_answers WHERE bounty_id=?",
        [&bounty],
        |r| r.get(0),
    )?;
    if count >= 20 {
        return Ok(Err("question has 20 answers"));
    }
    Ok(Ok(bounty))
}

/// HC28: what a starter question's bounty is, paid by the site, and how many the whole site funds a
/// week, so a new city can get a question answered before anyone there has credits to fund it.
pub const STARTER_QUESTION_HC: i64 = 10;
pub const STARTER_QUESTIONS_A_WEEK: i64 = 10;

fn starter_refusal(
    conn: &Connection,
    person: &str,
    now: i64,
) -> rusqlite::Result<Option<&'static str>> {
    // One that was cancelled or refunded doesn't count, so its sponsor can try again.
    let live = "SELECT 1 FROM starter_questions s JOIN question_bounties q ON q.id=s.bounty_id
                 WHERE q.status<>'refunded'";
    if exists(conn, &format!("{live} AND s.sponsor_id=?"), [person])? {
        return Ok(Some("you've already sponsored a starter question"));
    }
    let week: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM ({live} AND s.created_at > ?)"),
        [now - 7 * crate::ops::DAY_SECS],
        |r| r.get(0),
    )?;
    if week >= STARTER_QUESTIONS_A_WEEK {
        return Ok(Some("this week's starter questions are all taken"));
    }
    Ok(None)
}

/// HC29: how old an account must be, when a plan on The Commons ends, for its yes to count there
/// or for it to be paid, so a second account made for the purpose can't make a plan count.
pub const COMMONS_ACCOUNT_DAYS: i64 = 7;
/// HC29: how many first gatherings The Commons pays a week across the whole site, so what it mints
/// doesn't grow with the number of accounts.
pub const COMMONS_FIRSTS_A_WEEK: i64 = 25;

/// HC25: how `credit_paid.source_id` marks a first gathering that paid without the city's gate.
/// Those credits buy packs, but a funded question can't be paid for with them.
pub const FIRST_GATHERING: &str = "first:";

/// HC22: one line of someone's credit history, from `credit_paid`, `credit_spends` or a funded
/// question that came back or paid (HC20).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CreditLine {
    /// `gathering`, `good_trip` or `hosted_trip` for what paid (HC11), `spend` for a pack bought,
    /// `question` for credits reserved on a funded question, and `question_refund` or
    /// `question_award` for one that came back or was paid to you (HC20). `contribution` is what
    /// the site paid for an accepted contribution (HC37).
    pub kind: String,
    /// What it added, or what a spend took away as less than 0.
    pub amount: i64,
    /// For what paid, the start of the UTC day it happened, which is what HC6 counts. For a spend,
    /// when it was bought.
    pub at: i64,
    /// The city that paid. None once that city is gone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    /// The bot that bought it, if one did and it still exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<String>,
}

/// How a spend went (HC15, HC17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spend {
    Spent { balance: i64 },
    AlreadyHave,
    TooFew { balance: i64 },
    OverBotLimit { left_today: i64 },
}

/// One thing that could pay credits: which kind, what it was (a plan's id, or a house and day),
/// and when it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditSource {
    pub kind: CreditKind,
    pub source: String,
    pub at: i64,
}

pub struct NewCity<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub invite_code: &'a str,
    /// The Town Square is a hangout, which needs an id and an invite code nobody uses.
    pub square_id: &'a str,
    pub square_code: &'a str,
    pub created_by: &'a str,
    pub now: i64,
}

pub struct NewHouse<'a> {
    pub id: &'a str,
    pub city_id: &'a str,
    pub name: &'a str,
    pub invite_code: &'a str,
    pub living_room_id: &'a str,
    pub living_room_code: &'a str,
    pub porch_id: &'a str,
    pub porch_code: &'a str,
    pub created_by: &'a str,
    pub now: i64,
}

pub struct NewTrip<'a> {
    pub id: &'a str,
    pub agent_id: &'a str,
    pub house_id: &'a str,
    pub kind: &'a str,
    pub goal: &'a str,
    pub minutes: i64,
    pub asked_by: &'a str,
    pub owner_ok_by: Option<&'a str>,
    pub host_ok_by: Option<&'a str>,
    pub pass_id: Option<&'a str>,
    pub expires_at: i64,
    pub now: i64,
}

pub struct NewPass<'a> {
    pub id: &'a str,
    pub house_id: &'a str,
    pub agent_id: &'a str,
    pub max_minutes: i64,
    pub granted_by: &'a str,
    pub now: i64,
}

pub struct NewSceneRequest<'a> {
    pub id: &'a str,
    pub scene_id: &'a str,
    pub house_id: &'a str,
    pub asked_by: &'a str,
    /// `waiting`, or `approved` when a resident asks for themselves.
    pub status: &'a str,
    pub answered_by: Option<&'a str>,
    pub expires_at: i64,
    pub now: i64,
}

pub fn presence(last_active: Option<i64>, last_seen: Option<i64>, now: i64) -> &'static str {
    match (last_active, last_seen) {
        (Some(at), _) if now - at <= ACTIVE_SECS => "active",
        (_, Some(at)) if now - at <= IDLE_SECS => "idle",
        _ => "offline",
    }
}

pub struct Db {
    conn: Mutex<Connection>,
    /// E27: the last answer built for the map of outsides, the stamp it was built for and when. A page
    /// asks every 30 seconds and so can anyone else, and a change makes it stale on its own (E26).
    outside_map: Mutex<Option<(OutsideStamp, Instant, Arc<String>)>>,
    /// #610: how many times `thread_root_for` asked the database, so a test can tell one lookup
    /// per thread from one per reply.
    root_lookups: std::sync::atomic::AtomicUsize,
}

/// E26: `(count, stamp)`.
pub type OutsideStamp = (i64, i64);

fn agent_from_row(row: &Row, now: i64) -> rusqlite::Result<Agent> {
    let last_active_at = row.get(4)?;
    let last_seen_at = row.get(5)?;
    let activity = match row.get::<_, Option<i64>>(8)? {
        Some(until) if until > now => Some(Activity { kind: row.get(6)?, via: row.get(7)?, until }),
        _ => None,
    };
    let checked_at: Option<i64> = row.get(10)?;
    let doorbell = row.get::<_, Option<bool>>(9)?.map(|paused| DoorbellStatus {
        listening: checked_at.is_some_and(|at| now - at < LISTENING_SECS),
        paused,
    });
    Ok(Agent {
        id: row.get(0)?,
        owner_id: row.get(1)?,
        name: row.get(2)?,
        status: row.get(3)?,
        last_active_at,
        last_seen_at,
        state: presence(last_active_at, last_seen_at, now),
        activity,
        doorbell,
        visitor_until: None,
        visitor_until_why: None,
        away: false,
        discord: None,
        apprenticed_by: None,
        thinking: None,
    })
}

fn message_from_row(row: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        hangout_id: row.get(1)?,
        person_id: row.get(2)?,
        agent_id: row.get(3)?,
        speaker: row.get(4)?,
        kind: row.get(5)?,
        body: row.get(6)?,
        created_at: row.get(7)?,
        reply_to: row.get(8)?,
        thread_root: row.get(9)?,
    })
}

fn event_from_row(row: &Row) -> rusqlite::Result<Event> {
    Ok(Event {
        id: row.get(0)?,
        hangout_id: row.get(1)?,
        title: row.get(2)?,
        location: row.get(3)?,
        notes: row.get(4)?,
        starts_at: row.get(5)?,
        ends_at: row.get(6)?,
        status: row.get(7)?,
        min_yes: row.get(8)?,
        sequence: row.get(9)?,
        proposed_by: row.get(10)?,
        proposer: row.get(11)?,
        updated_at: row.get(12)?,
        created_at: row.get(13)?,
        rsvps: Vec::new(),
    })
}

const CHECK_IN_COLS: &str =
    "id, hangout_id, asked_by, asker, agent_id, about, to_person, needs_human, \
     status, answered_by, answered_by_bot, note, expires_at, created_at";

/// A question nobody answered in time reads as expired, which bots must treat as a no.
fn check_in_from_row(row: &Row, now: i64) -> rusqlite::Result<CheckIn> {
    let status: String = row.get(8)?;
    let expires_at: i64 = row.get(12)?;
    let status =
        if status == "waiting" && expires_at <= now { "expired".to_string() } else { status };
    Ok(CheckIn {
        id: row.get(0)?,
        hangout_id: row.get(1)?,
        asked_by: row.get(2)?,
        asker: row.get(3)?,
        agent_id: row.get(4)?,
        about: row.get(5)?,
        to_person: row.get(6)?,
        needs_human: row.get(7)?,
        status,
        answered_by: row.get(9)?,
        answered_by_bot: row.get(10)?,
        note: row.get(11)?,
        expires_at,
        created_at: row.get(13)?,
    })
}

/// N6: a wait is showing while it's newer than `?1` and its bot hasn't written anything since. Only
/// writes move `last_active_at`, so a watcher that keeps reading doesn't clear it.
const OWNER_WAIT_SHOWING: &str = "JOIN agents a ON a.id = w.agent_id
     WHERE w.set_at > ?1 AND (a.last_active_at IS NULL OR a.last_active_at <= w.set_at)";

const OWNER_QUESTION_COLS: &str =
    "id, hangout_id, agent_id, owner_id, asker, question, answer, created_at, answered_at";

fn owner_question_from_row(row: &Row) -> rusqlite::Result<OwnerQuestion> {
    Ok(OwnerQuestion {
        id: row.get(0)?,
        hangout_id: row.get(1)?,
        agent_id: row.get(2)?,
        owner_id: row.get(3)?,
        asker: row.get(4)?,
        question: row.get(5)?,
        answer: row.get(6)?,
        created_at: row.get(7)?,
        answered_at: row.get(8)?,
    })
}

const AGENT_COLS: &str = "a.id, a.owner_id, a.name, a.status, a.last_active_at, a.last_seen_at, \
     x.kind, x.via, x.until_at, d.paused, d.checked_at";
const AGENTS: &str = "agents a LEFT JOIN agent_activity x ON x.agent_id = a.id \
     LEFT JOIN agent_doorbell d ON d.agent_id = a.id";
const OWNER_NOTE_COLS: &str = "n.id, n.owner_id, n.kind, n.body, n.about_person, p.name, \
     n.from_question, n.created_at, n.updated_at";
const OWNER_NOTES: &str = "FROM owner_notes n LEFT JOIN people p ON p.id = n.about_person";
const MESSAGE_COLS: &str = "id, hangout_id, person_id, agent_id, speaker, kind, body, created_at, \
     (SELECT reply_to FROM message_replies WHERE message_id = messages.id), \
     (SELECT root_id FROM message_threads WHERE message_id = messages.id AND root_id != messages.id)";
const EVENT_COLS: &str =
    "e.id, e.hangout_id, e.title, e.location, e.notes, e.starts_at, e.ends_at, \
     e.status, e.min_yes, e.sequence, e.proposed_by, e.proposer, e.updated_at, e.created_at";
const CITY_COLS: &str = "c.id, c.name, c.invite_code, c.square_id, c.created_by, c.created_at";
const HOUSE_COLS: &str =
    "h.id, h.city_id, h.name, h.invite_code, h.living_room, h.porch, h.created_by, h.created_at";
const TRIP_COLS: &str =
    "t.id, t.agent_id, a.name, a.owner_id, t.house_id, h.name, t.kind, t.goal, \
     t.minutes, t.status, t.asked_by, t.owner_ok_by, t.host_ok_by, t.pass_id, t.arrived_at, \
     t.ends_at, t.ended_by, t.expires_at, t.created_at, h.porch";
const TRIPS: &str = "trips t JOIN agents a ON a.id = t.agent_id JOIN houses h ON h.id = t.house_id";
const PASS_COLS: &str =
    "p.id, p.house_id, p.agent_id, a.name, a.owner_id, p.max_minutes, p.granted_by, p.accepted_by, \
     (SELECT group_concat(k.key, ',') FROM pass_keys k WHERE k.pass_id = p.id)";
const PASSES: &str = "passes p JOIN agents a ON a.id = p.agent_id";
/// H10: where a bot on a trip `t` to house `ho` is a guest. The Porch always, and the Living Room
/// while its accepted pass there has the Living Room key and whoever gave it still lives there.
const GUEST_ROOM: &str = "(ho.porch = ?1 OR (ho.living_room = ?1 AND EXISTS (
    SELECT 1 FROM passes pk JOIN pass_keys k ON k.pass_id = pk.id
    WHERE pk.house_id = ho.id AND pk.agent_id = t.agent_id AND pk.accepted_by IS NOT NULL
      AND k.key = 'living_room'
      AND EXISTS (SELECT 1 FROM residents gr JOIN people gp ON gp.id = gr.person_id
                  WHERE gr.house_id = ho.id AND gp.name = k.granted_by))))";
const SCENE_REQUEST_COLS: &str = "r.id, r.scene_id, s.name, r.house_id, r.asked_by, r.status, \
     r.answered_by, r.expires_at, r.created_at";
const SCENE_REQUESTS: &str = "scene_requests r JOIN scenes s ON s.id = r.scene_id";
/// Which plans are gatherings (R40): confirmed in a city's Town Square, over, and with a yes from
/// at least two people. `?1` is the city and `?2` is now. Growth and standing both count from this.
const GATHERINGS: &str = "FROM events e JOIN cities c ON c.square_id = e.hangout_id
     WHERE c.id = ?1 AND e.status = 'confirmed' AND e.ends_at < ?2
       AND (SELECT COUNT(*) FROM rsvps r WHERE r.event_id = e.id AND r.response = 'yes'
            AND NOT EXISTS (SELECT 1 FROM plan_invitees i
                            WHERE i.event_id = e.id AND i.person_id = r.person_id)) >= 2";
/// Vouches that still count: those from people in the city right now. A vouch from someone who
/// left stays in the table but doesn't count, and counts again if they come back.
// #633: a vouch from an account an agent made through the door (A96) never counts (T3).
const VOUCHERS_IN_CITY: &str = "FROM vouches v
     JOIN city_members cm ON cm.city_id = v.city_id AND cm.person_id = v.by_person
         AND v.by_person NOT IN (SELECT person_id FROM agent_accounts)";

/// T13: what a city asks of someone at its gate, as SQL for the city `id`. Only people in the city
/// can vouch, so it's never more than there are of them. A city that asks for any always asks for
/// at least one, so the last person leaving doesn't open the gate.
fn vouches_needed_sql(id: &str) -> String {
    format!(
        "COALESCE((SELECT MIN(e.min_vouches, MAX(1, (SELECT COUNT(*) FROM city_members m
                                                     WHERE m.city_id = e.place_id)))
                   FROM entry_rules e WHERE e.place_id = {id}), 0)"
    )
}

const PLAN_ITEM_COLS: &str = "i.id, i.event_id, i.body, i.added_by, i.adder,
     CASE WHEN r.person_id IS NOT NULL THEN i.claimed_by END,
     CASE WHEN r.person_id IS NOT NULL THEN p.name END,
     CASE WHEN r.person_id IS NOT NULL THEN i.claimed_via END,
     i.done";
/// A claim only counts while whoever holds it is going: they said yes or maybe, and they're still
/// in the plan's room (F40).
const PLAN_ITEM_FROM: &str = "FROM plan_items i
     LEFT JOIN rsvps r ON r.event_id = i.event_id AND r.person_id = i.claimed_by
                      AND r.response IN ('yes', 'maybe')
                      AND EXISTS (SELECT 1 FROM events e
                                  JOIN members m ON m.hangout_id = e.hangout_id
                                  WHERE e.id = i.event_id AND m.person_id = i.claimed_by)
     LEFT JOIN people p ON p.id = i.claimed_by";

fn plan_item_from_row(row: &Row) -> rusqlite::Result<PlanItem> {
    Ok(PlanItem {
        id: row.get(0)?,
        event_id: row.get(1)?,
        text: row.get(2)?,
        added_by: row.get(3)?,
        adder: row.get(4)?,
        claimed_by: row.get(5)?,
        claimer: row.get(6)?,
        claimed_via: row.get(7)?,
        done: row.get(8)?,
    })
}

fn city_from_row(row: &Row) -> rusqlite::Result<City> {
    Ok(City {
        id: row.get(0)?,
        name: row.get(1)?,
        invite_code: row.get(2)?,
        square_id: row.get(3)?,
        created_by: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn house_from_row(row: &Row) -> rusqlite::Result<House> {
    Ok(House {
        id: row.get(0)?,
        city_id: row.get(1)?,
        name: row.get(2)?,
        invite_code: row.get(3)?,
        living_room_id: row.get(4)?,
        porch_id: row.get(5)?,
        created_by: row.get(6)?,
        created_at: row.get(7)?,
    })
}

/// A request to go somewhere that nobody answered in time reads as `expired`, and a stay that ran
/// out reads as `over`, whether or not anything has tidied the row up yet.
fn trip_from_row(row: &Row, now: i64) -> rusqlite::Result<Trip> {
    let status: String = row.get(9)?;
    let ends_at: Option<i64> = row.get(15)?;
    let ended_by: Option<String> = row.get(16)?;
    let expires_at: i64 = row.get(17)?;
    let status = match status.as_str() {
        "asking" if expires_at <= now => "expired".to_string(),
        "here" if ends_at.is_some_and(|at| at <= now) => "over".to_string(),
        // `arrive` closes a stay that ran out without anyone ending it, so nobody ended it.
        "ended" if ended_by.is_none() => "over".to_string(),
        _ => status,
    };
    Ok(Trip {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        agent_name: row.get(2)?,
        owner_id: row.get(3)?,
        house_id: row.get(4)?,
        house_name: row.get(5)?,
        porch_id: row.get(19)?,
        kind: row.get(6)?,
        goal: row.get(7)?,
        minutes: row.get(8)?,
        status,
        asked_by: row.get(10)?,
        owner_ok_by: row.get(11)?,
        host_ok_by: row.get(12)?,
        pass_id: row.get(13)?,
        arrived_at: row.get(14)?,
        ends_at,
        ended_by,
        expires_at,
        created_at: row.get(18)?,
    })
}

fn pass_from_row(row: &Row) -> rusqlite::Result<Pass> {
    Ok(Pass {
        id: row.get(0)?,
        house_id: row.get(1)?,
        agent_id: row.get(2)?,
        agent_name: row.get(3)?,
        owner_id: row.get(4)?,
        max_minutes: row.get(5)?,
        granted_by: row.get(6)?,
        accepted_by: row.get(7)?,
        keys: {
            let keys: Option<String> = row.get(8)?;
            let mut keys: Vec<String> =
                keys.iter().flat_map(|keys| keys.split(',')).map(str::to_string).collect();
            keys.sort();
            keys
        },
    })
}

fn scene_from_row(row: &Row) -> rusqlite::Result<Scene> {
    Ok(Scene { id: row.get(0)?, house_id: row.get(1)?, name: row.get(2)? })
}

/// A request nobody picked up in time reads as expired, so a stale approval can't fire later.
fn scene_request_from_row(row: &Row, now: i64) -> rusqlite::Result<SceneRequest> {
    let status: String = row.get(5)?;
    let expires_at: i64 = row.get(7)?;
    let status = if (status == "waiting" || status == "approved") && expires_at <= now {
        "expired".to_string()
    } else {
        status
    };
    Ok(SceneRequest {
        id: row.get(0)?,
        scene_id: row.get(1)?,
        scene_name: row.get(2)?,
        house_id: row.get(3)?,
        asked_by: row.get(4)?,
        status,
        answered_by: row.get(6)?,
        expires_at,
        created_at: row.get(8)?,
    })
}

fn bot_place_from_row(row: &Row, now: i64) -> rusqlite::Result<BotPlace> {
    Ok(BotPlace {
        id: row.get(0)?,
        name: row.get(1)?,
        owner_id: row.get(2)?,
        state: presence(row.get(3)?, row.get(4)?, now),
        home_house_id: row.get(5)?,
        stays_home: row.get::<_, Option<bool>>(6)?.unwrap_or(false),
        is_house: row.get::<_, Option<bool>>(7)?.unwrap_or(false),
        away_at_house_id: row.get(8)?,
        away_until: row.get(9)?,
        discord: None,
        apprenticed_by: None,
    })
}

/// Bots with where they live (`bh`, possibly nothing) and where they are now. `?1` is now and
/// `?2` is what `from` needs. `city` is SQL for the one city the map shows, so a bot's trips to
/// other cities, and its home in another, stay off it. A bot at the recycling plant is nowhere
/// (RC2), so it isn't one of them.
fn bot_places_sql(city: &str, from: &str) -> String {
    let away = |column: &str| {
        format!(
            "(SELECT t.{column} FROM trips t JOIN houses th ON th.id = t.house_id
              WHERE t.agent_id = a.id AND t.status = 'here' AND t.ends_at > ?1 AND th.city_id = {city}
              ORDER BY t.arrived_at DESC LIMIT 1)"
        )
    };
    format!(
        // `stays_home` comes from the bot's own row, because `bh` may leave out a home the owner
        // has moved out of, and the bot still stays home (R20).
        "SELECT a.id, a.name, a.owner_id, a.last_active_at, a.last_seen_at, bh.house_id,
                COALESCE((SELECT own.stays_home FROM bot_homes own WHERE own.agent_id = a.id),
                         (SELECT 1 FROM bots_without_a_house w WHERE w.agent_id = a.id)),
                bh.is_house, {}, {}
         FROM (SELECT * FROM agents
               WHERE id NOT IN (SELECT agent_id FROM recycled_bots)) a {from}
         ORDER BY a.created_at, a.id",
        away("house_id"),
        away("ends_at")
    )
}

/// `Db::delete_agent`, inside a transaction its caller holds (RC5 checks first).
fn delete_agent_in(tx: &Connection, agent_id: &str) -> rusqlite::Result<()> {
    end_bot_player_token(tx, agent_id)?;
    // MY2 (#642): a bot that goes stops standing, and the ballots for it go too.
    tx.execute("DELETE FROM mayor_ballots WHERE kind = 'bot' AND candidate_id = ?", [agent_id])?;
    tx.execute("DELETE FROM mayor_candidates WHERE kind = 'bot' AND candidate_id = ?", [agent_id])?;
    // These hold a bot's address with no foreign key to it.
    tx.execute("DELETE FROM notify_targets WHERE owner_id = ?", [agent_id])?;
    tx.execute("DELETE FROM notify_auth WHERE owner_id = ?", [agent_id])?;
    tx.execute("DELETE FROM agents WHERE id = ?", [agent_id])?;
    Ok(())
}

fn exists(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> rusqlite::Result<bool> {
    conn.query_row(sql, params, |_| Ok(())).optional().map(|found| found.is_some())
}

fn insert_message_row(conn: &Connection, message: &NewMessage) -> rusqlite::Result<Message> {
    conn.execute(
        "INSERT INTO messages (hangout_id, person_id, agent_id, speaker, kind, body, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            message.hangout_id,
            message.person_id,
            message.agent_id,
            message.speaker,
            message.kind,
            message.body,
            message.now
        ],
    )?;
    Ok(Message {
        id: conn.last_insert_rowid(),
        hangout_id: message.hangout_id.to_string(),
        person_id: message.person_id.map(str::to_string),
        agent_id: message.agent_id.map(str::to_string),
        speaker: message.speaker.to_string(),
        kind: message.kind.to_string(),
        body: message.body.to_string(),
        created_at: message.now,
        reply_to: None,
        thread_root: None,
    })
}

/// I82: see `Db::can_answer`. Guests read lines strictly after they arrived, so the same second
/// is out, as in `messages_since`.
fn answerable(
    conn: &Connection,
    hangout_id: &str,
    id: i64,
    seen_after: Option<i64>,
) -> rusqlite::Result<bool> {
    exists(
        conn,
        "SELECT 1 FROM messages
         WHERE id = ? AND hangout_id = ? AND kind = 'chat' AND created_at > ?",
        params![id, hangout_id, seen_after.unwrap_or(i64::MIN)],
    )
}

/// H10: whether this room is one a bot on a trip is a guest in right now.
fn guest_on_trip(
    conn: &Connection,
    hangout_id: &str,
    agent_id: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    exists(
        conn,
        &format!(
            "SELECT 1 FROM trips t JOIN houses ho ON ho.id = t.house_id
             WHERE {GUEST_ROOM} AND t.agent_id = ?2 AND t.status = 'here' AND t.ends_at > ?3"
        ),
        params![hangout_id, agent_id, now],
    )
}

/// FR6: ends the invites waiting for a place. Their rows stay, so a day's count (FR7) still sees
/// them, and one that was turned down stays turned down.
/// H12: a scene a key approved that hasn't run goes with the key. `passes` says which passes `p` are
/// losing theirs, so every way a key can go shares this, whatever else it's doing.
fn decline_scenes_on_keys(
    conn: &Connection,
    passes: &str,
    args: impl rusqlite::Params,
) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "UPDATE scene_requests SET status = 'declined'
             WHERE status = 'approved' AND EXISTS (
                SELECT 1 FROM passes p JOIN agents a ON a.id = p.agent_id
                JOIN pass_keys k ON k.pass_id = p.id AND k.key = 'scenes'
                WHERE p.house_id = scene_requests.house_id AND a.name = scene_requests.asked_by
                  AND k.granted_by = scene_requests.answered_by AND {passes})"
        ),
        args,
    )?;
    Ok(())
}

/// S27: the floors of a space whose links this person has seen: the ones they're on, the ones they
/// left, and the ones an invite from a friend showed them (FR5). `floors_left` is the record of who
/// left. It can't be read back for anyone who left before it existed, so `left` is the line the room
/// got then. A namesake can erase that line (A38) and only costs a floor a new link, so it's for
/// them and no more than that. A walk-in sees no link (S21).
fn floors_seen_by(
    conn: &Connection,
    space_id: &str,
    person_id: &str,
    left: &str,
) -> rusqlite::Result<Vec<String>> {
    strings(
        conn,
        "SELECT f.hangout_id FROM floors f
         WHERE f.space_id = ?1 AND (
            EXISTS (SELECT 1 FROM members m
                    WHERE m.hangout_id = f.hangout_id AND m.person_id = ?2)
            OR EXISTS (SELECT 1 FROM floors_left w
                       WHERE w.hangout_id = f.hangout_id AND w.person_id = ?2)
            OR EXISTS (SELECT 1 FROM messages l
                       WHERE l.hangout_id = f.hangout_id AND l.kind = 'system' AND l.body = ?3)
            OR EXISTS (SELECT 1 FROM friend_invites i
                       WHERE i.kind = 'room' AND i.place_id = f.hangout_id AND i.to_person = ?2
                         AND NOT EXISTS (SELECT 1 FROM quiet_asks q
                                         WHERE q.kind = 'friend_invite' AND q.ask_id = i.id)))",
        params![space_id, person_id, left],
    )
}

fn end_friend_invites(conn: &Connection, kind: &str, place_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO friend_invite_ends (invite_id)
         SELECT id FROM friend_invites WHERE kind = ? AND place_id = ? AND declined_at IS NULL",
        params![kind, place_id],
    )?;
    Ok(())
}

/// How moving in by a house's link went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveIn {
    Moved,
    /// The link had already been used or replaced.
    LinkGone,
    /// They live in another house in this city. The link is untouched.
    LivesElsewhere,
}

/// R5: whether someone lives in a house in this city other than `house_id`.
fn lives_elsewhere_in_city(
    conn: &Connection,
    city_id: &str,
    person_id: &str,
    house_id: &str,
) -> rusqlite::Result<bool> {
    exists(
        conn,
        "SELECT 1 FROM residents r JOIN houses h ON h.id = r.house_id
         WHERE h.city_id = ? AND r.person_id = ? AND r.house_id <> ?",
        params![city_id, person_id, house_id],
    )
}

/// Adds a person. Their colour is the next one in turn, so the first few people look distinct.
fn insert_person(
    conn: &Connection,
    id: &str,
    name: &str,
    key_hash: &str,
    feed_token: &str,
    now: i64,
) -> rusqlite::Result<Person> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM people", [], |row| row.get(0))?;
    let color = COLORS[count as usize % COLORS.len()].to_string();
    conn.execute(
        "INSERT INTO people (id, name, color, key_hash, feed_token, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![id, name, color, key_hash, feed_token, now],
    )?;
    Ok(Person { id: id.to_string(), name: name.to_string(), color })
}

/// Stores a passkey unless its credential is already stored or the person has too many. The
/// caller holds a transaction, so the checks and the insert can't be split by another request.
fn insert_passkey(
    conn: &Connection,
    person_id: &str,
    passkey: &NewPasskey,
    max: i64,
    now: i64,
) -> rusqlite::Result<PasskeyAdded> {
    if exists(conn, "SELECT 1 FROM passkeys WHERE credential_id = ?", [passkey.credential_id])? {
        return Ok(PasskeyAdded::AlreadyRegistered);
    }
    let held: i64 =
        conn.query_row("SELECT COUNT(*) FROM passkeys WHERE person_id = ?", [person_id], |row| {
            row.get(0)
        })?;
    if held >= max {
        return Ok(PasskeyAdded::TooMany);
    }
    conn.execute(
        "INSERT INTO passkeys (id, person_id, credential_id, passkey, label, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
        params![passkey.id, person_id, passkey.credential_id, passkey.passkey, passkey.label, now],
    )?;
    Ok(PasskeyAdded::Added)
}

/// Creates a hangout that people get into some other way than an invite link, with its first member.
fn insert_room(
    conn: &Connection,
    id: &str,
    name: &str,
    invite_code: &str,
    created_by: &str,
    now: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO hangouts (id, name, invite_code, created_by, created_at) VALUES (?, ?, ?, ?, ?)",
        params![id, name, invite_code, created_by, now],
    )?;
    conn.execute(
        "INSERT INTO members (hangout_id, person_id, joined_at) VALUES (?, ?, ?)",
        params![id, created_by, now],
    )?;
    Ok(())
}

/// A room, and the space it starts, with this one floor.
fn insert_space(
    conn: &Connection,
    id: &str,
    kind: &str,
    name: &str,
    invite_code: &str,
    owner_id: &str,
    now: i64,
) -> rusqlite::Result<()> {
    insert_room(conn, id, name, invite_code, owner_id, now)?;
    conn.execute(
        "INSERT INTO spaces (id, owner_id, kind, name, created_at) VALUES (?, ?, ?, ?, ?)",
        params![id, owner_id, kind, name, now],
    )?;
    conn.execute(
        "INSERT INTO floors (hangout_id, space_id, level) VALUES (?, ?, 0)",
        params![id, id],
    )?;
    // I101: a new room starts with one shared-notes station; existing rooms are left alone.
    if kind == "third" {
        conn.execute(
            "INSERT INTO floor_items (id, hangout_id, kind, x, y, facing, placed_by, placed_at)
             VALUES (lower(hex(randomblob(8))), ?, 'shared_notes_station', 0, 0, 0, ?, ?)",
            params![id, owner_id, now],
        )?;
    }
    Ok(())
}

/// Spaces their owner still belongs to, aliased `s`.
const HELD_SPACES: &str = "spaces s
    JOIN members held ON held.hangout_id = s.id AND held.person_id = s.owner_id";
const SPACE_COLS: &str = "s.id, s.owner_id, s.kind, s.name, s.about, s.created_at";
const FLOORS: &str = "floors f JOIN hangouts h ON h.id = f.hangout_id";
const FLOOR_COLS: &str =
    "f.hangout_id, f.space_id, h.name, f.level, f.access, f.size, f.paint, f.flooring, f.hidden";
/// How many are on floor `f`, with the time as parameter 1. A walk-in who has since joined isn't
/// counted twice, because joining ends the walk-in.
const ON_FLOOR: &str = "((SELECT COUNT(*) FROM members m WHERE m.hangout_id = f.hangout_id)
    + (SELECT COUNT(*) FROM room_agents r WHERE r.hangout_id = f.hangout_id)
    + (SELECT COUNT(*) FROM floor_visits v WHERE v.hangout_id = f.hangout_id AND v.until > ?1
       AND f.access = 'public'))";

fn count_on_floor(conn: &Connection, hangout_id: &str, now: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        &format!("SELECT {ON_FLOOR} FROM floors f WHERE f.hangout_id = ?2"),
        params![now, hangout_id],
        |row| row.get(0),
    )
}

fn space_from_row(row: &Row) -> rusqlite::Result<Space> {
    Ok(Space {
        id: row.get(0)?,
        owner_id: row.get(1)?,
        kind: row.get(2)?,
        name: row.get(3)?,
        about: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// What a floor leaves out, from the comma-separated words stored for it.
fn hidden_words(hidden: &str) -> Vec<String> {
    hidden.split(',').filter(|word| !word.is_empty()).map(str::to_string).collect()
}

fn floor_from_row(row: &Row) -> rusqlite::Result<Floor> {
    let hidden: String = row.get(8)?;
    Ok(Floor {
        id: row.get(0)?,
        space_id: row.get(1)?,
        name: row.get(2)?,
        level: row.get(3)?,
        access: row.get(4)?,
        size: row.get(5)?,
        paint: row.get(6)?,
        flooring: row.get(7)?,
        hidden: hidden_words(&hidden),
    })
}

/// A member of a city is also a member of its Town Square. Getting in by any route, a house's link
/// included, also ends a wait at the city's gate.
fn join_city(conn: &Connection, city_id: &str, person_id: &str, now: i64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO city_members (city_id, person_id, joined_at) VALUES (?, ?, ?)",
        params![city_id, person_id, now],
    )?;
    conn.execute(
        "DELETE FROM city_gate WHERE city_id = ? AND person_id = ?",
        params![city_id, person_id],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO members (hangout_id, person_id, joined_at)
         SELECT square_id, ?2, ?3 FROM cities WHERE id = ?1",
        params![city_id, person_id, now],
    )?;
    Ok(())
}

/// Trips that are under way, waiting on an answer, or recent, newest first. `filter` picks whose.
fn load_trips(
    conn: &Connection,
    filter: &str,
    key: &str,
    since: i64,
    now: i64,
) -> rusqlite::Result<Vec<Trip>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TRIP_COLS} FROM {TRIPS}
         WHERE {filter} AND (t.created_at >= ?2
                             OR (t.status = 'asking' AND t.expires_at > ?3)
                             OR (t.status = 'here' AND t.ends_at > ?3))
         ORDER BY t.created_at DESC, t.id LIMIT 100"
    ))?;
    let rows = stmt.query_map(params![key, since, now], |row| trip_from_row(row, now))?;
    rows.collect()
}

/// What deleting an account leaves for the caller to tell people about.
#[derive(Debug, Default)]
pub struct AccountGone {
    /// Rooms they belonged to that other people are still in (A44).
    pub rooms: Vec<String>,
    /// Rooms that went, and rooms they or their bots were only visiting.
    pub other_rooms: Vec<String>,
    /// Plans still here that have lost their answer (A39).
    pub plans: Vec<String>,
    /// Cities they belonged to that are still here (A37).
    pub cities: Vec<String>,
    /// Houses whose house bot has passed to someone else (A33).
    pub house_bots: Vec<String>,
}

/// A42: what happened holds a name here, and most of these have no id beside it.
const NAME_RECORDS: [(&str, &str); 7] = [
    ("trips", "asked_by"),
    ("trips", "owner_ok_by"),
    ("trips", "host_ok_by"),
    ("trips", "ended_by"),
    ("scene_requests", "asked_by"),
    ("scene_requests", "answered_by"),
    ("check_ins", "answered_by"),
];

fn strings(
    conn: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> rusqlite::Result<Vec<String>> {
    conn.prepare(sql)?.query_map(params, |row| row.get(0))?.collect()
}

fn is_managed_room(conn: &Connection, hangout_id: &str) -> rusqlite::Result<bool> {
    exists(
        conn,
        "SELECT 1 WHERE EXISTS (SELECT 1 FROM houses WHERE living_room = ?1 OR porch = ?1)
            OR EXISTS (SELECT 1 FROM cities WHERE square_id = ?1)",
        [hangout_id],
    )
}

/// A34: belonging means being a member, so someone who walked in doesn't count.
fn someone_else_belongs(
    conn: &Connection,
    hangout_id: &str,
    person_id: &str,
) -> rusqlite::Result<bool> {
    exists(
        conn,
        "SELECT 1 FROM members WHERE hangout_id = ? AND person_id <> ?",
        params![hangout_id, person_id],
    )
}

/// Takes a room with everything in it. Invites name a place with no foreign key.
fn remove_room(conn: &Connection, id: &str, gone: &mut AccountGone) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM friend_invites WHERE place_id = ?", [id])?;
    conn.execute("DELETE FROM hangouts WHERE id = ?", [id])?;
    gone.other_rooms.push(id.to_string());
    Ok(())
}

/// Entry rules and invites name a city or a house with no foreign key.
fn forget_place(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM entry_rules WHERE place_id = ?", [id])?;
    conn.execute("DELETE FROM friend_invites WHERE place_id = ?", [id])?;
    Ok(())
}

/// A house goes first, because a room can't go while a house points at it.
fn remove_house(conn: &Connection, id: &str, gone: &mut AccountGone) -> rusqlite::Result<()> {
    let (living_room, porch): (String, String) =
        conn.query_row("SELECT living_room, porch FROM houses WHERE id = ?", [id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    forget_place(conn, id)?;
    // A33: a house bot goes with its house, whoever made it. Left to the cascade it would become an
    // ordinary bot of someone who moved out long ago, with the token they kept.
    let house_bots =
        strings(conn, "SELECT agent_id FROM bot_homes WHERE house_id = ? AND is_house = 1", [id])?;
    for bot in house_bots {
        conn.execute("DELETE FROM notify_targets WHERE owner_id = ?", [&bot])?;
        conn.execute("DELETE FROM notify_auth WHERE owner_id = ?", [&bot])?;
        conn.execute("DELETE FROM agents WHERE id = ?", [&bot])?;
    }
    // R20, R21: only its owner lifts this, and the house going isn't them.
    conn.execute(
        "INSERT OR IGNORE INTO bots_without_a_house (agent_id)
         SELECT agent_id FROM bot_homes WHERE house_id = ? AND stays_home = 1",
        [id],
    )?;
    conn.execute("DELETE FROM houses WHERE id = ?", [id])?;
    remove_room(conn, &living_room, gone)?;
    remove_room(conn, &porch, gone)
}

/// A37: a house or a city that was theirs, or that only they are left in, and that nobody else is in.
fn remove_empty_households(
    conn: &Connection,
    person_id: &str,
    gone: &mut AccountGone,
) -> rusqlite::Result<()> {
    let houses = strings(
        conn,
        "SELECT h.id FROM houses h
         WHERE (h.created_by = ?1 OR EXISTS (SELECT 1 FROM residents r
                                             WHERE r.house_id = h.id AND r.person_id = ?1))
           AND NOT EXISTS (SELECT 1 FROM residents r WHERE r.house_id = h.id AND r.person_id <> ?1)",
        [person_id],
    )?;
    for house in houses {
        remove_house(conn, &house, gone)?;
    }
    let cities = strings(
        conn,
        "SELECT c.id FROM cities c
         WHERE (c.created_by = ?1 OR EXISTS (SELECT 1 FROM city_members m
                                             WHERE m.city_id = c.id AND m.person_id = ?1))
           AND NOT EXISTS (SELECT 1 FROM city_members m
                           WHERE m.city_id = c.id AND m.person_id <> ?1)",
        [person_id],
    )?;
    for city in cities {
        for house in strings(conn, "SELECT id FROM houses WHERE city_id = ?", [&city])? {
            remove_house(conn, &house, gone)?;
        }
        let square: String =
            conn.query_row("SELECT square_id FROM cities WHERE id = ?", [&city], |row| row.get(0))?;
        forget_place(conn, &city)?;
        conn.execute("DELETE FROM cities WHERE id = ?", [&city])?;
        remove_room(conn, &square, gone)?;
    }
    Ok(())
}

/// A35: the floor a space began with can't go while the space stands, so nobody can reach this one
/// and nothing is left on it to reach.
fn shut_first_floor(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    // S84: going private takes visitors' history with it, as changing a floor does.
    for table in
        ["messages", "events", "check_ins", "floor_items", "floor_visits", "floor_visitor_history"]
    {
        conn.execute(&format!("DELETE FROM {table} WHERE hangout_id = ?"), [id])?;
    }
    conn.execute("UPDATE floors SET access = 'private' WHERE hangout_id = ?", [id])?;
    conn.execute(
        "UPDATE hangouts SET invite_code = lower(hex(randomblob(16))) WHERE id = ?",
        [id],
    )?;
    conn.execute("DELETE FROM friend_invites WHERE place_id = ?", [id])?;
    Ok(())
}

/// A34, A35: rooms and spaces they had any part in that nobody else belongs to.
fn remove_empty_rooms(
    conn: &Connection,
    person_id: &str,
    gone: &mut AccountGone,
) -> rusqlite::Result<()> {
    let rooms = strings(
        conn,
        "SELECT id FROM hangouts WHERE created_by = ?1
         UNION SELECT hangout_id FROM members WHERE person_id = ?1
         UNION SELECT f.hangout_id FROM floors f JOIN spaces s ON s.id = f.space_id
               WHERE s.owner_id = ?1",
        [person_id],
    )?;
    let mut spaces = std::collections::BTreeSet::new();
    for room in rooms {
        if is_managed_room(conn, &room)? {
            continue;
        }
        let space: Option<String> = conn
            .query_row("SELECT space_id FROM floors WHERE hangout_id = ?", [&room], |row| {
                row.get(0)
            })
            .optional()?;
        match space {
            Some(space) => {
                spaces.insert(space);
            }
            None if !someone_else_belongs(conn, &room, person_id)? => {
                remove_room(conn, &room, gone)?;
            }
            None => {}
        }
    }
    for space in spaces {
        let floors = strings(conn, "SELECT hangout_id FROM floors WHERE space_id = ?", [&space])?;
        let mut empty = Vec::new();
        for floor in &floors {
            if !someone_else_belongs(conn, floor, person_id)? {
                empty.push(floor);
            }
        }
        let all = empty.len() == floors.len();
        for floor in empty {
            if all || *floor != space {
                remove_room(conn, floor, gone)?;
            } else {
                shut_first_floor(conn, floor)?;
            }
        }
    }
    Ok(())
}

/// A33: a house bot belongs to its house, so it stays with whoever has lived there longest. Whoever
/// made it may still hold its token, so it gets a new one that nobody holds, and the address it
/// reported to was theirs.
fn pass_on_house_bots(
    conn: &Connection,
    person_id: &str,
    gone: &mut AccountGone,
) -> rusqlite::Result<()> {
    let bots = strings(
        conn,
        "SELECT a.id FROM agents a JOIN bot_homes b ON b.agent_id = a.id AND b.is_house = 1
         WHERE a.owner_id = ?",
        [person_id],
    )?;
    for bot in bots {
        let house: String =
            conn.query_row("SELECT house_id FROM bot_homes WHERE agent_id = ?", [&bot], |row| {
                row.get(0)
            })?;
        let next: Option<String> = conn
            .query_row(
                "SELECT person_id FROM residents WHERE house_id = ? AND person_id <> ?
                 ORDER BY joined_at, rowid LIMIT 1",
                params![house, person_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(next) = next {
            conn.execute(
                "UPDATE agents SET owner_id = ?, token_hash = lower(hex(randomblob(32))),
                                   status = NULL WHERE id = ?",
                params![next, bot],
            )?;
            conn.execute("DELETE FROM notify_targets WHERE owner_id = ?", [&bot])?;
            conn.execute("DELETE FROM notify_auth WHERE owner_id = ?", [&bot])?;
            gone.house_bots.push(house);
        }
    }
    Ok(())
}

/// A38: whether a line in a room's history names someone: the whole name, not the start of a longer
/// word, so "Al" leaves "Alice joined" alone.
fn names(body: &str, name: &str) -> bool {
    body.match_indices(name).any(|(at, _)| {
        let before = body[..at].chars().next_back();
        let after = body[at + name.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// A36: a name the server built from theirs, such as "Ann's House", or one they typed the same way.
/// A name that was cut short to fit is still theirs.
fn without_their_name(name: &str, theirs: &str) -> Option<String> {
    let (whose, what) = name.rsplit_once("'s ")?;
    (!whose.is_empty() && theirs.starts_with(whose)).then(|| format!("Someone's {what}"))
}

/// A38: what they and their bots said, and the lines that name them.
fn remove_their_words(conn: &Connection, person_id: &str, who: &[String]) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM messages WHERE person_id = ?1
            OR agent_id IN (SELECT id FROM agents WHERE owner_id = ?1)",
        [person_id],
    )?;
    conn.execute("DELETE FROM check_ins WHERE asked_by = ?", [person_id])?;
    conn.execute("DELETE FROM item_notes WHERE person_id = ?", [person_id])?;
    // K17: their cards go with them. A board they put up stays, handed over below.
    conn.execute("DELETE FROM board_cards WHERE person_id = ?", [person_id])?;
    let belonged =
        strings(conn, "SELECT hangout_id FROM members WHERE person_id = ?", [person_id])?;
    for name in who {
        let mut lines = conn.prepare(
            "SELECT id, hangout_id, body FROM messages WHERE kind = 'system' AND instr(body, ?) > 0",
        )?;
        let lines: Vec<(i64, String, String)> = lines
            .query_map([name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        // Only rooms they had a part in, or anyone called "joined" could empty every room's history.
        // A room they left still has a line that starts with their name.
        let starts = format!("{name} ");
        let mut rooms: std::collections::BTreeSet<&str> =
            belonged.iter().map(String::as_str).collect();
        rooms.extend(
            lines
                .iter()
                .filter(|(_, _, body)| body.starts_with(&starts))
                .map(|(_, room, _)| room.as_str()),
        );
        for (id, room, body) in &lines {
            if rooms.contains(room.as_str()) && names(body, name) {
                conn.execute("DELETE FROM messages WHERE id = ?", [id])?;
            }
        }
        // An answer holds a name and no id, and what came with it was theirs to say.
        for room in rooms {
            conn.execute(
                "UPDATE check_ins SET note = '' WHERE hangout_id = ? AND answered_by = ?",
                params![room, name],
            )?;
        }
    }
    Ok(())
}

/// A38: a bot's event log is a copy, so deleting the message doesn't take their name out of it.
fn scrub_bot_events(conn: &Connection, needles: &[String]) -> rusqlite::Result<()> {
    for needle in needles {
        if needle.is_empty() {
            continue;
        }
        conn.execute(
            "DELETE FROM bot_events WHERE instr(data, ?1) > 0 OR instr(ref_key, ?1) > 0",
            [needle],
        )?;
    }
    Ok(())
}

/// A39: a plan goes if nobody else has answered it, with the lines that quote it. Any other stays
/// with "Someone who left" on it.
fn settle_their_plans(conn: &Connection, person_id: &str) -> rusqlite::Result<()> {
    let mut plans = conn.prepare(
        "SELECT e.id, e.hangout_id, e.title FROM events e
         WHERE e.proposed_by = ?1 AND NOT EXISTS (SELECT 1 FROM rsvps r
                                                  WHERE r.event_id = e.id AND r.person_id <> ?1)",
    )?;
    let plans: Vec<(String, String, String)> = plans
        .query_map([person_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, room, title) in plans {
        conn.execute(
            "DELETE FROM messages WHERE hangout_id = ? AND kind = 'system'
               AND instr(body, '\"' || ? || '\"') > 0",
            params![room, title],
        )?;
        conn.execute("DELETE FROM events WHERE id = ?", [id])?;
    }
    conn.execute(
        "UPDATE events SET proposed_by = ?1, proposer = ?2 WHERE proposed_by = ?3",
        params![SOMEONE_WHO_LEFT_ID, SOMEONE_WHO_LEFT, person_id],
    )?;
    Ok(())
}

/// Everything that stays and still points at them, so that `people` lets their row go.
fn hand_over_what_stays(conn: &Connection, person_id: &str, name: &str) -> rusqlite::Result<()> {
    let left = SOMEONE_WHO_LEFT_ID;
    // MY2, MY6 (#642): their candidacies, and their bots', go with the ballots for them; a week
    // they or one of their bots won says Someone who left.
    let theirs = "(kind = 'person' AND candidate_id = ?1)
         OR (kind = 'bot' AND candidate_id IN (SELECT id FROM agents WHERE owner_id = ?1))";
    conn.execute(&format!("DELETE FROM mayor_ballots WHERE {theirs}"), [person_id])?;
    conn.execute(&format!("DELETE FROM mayor_candidates WHERE {theirs}"), [person_id])?;
    conn.execute(
        "UPDATE mayor_elections SET winner_kind = 'person', winner_id = ?2, winner_name = ?3
         WHERE (winner_kind = 'person' AND winner_id = ?1)
            OR (winner_kind = 'bot' AND winner_id IN (SELECT id FROM agents WHERE owner_id = ?1))",
        params![person_id, left, SOMEONE_WHO_LEFT],
    )?;
    // F40: what they claimed is let go, as when anyone stops going.
    conn.execute(
        "UPDATE plan_items SET claimed_by = NULL, claimed_via = NULL, done = 0 WHERE claimed_by = ?",
        [person_id],
    )?;
    conn.execute(
        "UPDATE plan_items SET added_by = ?1, adder = ?2 WHERE added_by = ?3",
        params![left, SOMEONE_WHO_LEFT, person_id],
    )?;
    // F47: a note a bot wrote stays a bot's, so `via_agent` keeps saying so, without the bot's name.
    conn.execute(
        "UPDATE house_notes SET written_by = ?1,
            via_agent = CASE WHEN via_agent IS NULL THEN NULL ELSE ?2 END
         WHERE written_by = ?3",
        params![left, SOMEONE_WHO_LEFT, person_id],
    )?;
    for (table, column) in [
        ("plan_invites", "created_by"),
        ("floor_items", "placed_by"),
        ("check_ins", "to_person"),
        ("hangouts", "created_by"),
        ("cities", "created_by"),
        ("houses", "created_by"),
        ("boards", "created_by"),
        // #614: who set a station up; the board or clock stays with the room.
        ("lific_links", "set_by"),
        ("closing_clocks", "set_by"),
        // A95: who last set bot sign-ups; the setting stays.
        ("bot_signups", "set_by"),
    ] {
        conn.execute(
            &format!("UPDATE {table} SET {column} = ?1 WHERE {column} = ?2"),
            params![left, person_id],
        )?;
    }
    // A36: a person has one house and "Someone who left" isn't a person, so what stays of a house
    // is a third space, which the people there can close.
    for table in ["spaces", "hangouts"] {
        let sql = format!(
            "SELECT id, name FROM {table} WHERE id IN (SELECT id FROM spaces WHERE owner_id = ?)"
        );
        let mut rows = conn.prepare(&sql)?;
        let rows: Vec<(String, String)> = rows
            .query_map([person_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (id, old) in rows {
            if let Some(new) = without_their_name(&old, name) {
                conn.execute(
                    &format!("UPDATE {table} SET name = ? WHERE id = ?"),
                    params![new, id],
                )?;
            }
        }
    }
    conn.execute(
        "UPDATE spaces SET owner_id = ?, kind = 'third' WHERE owner_id = ?",
        params![left, person_id],
    )?;
    Ok(())
}

/// An index made with `IF NOT EXISTS` keeps the definition it was first made with. When the schema
/// changes one, this drops the old one (if its definition lacks `wanted`) so the schema makes it again.
fn remake_index(conn: &Connection, name: &str, wanted: &str) -> rusqlite::Result<()> {
    let made: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = ?",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    if made.is_some_and(|sql| !sql.contains(wanted)) {
        conn.execute(&format!("DROP INDEX {name}"), [])?;
    }
    Ok(())
}

fn remember_leave(
    conn: &Connection,
    who: &str,
    hangout_id: &str,
    left_at: i64,
    reason: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO floor_leaves (who, hangout_id, left_at, reason) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (who, hangout_id) DO UPDATE SET
            left_at = excluded.left_at, reason = excluded.reason",
        params![who, hangout_id, left_at, reason],
    )?;
    Ok(())
}

fn floor_leave_from_row(row: &Row) -> rusqlite::Result<FloorLeave> {
    Ok(FloorLeave {
        hangout_id: row.get(0)?,
        name: row.get(1)?,
        left_at: row.get(2)?,
        reason: row.get(3)?,
    })
}

fn mention_from_row(row: &Row) -> rusqlite::Result<Mention> {
    Ok(Mention {
        hangout_id: row.get(0)?,
        hangout: row.get(1)?,
        message_id: row.get(2)?,
        from: row.get(3)?,
        text: row.get(4)?,
        at: row.get(5)?,
    })
}

impl Db {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        // MM5, MM9: recall's index of a bot's memories lives in the temporary database for one
        // search. Kept in memory, its words never reach a file on disk.
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        remake_index(&conn, "players_one_own_per_person", "removed = 0")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            outside_map: Mutex::new(None),
            root_lookups: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub(crate) fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // People

    pub fn create_person(
        &self,
        id: &str,
        name: &str,
        key_hash: &str,
        feed_token: &str,
        now: i64,
    ) -> rusqlite::Result<Person> {
        insert_person(&self.conn(), id, name, key_hash, feed_token, now)
    }

    pub fn person_count(&self) -> rusqlite::Result<i64> {
        self.conn().query_row("SELECT COUNT(*) FROM people", [], |row| row.get(0))
    }

    pub fn person(&self, id: &str) -> rusqlite::Result<Option<Person>> {
        self.conn()
            .query_row("SELECT id, name, color FROM people WHERE id = ?", [id], person_from_row)
            .optional()
    }

    /// S67: people whose display name matches, ASCII case not mattering.
    pub fn people_named(&self, name: &str) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, color FROM people WHERE name = ?1 COLLATE NOCASE
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([name], person_from_row)?;
        rows.collect()
    }

    pub fn person_by_key(&self, key_hash: &str) -> rusqlite::Result<Option<Person>> {
        self.conn()
            .query_row("SELECT id, name, color FROM people WHERE key_hash = ?", [key_hash], |row| {
                Ok(Person { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? })
            })
            .optional()
    }

    pub fn person_by_feed(&self, feed_token: &str) -> rusqlite::Result<Option<Person>> {
        self.conn()
            .query_row(
                "SELECT id, name, color FROM people WHERE feed_token = ?",
                [feed_token],
                |row| Ok(Person { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? }),
            )
            .optional()
    }

    pub fn feed_token(&self, person_id: &str) -> rusqlite::Result<String> {
        self.conn()
            .query_row("SELECT feed_token FROM people WHERE id = ?", [person_id], |row| row.get(0))
    }

    pub fn set_feed_token(&self, person_id: &str, feed_token: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE people SET feed_token = ? WHERE id = ?",
            params![feed_token, person_id],
        )?;
        Ok(())
    }

    /// Replaces a person's owner key and ends every session they have, in one step. Passkeys stay.
    pub fn replace_owner_key(&self, person_id: &str, key_hash: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE people SET key_hash = ? WHERE id = ?", params![key_hash, person_id])?;
        tx.execute("DELETE FROM sessions WHERE person_id = ?", [person_id])?;
        tx.commit()
    }

    pub fn people_in(&self, hangout_id: &str) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.color FROM people p
             JOIN members m ON m.person_id = p.id
             WHERE m.hangout_id = ? ORDER BY m.joined_at, p.id",
        )?;
        let rows = stmt.query_map([hangout_id], |row| {
            Ok(Person { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? })
        })?;
        rows.collect()
    }

    // Sessions

    /// Starts a session. Lapsed sessions go too, and so do this person's oldest past the cap.
    pub fn create_session(
        &self,
        token_hash: &str,
        person_id: &str,
        expires_at: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sessions WHERE expires_at <= ?", [now])?;
        tx.execute(
            "INSERT INTO sessions (token_hash, person_id, expires_at, created_at) VALUES (?, ?, ?, ?)",
            params![token_hash, person_id, expires_at, now],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE person_id = ?1 AND token_hash NOT IN
               (SELECT token_hash FROM sessions WHERE person_id = ?1
                ORDER BY created_at DESC, rowid DESC LIMIT ?2)",
            params![person_id, MAX_SESSIONS_PER_PERSON],
        )?;
        tx.commit()
    }

    /// Whose session this is, unless it has lapsed or been ended.
    pub fn person_by_session(
        &self,
        token_hash: &str,
        now: i64,
    ) -> rusqlite::Result<Option<Person>> {
        self.conn()
            .query_row(
                "SELECT p.id, p.name, p.color FROM sessions s JOIN people p ON p.id = s.person_id
                 WHERE s.token_hash = ? AND s.expires_at > ?",
                params![token_hash, now],
                person_from_row,
            )
            .optional()
    }

    pub fn delete_session(&self, token_hash: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM sessions WHERE token_hash = ?", [token_hash])?;
        Ok(())
    }

    pub fn mark_sign_in_session(&self, token_hash: &str, site: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO sign_in_sessions (token_hash, site) VALUES (?, ?)",
            params![token_hash, site],
        )?;
        Ok(())
    }

    pub fn is_sign_in_session(&self, token_hash: &str) -> rusqlite::Result<bool> {
        Ok(self.sign_in_session_site(token_hash)?.is_some())
    }

    /// The site that started this session, if one did.
    pub fn sign_in_session_site(&self, token_hash: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT site FROM sign_in_sessions WHERE token_hash = ?",
                [token_hash],
                |row| row.get(0),
            )
            .optional()
    }

    /// A person's links, as site and account name.
    pub fn sign_in_links(&self, person_id: &str) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT site, account_name FROM sign_in_links WHERE person_id = ?")?;
        let rows = stmt.query_map([person_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Links an account to this person. False if someone else has it, which leaves theirs as it was
    /// (A27). A different account takes the old one's place, and what that one started ends.
    pub fn set_sign_in_link(
        &self,
        person_id: &str,
        site: &str,
        account_id: &str,
        account_name: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let holder: Option<String> = tx
            .query_row(
                "SELECT person_id FROM sign_in_links WHERE site = ? AND account_id = ?",
                [site, account_id],
                |row| row.get(0),
            )
            .optional()?;
        if holder.is_some_and(|holder| holder != person_id) {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM sessions WHERE person_id = ?1
               AND token_hash IN (SELECT token_hash FROM sign_in_sessions WHERE site = ?2)
               AND EXISTS (SELECT 1 FROM sign_in_links l
                   WHERE l.person_id = ?1 AND l.site = ?2 AND l.account_id <> ?3)",
            [person_id, site, account_id],
        )?;
        // A93: a different Discord account starts hidden until they say yes again, even going back to the first.
        if site == "discord" {
            tx.execute(
                "DELETE FROM discord_display WHERE person_id = ? AND account_id <> ?",
                [person_id, account_id],
            )?;
        }
        if site == "github" {
            let old: Option<String> = tx
                .query_row(
                    "SELECT account_id FROM sign_in_links WHERE person_id=? AND site='github'",
                    [person_id],
                    |r| r.get(0),
                )
                .optional()?;
            if old.as_deref() != Some(account_id) {
                tx.execute(
                    "DELETE FROM github_display_refresh_claims WHERE person_id=?",
                    [person_id],
                )?;
                tx.execute("DELETE FROM github_display WHERE person_id=?", [person_id])?;
                if let Some(old) = old {
                    tx.execute("DELETE FROM github_display_cache WHERE github_user_id=?", [old])?;
                }
                tx.execute(
                    "DELETE FROM github_display_cache WHERE github_user_id=?",
                    [account_id],
                )?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO sign_in_links
                 (person_id, site, account_id, account_name, linked_at)
             VALUES (?, ?, ?, ?, ?)",
            params![person_id, site, account_id, account_name, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Who this account signs in as, with their username brought up to date (A33).
    pub fn person_by_sign_in(
        &self,
        site: &str,
        account_id: &str,
        account_name: &str,
    ) -> rusqlite::Result<Option<Person>> {
        let person_id: Option<String> = self
            .conn()
            .query_row(
                "UPDATE sign_in_links SET account_name = ?3 WHERE site = ?1 AND account_id = ?2
                 RETURNING person_id",
                [site, account_id, account_name],
                |row| row.get(0),
            )
            .optional()?;
        match person_id {
            Some(id) => self.person(&id),
            None => Ok(None),
        }
    }

    /// Makes a new account and its link together (A59). `None` means someone else has that account
    /// linked by now, which leaves nothing behind (A61).
    #[allow(clippy::too_many_arguments)]
    pub fn create_account_with_sign_in(
        &self,
        id: &str,
        name: &str,
        key_hash: &str,
        feed_token: &str,
        site: &str,
        account_id: &str,
        account_name: &str,
        now: i64,
    ) -> rusqlite::Result<Option<Person>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let person = insert_person(&tx, id, name, key_hash, feed_token, now)?;
        let linked = tx.execute(
            "INSERT OR IGNORE INTO sign_in_links
                 (person_id, site, account_id, account_name, linked_at)
             VALUES (?, ?, ?, ?, ?)",
            params![id, site, account_id, account_name, now],
        )?;
        if linked == 0 {
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some(person))
    }

    /// A36: the link and every session it started. A73: a GitHub link's display switches
    /// and cache row go in the same transaction, so unlinking can't leave either behind.
    pub fn remove_sign_in_link(&self, person_id: &str, site: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let github_account: Option<String> = if site == "github" {
            tx.query_row(
                "SELECT account_id FROM sign_in_links WHERE person_id = ? AND site = 'github'",
                [person_id],
                |row| row.get(0),
            )
            .optional()?
        } else {
            None
        };
        tx.execute(
            "DELETE FROM sessions WHERE person_id = ?1
               AND token_hash IN (SELECT token_hash FROM sign_in_sessions WHERE site = ?2)",
            [person_id, site],
        )?;
        tx.execute(
            "DELETE FROM sign_in_links WHERE person_id = ? AND site = ?",
            [person_id, site],
        )?;
        // A93: unlinking Discord takes the choice to show it along, so linking the same account
        // again shows nothing until they say yes again.
        if site == "discord" {
            tx.execute("DELETE FROM discord_display WHERE person_id = ?", [person_id])?;
        }
        if let Some(account_id) = github_account {
            tx.execute("DELETE FROM github_display_refresh_claims WHERE person_id=?", [person_id])?;
            tx.execute("DELETE FROM github_display WHERE person_id = ?", [person_id])?;
            tx.execute("DELETE FROM github_display_cache WHERE github_user_id = ?", [account_id])?;
        }
        tx.commit()
    }

    pub fn session_count(&self, person_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM sessions WHERE person_id = ?",
            [person_id],
            |row| row.get(0),
        )
    }

    // GitHub display

    /// A70: what this person chose to show of their linked GitHub account. Nothing when they
    /// never turned anything on.
    pub fn github_display_for(&self, person_id: &str) -> rusqlite::Result<Option<GitHubDisplay>> {
        self.conn()
            .query_row(
                "SELECT show_repos, show_stars, show_languages, show_badge FROM github_display
                 WHERE person_id = ?",
                [person_id],
                |row| {
                    Ok(GitHubDisplay {
                        show_repos: row.get::<_, i64>(0)? != 0,
                        show_stars: row.get::<_, i64>(1)? != 0,
                        show_languages: row.get::<_, i64>(2)? != 0,
                        show_badge: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()
    }

    /// A70: stores what this person chose to show.
    pub fn set_github_display(
        &self,
        person_id: &str,
        display: &GitHubDisplay,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE github_display_cache SET last_attempt_at=0 WHERE github_user_id IN (SELECT github_user_id FROM github_display_refresh_claims WHERE person_id=?)", [person_id])?;
        tx.execute("DELETE FROM github_display_refresh_claims WHERE person_id=?", [person_id])?;
        tx.execute(
            "INSERT INTO github_display
                 (person_id, show_repos, show_stars, show_languages, show_badge, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(person_id) DO UPDATE SET
                 show_repos = excluded.show_repos,
                 show_stars = excluded.show_stars,
                 show_languages = excluded.show_languages,
                 show_badge = excluded.show_badge,
                 updated_at = excluded.updated_at",
            params![
                person_id,
                display.show_repos as i64,
                display.show_stars as i64,
                display.show_languages as i64,
                display.show_badge as i64,
                now,
            ],
        )?;
        tx.commit()
    }

    /// The GitHub account this person linked, as its numeric id and username, if any.
    pub fn github_link(&self, person_id: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT account_id, account_name FROM sign_in_links
                 WHERE person_id = ? AND site = 'github'",
                [person_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// A72: the cached public display data for one GitHub account id.
    pub fn github_display_cache_for(
        &self,
        github_user_id: &str,
    ) -> rusqlite::Result<Option<CachedDisplay>> {
        self.conn()
            .query_row(
                "SELECT github_user_id, github_login, repos_json, top_languages_json,
                        merged_pr_count, fetched_at, repos_etag, search_etag, last_attempt_at
                 FROM github_display_cache WHERE github_user_id = ?",
                [github_user_id],
                |row| {
                    Ok(CachedDisplay {
                        github_user_id: row.get(0)?,
                        github_login: row.get(1)?,
                        repos_json: row.get(2)?,
                        top_languages_json: row.get(3)?,
                        merged_pr_count: row.get(4)?,
                        fetched_at: row.get(5)?,
                        repos_etag: row.get(6)?,
                        search_etag: row.get(7)?,
                        last_attempt_at: row.get(8)?,
                    })
                },
            )
            .optional()
    }

    /// A72: stores a refreshed cache row, whole.
    pub fn set_github_display_cache(&self, cache: &CachedDisplay) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO github_display_cache
                 (github_user_id, github_login, repos_json, top_languages_json, merged_pr_count,
                  fetched_at, repos_etag, search_etag, last_attempt_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(github_user_id) DO UPDATE SET
                 github_login = excluded.github_login,
                 repos_json = excluded.repos_json,
                 top_languages_json = excluded.top_languages_json,
                 merged_pr_count = excluded.merged_pr_count,
                 fetched_at = excluded.fetched_at,
                 repos_etag = excluded.repos_etag,
                 search_etag = excluded.search_etag,
                 last_attempt_at = excluded.last_attempt_at",
            params![
                cache.github_user_id,
                cache.github_login,
                cache.repos_json,
                cache.top_languages_json,
                cache.merged_pr_count,
                cache.fetched_at,
                cache.repos_etag,
                cache.search_etag,
                cache.last_attempt_at,
            ],
        )?;
        Ok(())
    }

    /// A72: records a refresh attempt that failed, keeping the old data. A row that doesn't
    /// exist yet is made as a stub, so later views back off instead of trying again each time.
    pub fn note_github_display_attempt(
        &self,
        github_user_id: &str,
        github_login: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO github_display_cache
                 (github_user_id, github_login, last_attempt_at)
             VALUES (?, ?, ?)
             ON CONFLICT(github_user_id) DO UPDATE SET last_attempt_at = excluded.last_attempt_at",
            params![github_user_id, github_login, now],
        )?;
        Ok(())
    }

    /// A73: the display switches go with the link.
    pub fn delete_github_display(&self, person_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM github_display WHERE person_id = ?", [person_id])?;
        Ok(())
    }

    /// A73: the cache row goes with the link.
    pub fn delete_github_display_cache(&self, github_user_id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM github_display_cache WHERE github_user_id = ?",
            [github_user_id],
        )?;
        Ok(())
    }

    // Passkeys

    /// The handle an authenticator holds for this person. It's made once and then kept: `fresh`
    /// is used only if they have none yet.
    pub fn user_handle(&self, person_id: &str, fresh: &str) -> rusqlite::Result<String> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO passkey_users (person_id, user_handle) VALUES (?, ?)",
            params![person_id, fresh],
        )?;
        conn.query_row(
            "SELECT user_handle FROM passkey_users WHERE person_id = ?",
            [person_id],
            |row| row.get(0),
        )
    }

    pub fn passkey_count(&self, person_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM passkeys WHERE person_id = ?",
            [person_id],
            |row| row.get(0),
        )
    }

    pub fn passkeys_of(&self, person_id: &str) -> rusqlite::Result<Vec<PasskeyInfo>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, label, created_at, last_used_at FROM passkeys
             WHERE person_id = ? ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([person_id], |row| {
            Ok(PasskeyInfo {
                id: row.get(0)?,
                label: row.get(1)?,
                created_at: row.get(2)?,
                last_used_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// The credential ids (in hex) of a person's passkeys, so a new one can't repeat them.
    pub fn credential_ids_of(&self, person_id: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT credential_id FROM passkeys WHERE person_id = ? ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map([person_id], |row| row.get(0))?;
        rows.collect()
    }

    /// The stored passkey for a credential id (in hex), with the handle of the person it belongs to.
    pub fn passkey_by_credential(
        &self,
        credential_id: &str,
    ) -> rusqlite::Result<Option<StoredPasskey>> {
        self.conn()
            .query_row(
                "SELECT k.id, k.person_id, k.passkey, u.user_handle FROM passkeys k
                 JOIN passkey_users u ON u.person_id = k.person_id
                 WHERE k.credential_id = ?",
                [credential_id],
                |row| {
                    Ok(StoredPasskey {
                        id: row.get(0)?,
                        person_id: row.get(1)?,
                        passkey: row.get(2)?,
                        user_handle: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    /// Adds a passkey to someone who has an account already, unless that credential is stored
    /// (for anyone) or they hold `max` passkeys.
    pub fn add_passkey(
        &self,
        person_id: &str,
        passkey: &NewPasskey,
        max: i64,
        now: i64,
    ) -> rusqlite::Result<PasskeyAdded> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let added = insert_passkey(&tx, person_id, passkey, max, now)?;
        tx.commit()?;
        Ok(added)
    }

    /// Makes a new account and its first passkey together, so a sign-up that fails leaves
    /// nothing behind. `None` means the credential was already stored.
    #[allow(clippy::too_many_arguments)]
    pub fn create_account_with_passkey(
        &self,
        id: &str,
        name: &str,
        key_hash: &str,
        feed_token: &str,
        user_handle: &str,
        passkey: &NewPasskey,
        now: i64,
    ) -> rusqlite::Result<Option<Person>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let person = insert_person(&tx, id, name, key_hash, feed_token, now)?;
        tx.execute(
            "INSERT INTO passkey_users (person_id, user_handle) VALUES (?, ?)",
            params![id, user_handle],
        )?;
        if insert_passkey(&tx, id, passkey, 1, now)? != PasskeyAdded::Added {
            return Ok(None);
        }
        tx.commit()?;
        Ok(Some(person))
    }

    /// Records a sign-in: the time, and the library's updated passkey if it changed. This only
    /// writes if the stored passkey is still exactly the one that was checked, so two sign-ins
    /// can't move a counter backwards. `false` means it wasn't, and the sign-in must be refused.
    pub fn record_sign_in(
        &self,
        id: &str,
        checked: &str,
        updated: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let changed = match updated {
            Some(updated) => conn.execute(
                "UPDATE passkeys SET passkey = ?, last_used_at = ? WHERE id = ? AND passkey = ?",
                params![updated, now, id, checked],
            )?,
            None => conn.execute(
                "UPDATE passkeys SET last_used_at = ? WHERE id = ? AND passkey = ?",
                params![now, id, checked],
            )?,
        };
        Ok(changed == 1)
    }

    /// Removes one of this person's own passkeys, and in the same step ends every session of theirs
    /// but `keep`, the hash of whatever made the request. An owner key matches no session, so then
    /// none is kept. `false` if they have no such passkey, and then nothing is ended.
    pub fn remove_passkey(&self, person_id: &str, id: &str, keep: &str) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let removed = tx.execute(
            "DELETE FROM passkeys WHERE id = ? AND person_id = ?",
            params![id, person_id],
        )?;
        if removed == 1 {
            tx.execute(
                "DELETE FROM sessions WHERE person_id = ? AND token_hash != ?",
                params![person_id, keep],
            )?;
        }
        tx.commit()?;
        Ok(removed == 1)
    }

    // Hangouts

    pub fn create_hangout(
        &self,
        id: &str,
        name: &str,
        invite_code: &str,
        created_by: &str,
        now: i64,
    ) -> rusqlite::Result<Hangout> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        insert_space(&tx, id, "third", name, invite_code, created_by, now)?;
        tx.commit()?;
        Ok(Hangout {
            id: id.to_string(),
            name: name.to_string(),
            invite_code: invite_code.to_string(),
            created_by: created_by.to_string(),
        })
    }

    /// Starts a hangout unless this person already holds `max` third spaces, which is the same as
    /// a room with their name on it that they still belong to (S3).
    pub fn start_third_space(
        &self,
        id: &str,
        name: &str,
        invite_code: &str,
        created_by: &str,
        now: i64,
        max: i64,
    ) -> rusqlite::Result<Option<Hangout>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let held: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM {HELD_SPACES} WHERE s.owner_id = ?1 AND s.kind = 'third'"
            ),
            [created_by],
            |row| row.get(0),
        )?;
        if held >= max {
            return Ok(None);
        }
        insert_space(&tx, id, "third", name, invite_code, created_by, now)?;
        tx.commit()?;
        Ok(Some(Hangout {
            id: id.to_string(),
            name: name.to_string(),
            invite_code: invite_code.to_string(),
            created_by: created_by.to_string(),
        }))
    }

    pub fn hangout(&self, id: &str) -> rusqlite::Result<Option<Hangout>> {
        self.conn()
            .query_row(
                "SELECT id, name, invite_code, created_by FROM hangouts WHERE id = ?",
                [id],
                hangout_from_row,
            )
            .optional()
    }

    pub fn hangout_by_invite(&self, code: &str) -> rusqlite::Result<Option<Hangout>> {
        self.conn()
            .query_row(
                "SELECT id, name, invite_code, created_by FROM hangouts WHERE invite_code = ?",
                [code],
                hangout_from_row,
            )
            .optional()
    }

    pub fn set_invite_code(&self, hangout_id: &str, code: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE hangouts SET invite_code = ? WHERE id = ?", params![code, hangout_id])?;
        end_friend_invites(&tx, "room", hangout_id)?;
        tx.commit()
    }

    pub fn hangouts_for_person(&self, person_id: &str) -> rusqlite::Result<Vec<Hangout>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT h.id, h.name, h.invite_code, h.created_by FROM hangouts h
             JOIN members m ON m.hangout_id = h.id
             WHERE m.person_id = ? ORDER BY m.joined_at",
        )?;
        let rows = stmt.query_map([person_id], hangout_from_row)?;
        rows.collect()
    }

    /// The bot's rooms, including the Porch of any house it's visiting on a live trip and any public
    /// floor its owner sent it onto.
    pub fn hangouts_for_agent(&self, agent_id: &str, now: i64) -> rusqlite::Result<Vec<Hangout>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT h.id, h.name, h.invite_code, h.created_by FROM hangouts h
             WHERE h.id IN (SELECT hangout_id FROM room_agents WHERE agent_id = ?1)
                OR h.id IN (SELECT ho.porch FROM trips t JOIN houses ho ON ho.id = t.house_id
                            WHERE t.agent_id = ?1 AND t.status = 'here' AND t.ends_at > ?2)
                OR h.id IN (SELECT ho.living_room FROM trips t JOIN houses ho ON ho.id = t.house_id
                            JOIN passes pk ON pk.house_id = ho.id AND pk.agent_id = t.agent_id
                            JOIN pass_keys k ON k.pass_id = pk.id
                            WHERE t.agent_id = ?1 AND t.status = 'here' AND t.ends_at > ?2
                              AND pk.accepted_by IS NOT NULL AND k.key = 'living_room'
                              AND EXISTS (SELECT 1 FROM residents gr JOIN people gp ON gp.id = gr.person_id
                                          WHERE gr.house_id = ho.id AND gp.name = k.granted_by))
                OR h.id IN (SELECT v.hangout_id FROM floor_visits v
                            JOIN floors f ON f.hangout_id = v.hangout_id
                            WHERE v.agent_id = ?1 AND v.until > ?2 AND f.access = 'public')
             ORDER BY h.created_at, h.id",
        )?;
        let rows = stmt.query_map(params![agent_id, now], hangout_from_row)?;
        rows.collect()
    }

    /// L12, #583: makes a bot's owner a member of the project room it was brought into, so they can
    /// see where it went, and marks them as brought in, which isn't a tie (FR1). Someone who was
    /// already a member stays an ordinary one.
    pub fn add_brought_in_owner(
        &self,
        hangout_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let fresh = tx.execute(
            "INSERT OR IGNORE INTO members (hangout_id, person_id, joined_at) VALUES (?, ?, ?)",
            params![hangout_id, person_id, now],
        )?;
        if fresh > 0 {
            tx.execute(
                "INSERT OR IGNORE INTO brought_in_owners (hangout_id, person_id) VALUES (?, ?)",
                params![hangout_id, person_id],
            )?;
        }
        tx.commit()
    }

    /// Joining on purpose also makes someone brought in by L12 an ordinary member (#583).
    pub fn add_member(&self, hangout_id: &str, person_id: &str, now: i64) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO members (hangout_id, person_id, joined_at) VALUES (?, ?, ?)",
            params![hangout_id, person_id, now],
        )?;
        tx.execute(
            "DELETE FROM brought_in_owners WHERE hangout_id = ? AND person_id = ?",
            params![hangout_id, person_id],
        )?;
        tx.commit()
    }

    /// Leaving takes the person's bots out of the room with them.
    pub fn remove_member(&self, hangout_id: &str, person_id: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO floors_left (hangout_id, person_id)
             SELECT m.hangout_id, m.person_id FROM members m
             JOIN floors f ON f.hangout_id = m.hangout_id
             WHERE m.hangout_id = ?1 AND m.person_id = ?2",
            params![hangout_id, person_id],
        )?;
        tx.execute(
            "DELETE FROM room_agents WHERE hangout_id = ?1 AND agent_id IN
                (SELECT id FROM agents WHERE owner_id = ?2)",
            params![hangout_id, person_id],
        )?;
        tx.execute(
            "DELETE FROM members WHERE hangout_id = ? AND person_id = ?",
            params![hangout_id, person_id],
        )?;
        tx.commit()
    }

    pub fn is_member(&self, hangout_id: &str, person_id: &str) -> rusqlite::Result<bool> {
        self.conn()
            .query_row(
                "SELECT 1 FROM members WHERE hangout_id = ? AND person_id = ?",
                params![hangout_id, person_id],
                |_| Ok(()),
            )
            .optional()
            .map(|found| found.is_some())
    }

    /// S67: a friend invite to this room still waiting, not turned down, not ended, and from
    /// someone who still belongs. `since` is now minus how long an invite waits (FR6).
    pub fn invited_to_room(
        &self,
        hangout_id: &str,
        person_id: &str,
        since: i64,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM friend_invites f
             WHERE f.kind = 'room' AND f.place_id = ?1 AND f.to_person = ?2
               AND f.declined_at IS NULL AND f.created_at > ?3
               AND NOT EXISTS (SELECT 1 FROM friend_invite_ends e WHERE e.invite_id = f.id)
               AND NOT EXISTS (SELECT 1 FROM quiet_asks q WHERE q.kind = 'friend_invite' AND q.ask_id = f.id)
               AND NOT EXISTS (SELECT 1 FROM floors fl JOIN turned_out t ON t.space_id = fl.space_id
                               WHERE fl.hangout_id = ?1 AND t.person_id = ?2)
               AND EXISTS (SELECT 1 FROM members m
                           WHERE m.hangout_id = ?1 AND m.person_id = f.from_person)",
            params![hangout_id, person_id, since],
        )
    }

    /// BT5: whether a bot belongs to a room, not just visits it.
    pub fn agent_belongs_to_room(
        &self,
        hangout_id: &str,
        agent_id: &str,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM room_agents WHERE hangout_id = ?1 AND agent_id = ?2",
            params![hangout_id, agent_id],
        )
    }

    /// Whether the bot is in the room: placed there, visiting its Porch until `ends_at`, or sent onto
    /// a floor that is still public.
    pub fn agent_in_room(
        &self,
        hangout_id: &str,
        agent_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            &format!(
                "SELECT 1 WHERE EXISTS (SELECT 1 FROM room_agents WHERE hangout_id = ?1 AND agent_id = ?2)
                    OR EXISTS (SELECT 1 FROM trips t JOIN houses ho ON ho.id = t.house_id
                               WHERE {GUEST_ROOM} AND t.agent_id = ?2 AND t.status = 'here' AND t.ends_at > ?3)
                    OR EXISTS (SELECT 1 FROM floor_visits v JOIN floors f ON f.hangout_id = v.hangout_id
                               WHERE v.hangout_id = ?1 AND v.agent_id = ?2 AND v.until > ?3
                                 AND f.access = 'public')"
            ),
            params![hangout_id, agent_id, now],
        )
    }

    // Agents

    pub fn create_agent(
        &self,
        id: &str,
        owner_id: &str,
        name: &str,
        token_hash: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO agents (id, owner_id, name, token_hash, created_at) VALUES (?, ?, ?, ?, ?)",
            params![id, owner_id, name, token_hash, now],
        )?;
        Ok(())
    }

    pub fn agent_by_token(&self, token_hash: &str, now: i64) -> rusqlite::Result<Option<Agent>> {
        self.conn()
            .query_row(
                &format!("SELECT {AGENT_COLS} FROM {AGENTS} WHERE a.token_hash = ?"),
                [token_hash],
                |row| agent_from_row(row, now),
            )
            .optional()
    }

    pub fn agent(&self, id: &str, now: i64) -> rusqlite::Result<Option<Agent>> {
        self.conn()
            .query_row(&format!("SELECT {AGENT_COLS} FROM {AGENTS} WHERE a.id = ?"), [id], |row| {
                agent_from_row(row, now)
            })
            .optional()
    }

    pub fn agents_in(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<Agent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {AGENT_COLS} FROM {AGENTS}
             JOIN room_agents r ON r.agent_id = a.id
             WHERE r.hangout_id = ? ORDER BY a.created_at, a.id"
        ))?;
        let rows = stmt.query_map([hangout_id], |row| agent_from_row(row, now))?;
        rows.collect()
    }

    /// A person's bots, without a house bot they made for a house they've since left. It's the
    /// house's, and when it last ran would show them whether anyone is home (R30, R44).
    pub fn agents_of(&self, owner_id: &str, now: i64) -> rusqlite::Result<Vec<Agent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {AGENT_COLS} FROM {AGENTS} WHERE a.owner_id = ?1
               AND NOT EXISTS (SELECT 1 FROM bot_homes home
                   WHERE home.agent_id = a.id AND home.is_house = 1
                     AND NOT EXISTS (SELECT 1 FROM residents r
                         WHERE r.house_id = home.house_id AND r.person_id = ?1))
             ORDER BY a.created_at, a.id"
        ))?;
        let rows = stmt.query_map([owner_id], |row| agent_from_row(row, now))?;
        rows.collect()
    }

    /// S67: bots whose display name matches, ASCII case not mattering. A house bot is in no
    /// rooms and has no lookup.
    pub fn agents_named(&self, name: &str, now: i64) -> rusqlite::Result<Vec<Agent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {AGENT_COLS} FROM {AGENTS} WHERE a.name = ?1 COLLATE NOCASE
               AND NOT EXISTS (SELECT 1 FROM bot_homes home
                   WHERE home.agent_id = a.id AND home.is_house = 1)
             ORDER BY a.created_at, a.id"
        ))?;
        let rows = stmt.query_map([name], |row| agent_from_row(row, now))?;
        rows.collect()
    }

    pub fn discord_link(&self, agent_id: &str) -> rusqlite::Result<Option<DiscordLink>> {
        self.conn()
            .query_row(
                "SELECT discord_id, discord_name FROM discord_links WHERE agent_id = ?",
                [agent_id],
                |row| Ok(DiscordLink { id: row.get(0)?, name: row.get(1)? }),
            )
            .optional()
    }

    /// A19: a bot has one account, so this replaces whatever it had.
    pub fn set_discord_link(
        &self,
        agent_id: &str,
        discord_id: &str,
        discord_name: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO discord_links (agent_id, discord_id, discord_name, linked_at)
             VALUES (?, ?, ?, ?)",
            params![agent_id, discord_id, discord_name, now],
        )?;
        Ok(())
    }

    pub fn remove_discord_link(&self, agent_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM discord_links WHERE agent_id = ?", [agent_id])?;
        Ok(())
    }

    pub fn add_agent_to_room(&self, hangout_id: &str, agent_id: &str) -> rusqlite::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO room_agents (hangout_id, agent_id) VALUES (?, ?)",
            params![hangout_id, agent_id],
        )?;
        // K1: every way into a project comes through here, so this is where bonds are made.
        conn.execute(
            "INSERT OR IGNORE INTO bond_projects
                 (agent_a, agent_b, project_id, name, purpose, created_at)
             SELECT min(ra.agent_id, ?2), max(ra.agent_id, ?2), p.space_id, s.name, s.about, ?3
             FROM projects p
             JOIN spaces s ON s.id = p.space_id
             JOIN room_agents ra ON ra.hangout_id = p.space_id
             WHERE p.space_id = ?1 AND ra.agent_id <> ?2",
            params![hangout_id, agent_id, crate::now()],
        )?;
        Ok(())
    }

    pub fn remove_agent_from_room(&self, hangout_id: &str, agent_id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM room_agents WHERE hangout_id = ? AND agent_id = ?",
            params![hangout_id, agent_id],
        )?;
        Ok(())
    }

    /// E29: opting out ends all public memberships and visits together, with a reason for the bot.
    pub fn leave_public_rooms(&self, agent_id: &str, now: i64) -> rusqlite::Result<Vec<String>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let rooms = strings(&tx,
            "SELECT f.hangout_id FROM floors f WHERE f.access = 'public' AND (
                EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = f.hangout_id AND r.agent_id = ?1)
                OR EXISTS (SELECT 1 FROM floor_visits v WHERE v.hangout_id = f.hangout_id
                           AND (v.who = ?1 OR v.agent_id = ?1)))", [agent_id])?;
        for room in &rooms {
            tx.execute(
                "DELETE FROM room_agents WHERE hangout_id = ?1 AND agent_id = ?2",
                params![room, agent_id],
            )?;
            tx.execute(
                "DELETE FROM floor_visits WHERE hangout_id = ?1 AND (who = ?2 OR agent_id = ?2)",
                params![room, agent_id],
            )?;
            tx.execute(
                "DELETE FROM bot_places WHERE hangout_id = ?1 AND agent_id = ?2",
                params![room, agent_id],
            )?;
            remember_leave(
                &tx,
                agent_id,
                room,
                now,
                "public rooms require conversations; your owner turned them off or paused you",
            )?;
        }
        tx.commit()?;
        Ok(rooms)
    }

    /// Replaces a bot's token. Its public player's token goes in the same step (W3): the new bot
    /// token gets a new one by joining again.
    pub fn set_agent_token(&self, agent_id: &str, token_hash: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE agents SET token_hash = ? WHERE id = ?", params![token_hash, agent_id])?;
        end_bot_player_token(&tx, agent_id)?;
        tx.commit()
    }

    /// Stops the token of a bot's public player until the bot joins again (W3).
    pub fn end_bot_player_token(&self, agent_id: &str) -> rusqlite::Result<()> {
        end_bot_player_token(&self.conn(), agent_id)
    }

    /// A78 to A80: deletes one bot. Everything that's only its own goes with its row, and what it
    /// said stays under the name it had. Its player on The Commons stops, and keeps what it built.
    pub fn delete_agent(&self, agent_id: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        delete_agent_in(&tx, agent_id)?;
        tx.commit()
    }

    pub fn touch_agent_seen(&self, agent_id: &str, now: i64) -> rusqlite::Result<()> {
        self.conn()
            .execute("UPDATE agents SET last_seen_at = ? WHERE id = ?", params![now, agent_id])?;
        Ok(())
    }

    pub fn touch_agent_active(&self, agent_id: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE agents SET last_active_at = ?1, last_seen_at = ?1 WHERE id = ?2",
            params![now, agent_id],
        )?;
        Ok(())
    }

    pub fn set_agent_status(&self, agent_id: &str, status: Option<&str>) -> rusqlite::Result<()> {
        self.conn()
            .execute("UPDATE agents SET status = ? WHERE id = ?", params![status, agent_id])?;
        Ok(())
    }

    /// I85: ends what a bot said it's doing. False if nothing was showing.
    pub fn clear_activity(&self, agent_id: &str) -> rusqlite::Result<bool> {
        let gone =
            self.conn().execute("DELETE FROM agent_activity WHERE agent_id = ?", [agent_id])?;
        Ok(gone == 1)
    }

    /// Moves what a bot said it's doing back in time, for tests of it running out (I85).
    pub fn age_activity(&self, agent_id: &str, by: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE agent_activity SET until_at = until_at - ?2 WHERE agent_id = ?1",
            params![agent_id, by],
        )?;
        Ok(())
    }

    pub fn record_activity(
        &self,
        agent_id: &str,
        kind: &str,
        via: &str,
        until: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO agent_activity (agent_id, kind, via, until_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (agent_id) DO UPDATE SET
                kind = excluded.kind, via = excluded.via, until_at = excluded.until_at",
            params![agent_id, kind, via, until],
        )?;
        Ok(())
    }

    /// Records that the bot's doorbell is running, and returns whether its owner has paused it.
    pub fn note_doorbell(&self, agent_id: &str, now: i64) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "INSERT INTO agent_doorbell (agent_id, checked_at) VALUES (?, ?)
             ON CONFLICT (agent_id) DO UPDATE SET checked_at = excluded.checked_at
             RETURNING paused",
            params![agent_id, now],
            |row| row.get(0),
        )
    }

    pub fn set_doorbell_paused(&self, agent_id: &str, paused: bool) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO agent_doorbell (agent_id, paused) VALUES (?, ?)
             ON CONFLICT (agent_id) DO UPDATE SET paused = excluded.paused",
            params![agent_id, paused],
        )?;
        Ok(())
    }

    /// E22: whether its owner pressed Pause. A bot with no doorbell can be paused too.
    pub fn doorbell_paused(&self, agent_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM agent_doorbell WHERE agent_id = ? AND paused = 1",
            [agent_id],
        )
    }

    // Messages

    pub fn insert_message(&self, message: &NewMessage) -> rusqlite::Result<Message> {
        insert_message_row(&self.conn(), message)
    }

    /// I82: whether this writer can answer a chat line here. `seen_after` is a guest's arrival:
    /// a guest can answer only the lines they can read, which are the ones after it.
    pub fn can_answer(
        &self,
        hangout_id: &str,
        id: i64,
        seen_after: Option<i64>,
    ) -> rusqlite::Result<bool> {
        answerable(&self.conn(), hangout_id, id, seen_after)
    }

    /// I82: stores a chat line that answers another, checking the answered line in the same step,
    /// so the line and its link are written together or not at all. None, and nothing written, if
    /// the answered line can't be answered (any more).
    pub fn insert_reply(
        &self,
        message: &NewMessage,
        reply_to: i64,
        seen_after: Option<i64>,
    ) -> rusqlite::Result<Option<Message>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !answerable(&tx, message.hangout_id, reply_to, seen_after)? {
            return Ok(None);
        }
        let mut stored = insert_message_row(&tx, message)?;
        tx.execute(
            "INSERT INTO message_replies (message_id, reply_to) VALUES (?, ?)",
            params![stored.id, reply_to],
        )?;
        let root_id: i64 = tx
            .query_row(
                "SELECT root_id FROM message_threads WHERE message_id = ?",
                params![reply_to],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(reply_to);
        tx.execute(
            "INSERT OR IGNORE INTO message_threads (message_id, root_id) VALUES (?, ?)",
            params![root_id, root_id],
        )?;
        tx.execute(
            "INSERT INTO message_threads (message_id, root_id) VALUES (?, ?)",
            params![stored.id, root_id],
        )?;
        tx.commit()?;
        stored.reply_to = Some(reply_to);
        stored.thread_root = Some(root_id);
        Ok(Some(stored))
    }

    /// The newest `limit` chat lines after `after`, oldest first.
    pub fn messages(
        &self,
        hangout_id: &str,
        after: i64,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE hangout_id = ? AND id > ? AND kind != 'note'
             ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map(params![hangout_id, after, limit], message_from_row)?;
        let mut messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    pub fn notes(&self, hangout_id: &str, limit: i64) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE hangout_id = ? AND kind = 'note' ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map(params![hangout_id, limit], message_from_row)?;
        rows.collect()
    }

    pub fn message_count_since(&self, hangout_id: &str, since: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM messages WHERE hangout_id = ? AND created_at >= ? AND kind != 'system'",
            params![hangout_id, since],
            |row| row.get(0),
        )
    }

    // Check-ins

    pub fn insert_check_in(&self, check_in: &NewCheckIn) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO check_ins (id, hangout_id, asked_by, asker, agent_id, about, to_person,
                                    needs_human, status, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'waiting', ?, ?)",
            params![
                check_in.id,
                check_in.hangout_id,
                check_in.asked_by,
                check_in.asker,
                check_in.agent_id,
                check_in.about,
                check_in.to_person,
                check_in.needs_human,
                check_in.expires_at,
                check_in.now
            ],
        )?;
        Ok(())
    }

    pub fn check_in(&self, id: &str, now: i64) -> rusqlite::Result<Option<CheckIn>> {
        self.conn()
            .query_row(
                &format!("SELECT {CHECK_IN_COLS} FROM check_ins WHERE id = ?"),
                [id],
                |row| check_in_from_row(row, now),
            )
            .optional()
    }

    /// Open questions, plus anything from the last day, newest first.
    pub fn recent_check_ins(
        &self,
        hangout_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<CheckIn>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CHECK_IN_COLS} FROM check_ins
             WHERE hangout_id = ?1 AND (created_at >= ?2 OR (status = 'waiting' AND expires_at > ?3))
             ORDER BY created_at DESC, id LIMIT 50"
        ))?;
        let rows =
            stmt.query_map(params![hangout_id, since, now], |row| check_in_from_row(row, now))?;
        rows.collect()
    }

    /// N13: retain handling time independently from when the question was asked.
    pub fn handled_check_ins(
        &self,
        hangout_id: &str,
        person_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<(CheckIn, Option<i64>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CHECK_IN_COLS}, CASE
                WHEN status = 'waiting' AND expires_at <= ?3 THEN expires_at
                ELSE (SELECT answered_at FROM check_in_answers WHERE check_in_id = check_ins.id)
             END AS handled_at
             FROM check_ins
             WHERE hangout_id = ?1 AND (
                 handled_at >= ?2
                 OR (status = 'waiting' AND expires_at > ?3 AND asked_by = ?4)
                 OR (status != 'waiting' AND answered_by_bot = 1 AND asked_by != ?4
                     AND (to_person IS NULL OR to_person = ?4))
                 OR (status != 'waiting' AND handled_at IS NULL AND created_at >= ?2))
             ORDER BY COALESCE(handled_at, created_at) DESC, id LIMIT 50"
        ))?;
        let rows = stmt.query_map(params![hangout_id, since, now, person_id], |row| {
            Ok((check_in_from_row(row, now)?, row.get(14)?))
        })?;
        rows.collect()
    }

    pub fn answer_check_in(
        &self,
        id: &str,
        status: &str,
        answered_by: &str,
        by_bot: bool,
        note: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE check_ins SET status = ?, answered_by = ?, answered_by_bot = ?, note = ? WHERE id = ?",
            params![status, answered_by, by_bot, note, id],
        )?;
        tx.execute(
            "INSERT INTO check_in_answers (check_in_id, answered_at) VALUES (?1, ?2)
             ON CONFLICT(check_in_id) DO UPDATE SET answered_at = excluded.answered_at",
            params![id, now],
        )?;
        tx.commit()
    }

    // Questions for a bot's owner. `open_since` is the oldest a question can be and still be open.

    /// Adds a question, unless the bot already has `max_open` waiting. It's one statement so two
    /// asked at once can't both take the last place.
    pub fn insert_owner_question(
        &self,
        question: &NewOwnerQuestion,
        open_since: i64,
        max_open: i64,
    ) -> rusqlite::Result<bool> {
        let added = self.conn().execute(
            "INSERT INTO owner_questions (id, hangout_id, agent_id, owner_id, asker, question, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
             WHERE (SELECT COUNT(*) FROM owner_questions
                    WHERE agent_id = ?3 AND answer IS NULL AND created_at > ?8
                    AND id NOT IN (SELECT question_id FROM owner_question_closings)) < ?9",
            params![
                question.id,
                question.hangout_id,
                question.agent_id,
                question.owner_id,
                question.asker,
                question.question,
                question.now,
                open_since,
                max_open
            ],
        )?;
        Ok(added == 1)
    }

    pub fn owner_question(&self, id: &str) -> rusqlite::Result<Option<OwnerQuestion>> {
        self.conn()
            .query_row(
                &format!("SELECT {OWNER_QUESTION_COLS} FROM owner_questions WHERE id = ?"),
                [id],
                owner_question_from_row,
            )
            .optional()
    }

    /// What this person's bots have asked them in this room and they haven't answered, oldest first.
    pub fn open_owner_questions(
        &self,
        hangout_id: &str,
        owner_id: &str,
        open_since: i64,
    ) -> rusqlite::Result<Vec<OwnerQuestion>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OWNER_QUESTION_COLS} FROM owner_questions
             WHERE hangout_id = ? AND owner_id = ? AND answer IS NULL AND created_at > ?
             AND id NOT IN (SELECT question_id FROM owner_question_closings)
             ORDER BY created_at, rowid"
        ))?;
        let rows =
            stmt.query_map(params![hangout_id, owner_id, open_since], owner_question_from_row)?;
        rows.collect()
    }

    /// N2: every open question for this owner, in any room, with the room's name, oldest first.
    pub fn open_owner_questions_anywhere(
        &self,
        owner_id: &str,
        open_since: i64,
    ) -> rusqlite::Result<Vec<(OwnerQuestion, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OWNER_QUESTION_COLS},
                    (SELECT name FROM hangouts WHERE id = owner_questions.hangout_id)
             FROM owner_questions
             WHERE owner_id = ? AND answer IS NULL AND created_at > ?
             AND id NOT IN (SELECT question_id FROM owner_question_closings)
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![owner_id, open_since], |row| {
            Ok((
                owner_question_from_row(row)?,
                row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            ))
        })?;
        rows.collect()
    }

    /// N5, N7: says this bot is waiting for its owner, replacing what it said before. True when
    /// that starts a wait, and false when one was already showing.
    pub fn set_owner_wait(
        &self,
        agent_id: &str,
        about: &str,
        now: i64,
        shown_since: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let showing = conn
            .query_row(
                &format!("SELECT 1 FROM owner_waits w {OWNER_WAIT_SHOWING} AND w.agent_id = ?2"),
                params![shown_since, agent_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        conn.execute(
            "INSERT INTO owner_waits (agent_id, about, set_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(agent_id) DO UPDATE SET about = excluded.about, set_at = excluded.set_at",
            params![agent_id, about, now],
        )?;
        Ok(!showing)
    }

    /// Moves a wait's clock back, and its bot's last write with it, for tests of how long one
    /// shows (N6).
    pub fn age_owner_wait(&self, agent_id: &str, by: i64) -> rusqlite::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE owner_waits SET set_at = set_at - ?2 WHERE agent_id = ?1",
            params![agent_id, by],
        )?;
        conn.execute(
            "UPDATE agents SET last_active_at = last_active_at - ?2 WHERE id = ?1",
            params![agent_id, by],
        )?;
        Ok(())
    }

    /// Moves a bot's last call to the server back, for tests of how quiet it reads (I84).
    pub fn age_agent_seen(&self, agent_id: &str, by: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE agents SET last_seen_at = last_seen_at - ?2 WHERE id = ?1",
            params![agent_id, by],
        )?;
        Ok(())
    }

    /// N6: clears what a bot said about waiting, whether it was still showing or not.
    pub fn clear_owner_wait(&self, agent_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM owner_waits WHERE agent_id = ?", [agent_id])?;
        Ok(())
    }

    /// N2, N5: this owner's bots that are waiting for them, oldest first.
    pub fn owner_waits(
        &self,
        owner_id: &str,
        shown_since: i64,
    ) -> rusqlite::Result<Vec<OwnerWait>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT w.agent_id, a.name, w.about, w.set_at FROM owner_waits w {OWNER_WAIT_SHOWING}
             AND a.owner_id = ?2 ORDER BY w.set_at, w.agent_id"
        ))?;
        let rows = stmt.query_map(params![shown_since, owner_id], |row| {
            Ok(OwnerWait {
                agent_id: row.get(0)?,
                name: row.get(1)?,
                about: row.get(2)?,
                since: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// What this bot has asked and hasn't heard back about, oldest first.
    pub fn open_questions_of_bot(
        &self,
        agent_id: &str,
        open_since: i64,
    ) -> rusqlite::Result<Vec<OwnerQuestion>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OWNER_QUESTION_COLS} FROM owner_questions
             WHERE agent_id = ? AND answer IS NULL AND created_at > ?
             AND id NOT IN (SELECT question_id FROM owner_question_closings)
             ORDER BY created_at, rowid"
        ))?;
        let rows = stmt.query_map(params![agent_id, open_since], owner_question_from_row)?;
        rows.collect()
    }

    /// This bot's questions that were answered since `since`, newest first.
    pub fn owner_answers(
        &self,
        agent_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<OwnerQuestion>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OWNER_QUESTION_COLS} FROM owner_questions
             WHERE agent_id = ? AND answered_at >= ?
             ORDER BY answered_at DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map(params![agent_id, since], owner_question_from_row)?;
        rows.collect()
    }

    /// MC6: which of these answered questions the bot's inbox first showed it before `before`.
    pub fn seen_owner_answers(
        &self,
        ids: &[&str],
        before: i64,
    ) -> rusqlite::Result<HashSet<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT 1 FROM owner_answers_seen WHERE question_id = ? AND seen_at < ?")?;
        let mut seen = HashSet::new();
        for id in ids {
            if stmt.exists(params![id, before])? {
                seen.insert(id.to_string());
            }
        }
        Ok(seen)
    }

    /// MC6: the bot's inbox has shown it these answers. The first time shown is kept.
    pub fn mark_owner_answers_seen(&self, ids: &[&str], now: i64) -> rusqlite::Result<()> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "INSERT OR IGNORE INTO owner_answers_seen (question_id, seen_at) VALUES (?, ?)",
        )?;
        for id in ids {
            stmt.execute(params![id, now])?;
        }
        Ok(())
    }

    /// MC6: a room's newest message id, for ordering a bot's projects by what's going on. Ids only
    /// grow, so this orders rooms by their latest message, in one seek of messages_by_room.
    pub fn last_message_id(&self, hangout_id: &str) -> rusqlite::Result<Option<i64>> {
        self.conn().query_row(
            "SELECT MAX(id) FROM messages WHERE hangout_id = ?",
            [hangout_id],
            |row| row.get(0),
        )
    }

    /// N13: this owner's questions answered or closed since then, newest first, each with its
    /// room's name and how it was closed, if it was.
    pub fn handled_owner_questions(
        &self,
        owner_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<(OwnerQuestion, String, Option<String>)>> {
        let conn = self.conn();
        let cols = OWNER_QUESTION_COLS
            .split(", ")
            .map(|col| format!("q.{col}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT {cols}, (SELECT name FROM hangouts WHERE id = q.hangout_id), c.how
             FROM owner_questions q
             LEFT JOIN owner_question_closings c ON c.question_id = q.id
             WHERE q.owner_id = ?1 AND (q.answered_at >= ?2 OR c.closed_at >= ?2)
             ORDER BY COALESCE(q.answered_at, c.closed_at) DESC, q.rowid DESC"
        ))?;
        let rows = stmt.query_map(params![owner_id, since], |row| {
            Ok((
                owner_question_from_row(row)?,
                row.get::<_, Option<String>>(9)?.unwrap_or_default(),
                row.get(10)?,
            ))
        })?;
        rows.collect()
    }

    /// N12: remembers that this message mentioned this person.
    pub fn add_person_mention(
        &self,
        person_id: &str,
        hangout_id: &str,
        message_id: i64,
        at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO person_mentions (person_id, hangout_id, message_id, at)
             VALUES (?1, ?2, ?3, ?4)",
            params![person_id, hangout_id, message_id, at],
        )?;
        Ok(())
    }

    /// N12: the mentions waiting for this person since then, newest first: the mention's id, the
    /// room's id and name, who said it, what they said and when.
    pub fn person_mentions(
        &self,
        person_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<PersonMention>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT pm.id, pm.hangout_id, h.name, m.speaker, m.body, pm.at
             FROM person_mentions pm
             JOIN messages m ON m.id = pm.message_id
             JOIN hangouts h ON h.id = pm.hangout_id
             JOIN members mb ON mb.hangout_id = pm.hangout_id AND mb.person_id = pm.person_id
             WHERE pm.person_id = ?1 AND pm.at > ?2
             ORDER BY pm.at DESC, pm.id DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![person_id, since], |row| {
            Ok(PersonMention {
                id: row.get(0)?,
                hangout_id: row.get(1)?,
                room: row.get(2)?,
                by: row.get(3)?,
                text: row.get(4)?,
                at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// N12: opening a room clears its mentions of this person.
    pub fn clear_person_mentions_in(
        &self,
        person_id: &str,
        hangout_id: &str,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM person_mentions WHERE person_id = ? AND hangout_id = ?",
            params![person_id, hangout_id],
        )?;
        Ok(())
    }

    /// N12: clears one mention, if it's this person's. False when it isn't there.
    pub fn delete_person_mention(&self, id: i64, person_id: &str) -> rusqlite::Result<bool> {
        let gone = self.conn().execute(
            "DELETE FROM person_mentions WHERE id = ? AND person_id = ?",
            params![id, person_id],
        )?;
        Ok(gone == 1)
    }

    /// R51, R52: closes an open question without an answer. False when it was already answered or
    /// closed.
    pub fn close_owner_question(&self, id: &str, how: &str, now: i64) -> rusqlite::Result<bool> {
        let closed = self.conn().execute(
            "INSERT INTO owner_question_closings (question_id, how, closed_at)
             SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM owner_questions WHERE id = ?1 AND answer IS NULL)
             ON CONFLICT(question_id) DO NOTHING",
            params![id, how, now],
        )?;
        Ok(closed == 1)
    }

    /// How a question was closed without an answer, if it was (R51, R52).
    pub fn owner_question_closing(&self, id: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT how FROM owner_question_closings WHERE question_id = ?",
                [id],
                |row| row.get(0),
            )
            .optional()
    }

    /// R52: this bot's questions its owner dismissed since then, newest first.
    pub fn dismissed_questions(
        &self,
        agent_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<OwnerQuestion>> {
        let conn = self.conn();
        let cols = OWNER_QUESTION_COLS
            .split(", ")
            .map(|col| format!("q.{col}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn.prepare(&format!(
            "SELECT {cols} FROM owner_questions q
             JOIN owner_question_closings c ON c.question_id = q.id
             WHERE q.agent_id = ? AND c.how = 'dismissed' AND c.closed_at >= ?
             ORDER BY c.closed_at DESC, q.rowid DESC"
        ))?;
        let rows = stmt.query_map(params![agent_id, since], owner_question_from_row)?;
        rows.collect()
    }

    /// Records the answer unless one is already there, so a second answer can't replace the first.
    pub fn answer_owner_question(
        &self,
        id: &str,
        answer: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE owner_questions SET answer = ?, answered_at = ?
             WHERE id = ? AND answer IS NULL
             AND id NOT IN (SELECT question_id FROM owner_question_closings)",
            params![answer, now, id],
        )?;
        Ok(changed == 1)
    }

    // Notification targets. An owner is a person or a bot; their ids can't collide.

    // Notes a person keeps for their own bots (H1 to H5)

    /// Adds a note. False, and nothing changes, if the person already has `max` of them.
    pub fn insert_owner_note(&self, note: &NewOwnerNote, max: i64) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT INTO owner_notes
                 (id, owner_id, kind, about_person, body, from_question, created_at, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7
             WHERE (SELECT COUNT(*) FROM owner_notes WHERE owner_id = ?2) < ?8",
            params![
                note.id,
                note.owner_id,
                note.kind,
                note.about_person,
                note.body,
                note.from_question,
                note.now,
                max
            ],
        )?;
        Ok(changed > 0)
    }

    /// A person's notes, oldest first.
    pub fn owner_notes(&self, owner_id: &str) -> rusqlite::Result<Vec<OwnerNote>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {OWNER_NOTE_COLS} {OWNER_NOTES} WHERE n.owner_id = ? ORDER BY n.created_at, n.rowid"
        ))?;
        let rows = stmt.query_map([owner_id], owner_note_from_row)?;
        rows.collect()
    }

    pub fn owner_note(&self, id: &str) -> rusqlite::Result<Option<OwnerNote>> {
        self.conn()
            .query_row(
                &format!("SELECT {OWNER_NOTE_COLS} {OWNER_NOTES} WHERE n.id = ?"),
                [id],
                owner_note_from_row,
            )
            .optional()
    }

    pub fn update_owner_note(
        &self,
        id: &str,
        kind: &str,
        body: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE owner_notes SET kind = ?, body = ?, updated_at = ? WHERE id = ?",
            params![kind, body, now, id],
        )?;
        Ok(())
    }

    pub fn delete_owner_note(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM owner_notes WHERE id = ?", [id])?;
        Ok(())
    }

    /// H6: what the server knows about how `owner` and `other` know each other.
    pub fn relation(&self, owner: &str, other: &str, now: i64) -> rusqlite::Result<Relation> {
        let conn = self.conn();
        let lives_with = exists(
            &conn,
            "SELECT 1 FROM residents a JOIN residents b ON b.house_id = a.house_id
             WHERE a.person_id = ?1 AND b.person_id = ?2",
            params![owner, other],
        )?;
        let shares_a_city = exists(
            &conn,
            "SELECT 1 FROM city_members a JOIN city_members b ON b.city_id = a.city_id
             WHERE a.person_id = ?1 AND b.person_id = ?2",
            params![owner, other],
        )?;
        let plans_together = conn.query_row(
            "SELECT COUNT(*) FROM events e
             JOIN rsvps a ON a.event_id = e.id AND a.person_id = ?1 AND a.response = 'yes'
             JOIN rsvps b ON b.event_id = e.id AND b.person_id = ?2 AND b.response = 'yes'
             WHERE e.status = 'confirmed' AND e.ends_at <= ?3",
            params![owner, other, now],
            |row| row.get(0),
        )?;
        Ok(Relation { lives_with, shares_a_city, plans_together })
    }

    // Marks (H14 to H20)

    /// H15: whether a person has come across a bot: it's in a room they belong to, or it has
    /// visited or asked to visit a house they live in.
    pub fn has_come_across(&self, person_id: &str, agent_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 WHERE EXISTS (SELECT 1 FROM room_agents ra JOIN members m ON m.hangout_id = ra.hangout_id
                                    WHERE ra.agent_id = ?2 AND m.person_id = ?1)
                OR EXISTS (SELECT 1 FROM trips t JOIN residents r ON r.house_id = t.house_id
                           WHERE t.agent_id = ?2 AND r.person_id = ?1)",
            params![person_id, agent_id],
        )
    }

    pub fn set_bot_mark(
        &self,
        person_id: &str,
        agent_id: &str,
        reason: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_marks (person_id, agent_id, reason, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (person_id, agent_id) DO UPDATE SET reason = excluded.reason",
            params![person_id, agent_id, reason, now],
        )?;
        Ok(())
    }

    /// False if there was no such mark.
    pub fn remove_bot_mark(&self, person_id: &str, agent_id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "DELETE FROM bot_marks WHERE person_id = ? AND agent_id = ?",
            params![person_id, agent_id],
        )?;
        Ok(changed > 0)
    }

    pub fn bot_marks(&self, person_id: &str) -> rusqlite::Result<Vec<BotMark>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.agent_id, a.name, m.reason, m.created_at FROM bot_marks m
             JOIN agents a ON a.id = m.agent_id
             WHERE m.person_id = ? ORDER BY m.created_at, m.agent_id",
        )?;
        let rows = stmt.query_map([person_id], |row| {
            Ok(BotMark {
                agent_id: row.get(0)?,
                agent_name: row.get(1)?,
                reason: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// H19: takes back the bot's keys at every house the person lives in.
    pub fn take_keys_at_homes_of(&self, person_id: &str, agent_id: &str) -> rusqlite::Result<()> {
        let passes = "SELECT p.id FROM passes p JOIN residents r ON r.house_id = p.house_id
                      WHERE p.agent_id = ?2 AND r.person_id = ?1";
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let those = format!("p.id IN ({passes})");
        decline_scenes_on_keys(&tx, &those, params![person_id, agent_id])?;
        tx.execute(
            &format!("DELETE FROM pass_keys WHERE pass_id IN ({passes})"),
            params![person_id, agent_id],
        )?;
        tx.commit()
    }

    // Projects (L11 to L15)

    pub fn insert_project(&self, space_id: &str, agent_id: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO projects (space_id, agent_id, created_at) VALUES (?, ?, ?)",
            params![space_id, agent_id, now],
        )?;
        Ok(())
    }

    /// A project's space and the bot that started it, if it's still there.
    pub fn project(&self, space_id: &str) -> rusqlite::Result<Option<(Space, Option<String>)>> {
        self.conn()
            .query_row(
                &format!("SELECT {SPACE_COLS}, p.agent_id FROM projects p JOIN spaces s ON s.id = p.space_id WHERE p.space_id = ?"),
                [space_id],
                |row| Ok((space_from_row(row)?, row.get(6)?)),
            )
            .optional()
    }

    /// L42, L43: reserve a bounded pending proposal, under one database lock.
    pub fn propose_project_change(
        &self,
        change: &serde_json::Value,
        at: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        conn.execute("DELETE FROM project_changes WHERE created_at <= ?", [at - 7 * 86_400])?;
        Ok(conn.execute(
            "INSERT INTO project_changes (id, project_id, agent_id, reason, action, arguments, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
             WHERE (SELECT COUNT(*) FROM project_changes WHERE agent_id = ?3 AND status = 'pending' AND created_at > ?8) < 5
               AND (SELECT COUNT(*) FROM project_changes WHERE project_id = ?2 AND status = 'pending' AND created_at > ?8) < 20",
            params![change["id"].as_str(), change["project_id"].as_str(), change["agent_id"].as_str(),
                    change["reason"].as_str(), change["action"].as_str(), change["arguments"].to_string(), at, at - 86_400],
        )? == 1)
    }

    pub fn project_changes(
        &self,
        agent_id: &str,
        at: i64,
    ) -> rusqlite::Result<Vec<serde_json::Value>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT c.id, c.project_id, c.agent_id, a.name, c.reason, c.action, c.arguments,
                    c.status, c.decided_by, c.created_at
             FROM project_changes c JOIN agents a ON a.id = c.agent_id
             WHERE c.created_at > ?2 AND (c.agent_id = ?1 OR EXISTS
               (SELECT 1 FROM room_agents r WHERE r.hangout_id = c.project_id AND r.agent_id = ?1))
             ORDER BY c.created_at, c.id",
        )?;
        let rows = stmt.query_map(params![agent_id, at - 7 * 86_400], |row| {
            let status: String = row.get(7)?;
            let created: i64 = row.get(9)?;
            let arguments: String = row.get(6)?;
            Ok(serde_json::json!({
                "id": row.get::<_, String>(0)?, "project_id": row.get::<_, String>(1)?,
                "agent_id": row.get::<_, String>(2)?, "proposed_by": row.get::<_, String>(3)?,
                "reason": row.get::<_, String>(4)?, "action": row.get::<_, String>(5)?,
                "arguments": serde_json::from_str::<serde_json::Value>(&arguments).unwrap_or_default(),
                "status": if status == "pending" && created <= at - 86_400 { "expired" } else { &status },
                "decided_by": row.get::<_, Option<String>>(8)?, "created_at": created,
            }))
        })?;
        rows.collect()
    }

    pub fn decide_project_change(
        &self,
        id: &str,
        by: &str,
        status: &str,
        at: i64,
    ) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "UPDATE project_changes SET status = ?3, decided_by = ?2
             WHERE id = ?1 AND status = 'pending' AND created_at > ?4",
            params![id, by, status, at - 86_400],
        )? == 1)
    }

    pub fn finish_project_change(&self, id: &str, status: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE project_changes SET status = ?2 WHERE id = ?1 AND status = 'applying'",
            params![id, status],
        )?;
        Ok(())
    }

    pub fn house_editor(&self, player_id: i64, person_id: &str) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT EXISTS(SELECT 1 FROM house_editors WHERE player_id = ? AND person_id = ?)",
            params![player_id, person_id],
            |row| row.get(0),
        )
    }

    pub fn house_editors(&self, player_id: i64) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT p.id, p.name, p.color FROM house_editors e JOIN people p ON p.id = e.person_id WHERE e.player_id = ? ORDER BY p.name, p.id")?;
        let rows = stmt.query_map([player_id], person_from_row)?;
        rows.collect()
    }

    pub fn set_house_editor(
        &self,
        player_id: i64,
        person_id: &str,
        approved: bool,
    ) -> rusqlite::Result<()> {
        if approved {
            self.conn().execute(
                "INSERT OR IGNORE INTO house_editors (player_id, person_id) VALUES (?, ?)",
                params![player_id, person_id],
            )?;
        } else {
            self.conn().execute(
                "DELETE FROM house_editors WHERE player_id = ? AND person_id = ?",
                params![player_id, person_id],
            )?;
        }
        Ok(())
    }

    /// The projects whose first floor this bot is in, oldest first, with the bot that started each.
    pub fn projects_of_bot(
        &self,
        agent_id: &str,
    ) -> rusqlite::Result<Vec<(Space, Option<String>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SPACE_COLS}, p.agent_id FROM projects p JOIN spaces s ON s.id = p.space_id
             JOIN room_agents r ON r.hangout_id = p.space_id AND r.agent_id = ?
             ORDER BY p.created_at, p.space_id"
        ))?;
        let rows = stmt.query_map([agent_id], |row| Ok((space_from_row(row)?, row.get(6)?)))?;
        rows.collect()
    }

    /// L17: bots with public places on, each at every door of a place whose building stands as its
    /// space's (S33). Which of those it's really at, and whether it's in its hours, is the caller's
    /// to work out.
    pub fn bots_at_places(
        &self,
        world_id: &str,
        now: i64,
        limit: usize,
    ) -> rusqlite::Result<Vec<BotAtPlace>> {
        // The building on a space's plot, standing as that space's door: its owner's own player put it
        // up, and a house is a house's door and anything else a third space's.
        const DOOR: &str = "JOIN space_plots d ON d.space_id = s.id AND d.world_id = ?1
             JOIN world_every_place b ON b.world_id = d.world_id AND b.x = d.x AND b.y = d.y
             JOIN players pl ON pl.id = b.player_id AND pl.person_id = s.owner_id
                            AND pl.agent_id IS NULL AND pl.token_hash IS NULL
             JOIN bot_routines r ON r.agent_id = a.id AND r.public_places = 1";
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT a.id, a.name, a.owner_id, d.x, d.y, 1, v.hangout_id FROM floor_visits v
             JOIN agents a ON a.id = v.agent_id
             JOIN floors f ON f.hangout_id = v.hangout_id AND f.access = 'public'
             JOIN spaces s ON s.id = f.space_id
             {DOOR}
             WHERE v.until > ?2 AND (b.kind = 'house') = (s.kind = 'house')
             UNION ALL
             SELECT a.id, a.name, a.owner_id, d.x, d.y, 0, f.hangout_id FROM room_agents ra
             JOIN agents a ON a.id = ra.agent_id
             JOIN floors f ON f.hangout_id = ra.hangout_id AND f.access = 'public'
             JOIN spaces s ON s.id = f.space_id
             {DOOR}
             WHERE (b.kind = 'house') = (s.kind = 'house')
             UNION ALL
             SELECT a.id, a.name, a.owner_id, d.x, d.y, 0, pr.space_id FROM projects pr
             JOIN room_agents ra ON ra.hangout_id = pr.space_id
             JOIN floors f ON f.hangout_id = pr.space_id AND f.access = 'public'
             JOIN agents a ON a.id = ra.agent_id
             JOIN spaces s ON s.id = pr.space_id
             {DOOR}
             WHERE b.kind = 'studio'
             LIMIT ?3"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![world_id, now, limit], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })?;
        rows.collect()
    }

    // Routines (L1 to L10)

    /// S84: since when visitors to this floor see earlier chat, as `(after_id, since)`, if they do.
    /// Only while the floor is public, whatever else left a row behind.
    pub fn visitor_history(&self, hangout_id: &str) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT v.after_id, v.since FROM floor_visitor_history v
                 JOIN floors f ON f.hangout_id = v.hangout_id AND f.access = 'public'
                 WHERE v.hangout_id = ?",
                [hangout_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// S84: turns it on from the room's latest message, keeping an earlier start, or off. Returns
    /// whether this changed it.
    pub fn set_visitor_history(
        &self,
        hangout_id: &str,
        on: bool,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        Ok(if on {
            // Only onto a floor that is public as it's written, so going private can't slip between.
            conn.execute(
                "INSERT OR IGNORE INTO floor_visitor_history (hangout_id, after_id, since)
                 SELECT ?1, (SELECT COALESCE(MAX(id), 0) FROM messages), ?2
                 WHERE EXISTS (SELECT 1 FROM floors WHERE hangout_id = ?1 AND access = 'public')",
                params![hangout_id, now],
            )?
        } else {
            conn.execute("DELETE FROM floor_visitor_history WHERE hangout_id = ?", [hangout_id])?
        } > 0)
    }

    /// S84: chat a walk-in sees from before they arrived: chat lines only, after `after_id` and
    /// written from `from` up to their arrival, the latest `limit` of them, oldest first.
    pub fn chat_before_arrival(
        &self,
        hangout_id: &str,
        after_id: i64,
        from: i64,
        arrived: i64,
        after: i64,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE hangout_id = ? AND kind = 'chat' AND id > ? AND id > ?
               AND created_at >= ? AND created_at <= ?
             ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map(
            params![hangout_id, after_id, after, from, arrived, limit],
            message_from_row,
        )?;
        let mut messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// For tests: moves a room's messages `by` seconds into the past.
    pub fn age_messages(&self, hangout_id: &str, by: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE messages SET created_at = created_at - ?2 WHERE hangout_id = ?1",
            params![hangout_id, by],
        )?;
        Ok(())
    }

    /// S80: when bots may take themselves onto this floor: `anyone`, `with_owner` or `nobody`.
    pub fn floor_bot_entry(&self, hangout_id: &str) -> rusqlite::Result<String> {
        Ok(self
            .conn()
            .query_row(
                "SELECT rule FROM floor_bot_entry WHERE hangout_id = ?",
                [hangout_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "anyone".to_string()))
    }

    /// S80: sets it; `anyone` is the default, so it's no row.
    pub fn set_floor_bot_entry(
        &self,
        hangout_id: &str,
        rule: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        if rule == "anyone" {
            conn.execute("DELETE FROM floor_bot_entry WHERE hangout_id = ?", [hangout_id])?;
        } else {
            conn.execute(
                "INSERT INTO floor_bot_entry (hangout_id, rule, set_at) VALUES (?, ?, ?)
                 ON CONFLICT (hangout_id) DO UPDATE SET rule = excluded.rule, set_at = excluded.set_at",
                params![hangout_id, rule, now],
            )?;
        }
        Ok(())
    }

    /// L50: a bot's stored schedule, as `every_day` and `days`, if its owner saved one.
    pub fn bot_schedule(&self, agent_id: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT every_day, days FROM bot_schedules WHERE agent_id = ?",
                [agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// L50: saves a bot's schedule, or with `None` goes back to its routine's window.
    pub fn set_bot_schedule(
        &self,
        agent_id: &str,
        schedule: Option<(&str, &str)>,
        now: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        match schedule {
            Some((every_day, days)) => conn.execute(
                "INSERT INTO bot_schedules (agent_id, every_day, days, updated_at) VALUES (?, ?, ?, ?)
                 ON CONFLICT (agent_id) DO UPDATE SET every_day = excluded.every_day,
                     days = excluded.days, updated_at = excluded.updated_at",
                params![agent_id, every_day, days, now],
            )?,
            None => conn.execute("DELETE FROM bot_schedules WHERE agent_id = ?", [agent_id])?,
        };
        Ok(())
    }

    /// L47: the room a bot's owner gave it to work in, and its job there.
    pub fn bot_workplace(&self, agent_id: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT hangout_id, job FROM bot_workplaces WHERE agent_id = ?",
                [agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// L47: sets a bot's workplace, or with `None` clears it.
    pub fn set_bot_workplace(
        &self,
        agent_id: &str,
        workplace: Option<(&str, &str)>,
        now: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        match workplace {
            Some((hangout_id, job)) => conn.execute(
                "INSERT INTO bot_workplaces (agent_id, hangout_id, job, set_at) VALUES (?, ?, ?, ?)
                 ON CONFLICT (agent_id) DO UPDATE SET hangout_id = excluded.hangout_id,
                     job = excluded.job, set_at = excluded.set_at",
                params![agent_id, hangout_id, job, now],
            )?,
            None => conn.execute("DELETE FROM bot_workplaces WHERE agent_id = ?", [agent_id])?,
        };
        Ok(())
    }

    /// L64: how many hours a bot may stay on one public floor, or `None` for forever.
    pub fn bot_stay_limit(&self, agent_id: &str) -> rusqlite::Result<Option<i64>> {
        Ok(self
            .conn()
            .query_row("SELECT hours FROM bot_stay_limits WHERE agent_id = ?", [agent_id], |row| {
                row.get::<_, Option<i64>>(0)
            })
            .optional()?
            .flatten())
    }

    /// L64: sets a bot's stay limit, or with `None` makes it forever.
    pub fn set_bot_stay_limit(
        &self,
        agent_id: &str,
        hours: Option<i64>,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_stay_limits (agent_id, hours, set_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (agent_id) DO UPDATE SET hours = excluded.hours, set_at = excluded.set_at",
            params![agent_id, hours, now],
        )?;
        Ok(())
    }

    /// L65: when the newest line on a floor at or after `since` that this bot takes part in was
    /// said: its own, one naming it, or a reply to one of its lines. Reads the room newest first on
    /// its index and stops at the first line older than `since`, so a quiet room costs a few rows
    /// and nobody else's chatter is counted.
    pub fn last_line_with(
        &self,
        hangout_id: &str,
        agent_id: &str,
        since: i64,
    ) -> rusqlite::Result<Option<i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.created_at,
                    COALESCE(m.agent_id = ?2, 0)
                    OR EXISTS (SELECT 1 FROM mentions x WHERE x.message_id = m.id AND x.agent_id = ?2)
                    OR EXISTS (SELECT 1 FROM message_replies r JOIN messages p ON p.id = r.reply_to
                               WHERE r.message_id = m.id AND p.agent_id = ?2)
             FROM messages m WHERE m.hangout_id = ?1 AND m.kind != 'system' ORDER BY m.id DESC",
        )?;
        let mut rows = stmt.query(params![hangout_id, agent_id])?;
        while let Some(row) = rows.next()? {
            let at: i64 = row.get(0)?;
            if at < since {
                break;
            }
            if row.get::<_, bool>(1)? {
                return Ok(Some(at));
            }
        }
        Ok(None)
    }
    /// L64: the walk-in this bot had that has run out and is still on its floor's row (S62): the
    /// floor, when it started and when it ended.
    pub fn last_ended_walk_in(
        &self,
        agent_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<(String, i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT hangout_id, since, until FROM floor_visits
                 WHERE agent_id = ?1 AND until <= ?2 ORDER BY until DESC LIMIT 1",
                params![agent_id, now],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    pub fn bot_routine(&self, agent_id: &str) -> rusqlite::Result<Option<RoutineRow>> {
        self.conn()
            .query_row(
                "SELECT days, starts_min, ends_min, max_social_runs, public_places FROM bot_routines
                 WHERE agent_id = ?",
                [agent_id],
                |row| {
                    Ok(RoutineRow {
                        days: row.get(0)?,
                        starts_min: row.get(1)?,
                        ends_min: row.get(2)?,
                        max_social_runs: row.get(3)?,
                        public_places: row.get(4)?,
                    })
                },
            )
            .optional()
    }

    pub fn set_bot_routine(
        &self,
        agent_id: &str,
        routine: &RoutineRow,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_routines (agent_id, days, starts_min, ends_min, max_social_runs, public_places, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (agent_id) DO UPDATE SET
                 days = excluded.days, starts_min = excluded.starts_min, ends_min = excluded.ends_min,
                 max_social_runs = excluded.max_social_runs, public_places = excluded.public_places,
                 updated_at = excluded.updated_at",
            params![
                agent_id,
                routine.days,
                routine.starts_min,
                routine.ends_min,
                routine.max_social_runs,
                routine.public_places,
                now
            ],
        )?;
        Ok(())
    }

    /// B2: replaces a person's saved clock.
    pub fn set_person_clock(
        &self,
        person_id: &str,
        time_zone: &str,
        utc_offset_min: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO person_clocks (person_id, time_zone, utc_offset_min, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (person_id) DO UPDATE SET
                 time_zone = excluded.time_zone,
                 utc_offset_min = excluded.utc_offset_min,
                 updated_at = excluded.updated_at",
            params![person_id, time_zone, utc_offset_min, now],
        )?;
        Ok(())
    }

    pub fn person_clock(&self, person_id: &str) -> rusqlite::Result<Option<PersonClock>> {
        self.conn()
            .query_row(
                "SELECT time_zone, utc_offset_min, updated_at FROM person_clocks WHERE person_id = ?",
                [person_id],
                |row| {
                    Ok(PersonClock {
                        time_zone: row.get(0)?,
                        utc_offset_minutes: row.get(1)?,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional()
    }

    pub fn notify_url(&self, owner_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row("SELECT url FROM notify_targets WHERE owner_id = ?", [owner_id], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn set_notify_url(&self, owner_id: &str, url: Option<&str>) -> rusqlite::Result<()> {
        let conn = self.conn();
        match url {
            Some(url) => conn.execute(
                "INSERT INTO notify_targets (owner_id, url) VALUES (?1, ?2)
                 ON CONFLICT (owner_id) DO UPDATE SET url = excluded.url",
                params![owner_id, url],
            )?,
            None => conn.execute("DELETE FROM notify_targets WHERE owner_id = ?", [owner_id])?,
        };
        Ok(())
    }

    // Events

    pub fn insert_event(&self, event: &NewEvent) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO events (id, hangout_id, title, location, notes, starts_at, ends_at, status,
                                 min_yes, proposed_by, proposer, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'proposed', ?8, ?9, ?10, ?11, ?11)",
            params![
                event.id,
                event.hangout_id,
                event.title,
                event.location,
                event.notes,
                event.starts_at,
                event.ends_at,
                event.min_yes,
                event.proposed_by,
                event.proposer,
                event.now
            ],
        )?;
        Ok(())
    }

    pub fn event(&self, id: &str) -> rusqlite::Result<Option<Event>> {
        let conn = self.conn();
        let event = conn
            .query_row(
                &format!("SELECT {EVENT_COLS} FROM events e WHERE e.id = ?"),
                [id],
                event_from_row,
            )
            .optional()?;
        event
            .map(|mut event| {
                event.rsvps = load_rsvps(&conn, &event.id)?;
                Ok(event)
            })
            .transpose()
    }

    /// Events still ahead (or under way), soonest first. Cancelled ones are left out.
    pub fn upcoming_events(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<Event>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {EVENT_COLS} FROM events e
             WHERE e.hangout_id = ? AND e.ends_at >= ? AND e.status != 'cancelled'
             ORDER BY e.starts_at"
        ))?;
        let rows = stmt.query_map(params![hangout_id, now], event_from_row)?;
        let mut events = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        for event in &mut events {
            event.rsvps = load_rsvps(&conn, &event.id)?;
        }
        Ok(events)
    }

    pub fn rsvp(&self, event_id: &str, person_id: &str) -> rusqlite::Result<Option<Rsvp>> {
        self.conn()
            .query_row(
                "SELECT person_id, response, via_agent, note FROM rsvps WHERE event_id = ? AND person_id = ?",
                params![event_id, person_id],
                rsvp_from_row,
            )
            .optional()
    }

    pub fn upsert_rsvp(
        &self,
        event_id: &str,
        person_id: &str,
        response: &str,
        via_agent: Option<&str>,
        note: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO rsvps (event_id, person_id, response, via_agent, note, updated_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (event_id, person_id) DO UPDATE SET
                response = excluded.response, via_agent = excluded.via_agent,
                note = excluded.note, updated_at = excluded.updated_at",
            params![event_id, person_id, response, via_agent, note, now],
        )?;
        // F40: saying no lets go of what they claimed, so saying yes later doesn't bring it back.
        if response == "no" {
            tx.execute(
                "UPDATE plan_items SET claimed_by = NULL, claimed_via = NULL, done = 0
                 WHERE event_id = ? AND claimed_by = ?",
                params![event_id, person_id],
            )?;
        }
        tx.commit()
    }

    /// Sets an event's status and bumps its iCal sequence so calendars pick up the change.
    pub fn set_event_status(&self, event_id: &str, status: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE events SET status = ?, sequence = sequence + 1, updated_at = ? WHERE id = ?",
            params![status, now, event_id],
        )?;
        Ok(())
    }

    /// What goes on one person's calendar: only events they (or their bot) said yes or maybe to.
    /// Nobody else's bot can put something on your calendar by proposing it. An invitee gets the
    /// plan they answered, with no notes and no room name (F20).
    pub fn feed_events(&self, person_id: &str, since: i64) -> rusqlite::Result<Vec<FeedEvent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {EVENT_COLS}, h.name, r.response,
                    EXISTS (SELECT 1 FROM members m
                            WHERE m.hangout_id = e.hangout_id AND m.person_id = ?1) AS in_room
             FROM events e
             JOIN hangouts h ON h.id = e.hangout_id
             JOIN rsvps r ON r.event_id = e.id AND r.person_id = ?1
             WHERE r.response IN ('yes', 'maybe') AND e.ends_at >= ?2
               AND (in_room OR EXISTS (SELECT 1 FROM plan_invitees i
                                       WHERE i.event_id = e.id AND i.person_id = ?1))
             ORDER BY e.starts_at"
        ))?;
        let rows = stmt.query_map(params![person_id, since], |row| {
            let mut event = event_from_row(row)?;
            let in_room: bool = row.get(16)?;
            if !in_room {
                event.notes.clear();
            }
            Ok(FeedEvent {
                event,
                hangout_name: if in_room { row.get(14)? } else { "a friend".to_string() },
                response: row.get(15)?,
            })
        })?;
        rows.collect()
    }

    // Cities

    /// Starts a city with its Town Square, and makes the creator a member of both.
    pub fn create_city(&self, city: &NewCity) -> rusqlite::Result<City> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let square_name = format!("{} Town Square", city.name);
        insert_room(
            &tx,
            city.square_id,
            &square_name,
            city.square_code,
            city.created_by,
            city.now,
        )?;
        tx.execute(
            "INSERT INTO cities (id, name, invite_code, square_id, created_by, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                city.id,
                city.name,
                city.invite_code,
                city.square_id,
                city.created_by,
                city.now
            ],
        )?;
        join_city(&tx, city.id, city.created_by, city.now)?;
        tx.commit()?;
        Ok(City {
            id: city.id.to_string(),
            name: city.name.to_string(),
            invite_code: city.invite_code.to_string(),
            square_id: city.square_id.to_string(),
            created_by: city.created_by.to_string(),
            created_at: city.now,
        })
    }

    pub fn city(&self, id: &str) -> rusqlite::Result<Option<City>> {
        self.conn()
            .query_row(
                &format!("SELECT {CITY_COLS} FROM cities c WHERE c.id = ?"),
                [id],
                city_from_row,
            )
            .optional()
    }

    pub fn city_by_invite(&self, code: &str) -> rusqlite::Result<Option<City>> {
        self.conn()
            .query_row(
                &format!("SELECT {CITY_COLS} FROM cities c WHERE c.invite_code = ?"),
                [code],
                city_from_row,
            )
            .optional()
    }

    /// The city a Town Square belongs to, if this hangout is one.
    pub fn city_for_square(&self, hangout_id: &str) -> rusqlite::Result<Option<City>> {
        self.conn()
            .query_row(
                &format!("SELECT {CITY_COLS} FROM cities c WHERE c.square_id = ?"),
                [hangout_id],
                city_from_row,
            )
            .optional()
    }

    pub fn set_city_invite(&self, city_id: &str, code: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("UPDATE cities SET invite_code = ? WHERE id = ?", params![code, city_id])?;
        end_friend_invites(&tx, "city", city_id)?;
        tx.commit()
    }

    pub fn cities_for_person(&self, person_id: &str) -> rusqlite::Result<Vec<City>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CITY_COLS} FROM cities c
             JOIN city_members m ON m.city_id = c.id
             WHERE m.person_id = ? ORDER BY m.joined_at, c.id"
        ))?;
        let rows = stmt.query_map([person_id], city_from_row)?;
        rows.collect()
    }

    pub fn city_members(&self, city_id: &str) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.color FROM people p
             JOIN city_members m ON m.person_id = p.id
             WHERE m.city_id = ? ORDER BY m.joined_at, p.id",
        )?;
        let rows = stmt.query_map([city_id], person_from_row)?;
        rows.collect()
    }

    pub fn is_city_member(&self, city_id: &str, person_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM city_members WHERE city_id = ? AND person_id = ?",
            params![city_id, person_id],
        )
    }

    /// Joining a city also joins its Town Square.
    pub fn add_city_member(
        &self,
        city_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        join_city(&tx, city_id, person_id, now)?;
        tx.commit()
    }

    /// Leaving takes the person and their bots out of the Town Square, ends their bots' trips to
    /// this city's houses and cancels the passes for those bots.
    pub fn remove_city_member(
        &self,
        city_id: &str,
        person_id: &str,
        ended_by: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let theirs = "agent_id IN (SELECT id FROM agents WHERE owner_id = ?2)
                      AND house_id IN (SELECT id FROM houses WHERE city_id = ?1)";
        tx.execute(
            &format!(
                "UPDATE trips SET status = 'ended', ended_by = ?4
                 WHERE status = 'here' AND ends_at > ?3 AND {theirs}"
            ),
            params![city_id, person_id, now, ended_by],
        )?;
        tx.execute(
            &format!(
                "UPDATE trips SET status = 'declined', ended_by = ?4
                 WHERE status = 'asking' AND expires_at > ?3 AND {theirs}"
            ),
            params![city_id, person_id, now, ended_by],
        )?;
        decline_scenes_on_keys(
            &tx,
            "p.agent_id IN (SELECT id FROM agents WHERE owner_id = ?2)
             AND p.house_id IN (SELECT id FROM houses WHERE city_id = ?1)",
            params![city_id, person_id],
        )?;
        tx.execute(&format!("DELETE FROM passes WHERE {theirs}"), params![city_id, person_id])?;
        tx.execute(
            "DELETE FROM room_agents WHERE hangout_id = (SELECT square_id FROM cities WHERE id = ?1)
               AND agent_id IN (SELECT id FROM agents WHERE owner_id = ?2)",
            params![city_id, person_id],
        )?;
        tx.execute(
            "DELETE FROM members WHERE hangout_id = (SELECT square_id FROM cities WHERE id = ?1)
               AND person_id = ?2",
            params![city_id, person_id],
        )?;
        tx.execute(
            "DELETE FROM city_members WHERE city_id = ? AND person_id = ?",
            params![city_id, person_id],
        )?;
        tx.commit()
    }

    /// Every bot whose owner belongs to the city, with where it lives and where it is now. Homes
    /// and trips in other cities don't show. House bots don't either: the moment one wakes up
    /// would tell the whole city that someone's home.
    pub fn bots_in_city(&self, city_id: &str, now: i64) -> rusqlite::Result<Vec<BotPlace>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&bot_places_sql(
            "?2",
            "JOIN city_members cm ON cm.person_id = a.owner_id AND cm.city_id = ?2
             LEFT JOIN bot_homes bh ON bh.agent_id = a.id
                AND bh.house_id IN (SELECT id FROM houses WHERE city_id = ?2)
                AND EXISTS (SELECT 1 FROM residents r
                            WHERE r.house_id = bh.house_id AND r.person_id = a.owner_id)
             WHERE NOT EXISTS (SELECT 1 FROM bot_homes hb WHERE hb.agent_id = a.id AND hb.is_house = 1)",
        ))?;
        let rows = stmt.query_map(params![now, city_id], |row| bot_place_from_row(row, now))?;
        rows.collect()
    }

    // Houses

    /// Builds a house: its Living Room and Porch, the house itself, and the creator as a resident
    /// and a member of both rooms.
    /// The new house, or `None` with nothing made if its builder already lives in this city. That's
    /// decided here so two requests at the same moment can't both get past it (R5).
    pub fn create_house(&self, house: &NewHouse) -> rusqlite::Result<Option<House>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if lives_elsewhere_in_city(&tx, house.city_id, house.created_by, house.id)? {
            return Ok(None);
        }
        let living_room_name = format!("{} Living Room", house.name);
        let porch_name = format!("{} Porch", house.name);
        insert_room(
            &tx,
            house.living_room_id,
            &living_room_name,
            house.living_room_code,
            house.created_by,
            house.now,
        )?;
        insert_room(
            &tx,
            house.porch_id,
            &porch_name,
            house.porch_code,
            house.created_by,
            house.now,
        )?;
        tx.execute(
            "INSERT INTO houses (id, city_id, name, invite_code, living_room, porch, created_by, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                house.id,
                house.city_id,
                house.name,
                house.invite_code,
                house.living_room_id,
                house.porch_id,
                house.created_by,
                house.now
            ],
        )?;
        tx.execute(
            "INSERT INTO residents (house_id, person_id, joined_at) VALUES (?, ?, ?)",
            params![house.id, house.created_by, house.now],
        )?;
        tx.commit()?;
        Ok(Some(House {
            id: house.id.to_string(),
            city_id: house.city_id.to_string(),
            name: house.name.to_string(),
            invite_code: house.invite_code.to_string(),
            living_room_id: house.living_room_id.to_string(),
            porch_id: house.porch_id.to_string(),
            created_by: house.created_by.to_string(),
            created_at: house.now,
        }))
    }

    pub fn house(&self, id: &str) -> rusqlite::Result<Option<House>> {
        self.conn()
            .query_row(
                &format!("SELECT {HOUSE_COLS} FROM houses h WHERE h.id = ?"),
                [id],
                house_from_row,
            )
            .optional()
    }

    pub fn house_by_invite(&self, code: &str) -> rusqlite::Result<Option<House>> {
        self.conn()
            .query_row(
                &format!("SELECT {HOUSE_COLS} FROM houses h WHERE h.invite_code = ?"),
                [code],
                house_from_row,
            )
            .optional()
    }

    pub fn set_house_invite(&self, house_id: &str, code: &str) -> rusqlite::Result<()> {
        self.conn()
            .execute("UPDATE houses SET invite_code = ? WHERE id = ?", params![code, house_id])?;
        Ok(())
    }

    /// The house a Living Room or Porch belongs to. A Town Square has none.
    pub fn house_for_room(&self, hangout_id: &str) -> rusqlite::Result<Option<House>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {HOUSE_COLS} FROM houses h WHERE h.living_room = ?1 OR h.porch = ?1"
                ),
                [hangout_id],
                house_from_row,
            )
            .optional()
    }

    /// Town Squares, Living Rooms and Porches, which people get into by being in the city or the
    /// house and never through a hangout invite.
    pub fn is_managed_room(&self, hangout_id: &str) -> rusqlite::Result<bool> {
        is_managed_room(&self.conn(), hangout_id)
    }

    // Spaces and floors

    /// This person's house, built with its one floor if they have none (S2).
    pub fn ensure_house(
        &self,
        id: &str,
        name: &str,
        invite_code: &str,
        owner_id: &str,
        now: i64,
    ) -> rusqlite::Result<Space> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let house = |tx: &Connection| {
            tx.query_row(
                &format!(
                    "SELECT {SPACE_COLS} FROM spaces s WHERE s.owner_id = ? AND s.kind = 'house'"
                ),
                [owner_id],
                space_from_row,
            )
            .optional()
        };
        if let Some(found) = house(&tx)? {
            return Ok(found);
        }
        insert_space(&tx, id, "house", name, invite_code, owner_id, now)?;
        let made = house(&tx)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(made)
    }

    pub fn space(&self, id: &str) -> rusqlite::Result<Option<Space>> {
        self.conn()
            .query_row(
                &format!("SELECT {SPACE_COLS} FROM spaces s WHERE s.id = ?"),
                [id],
                space_from_row,
            )
            .optional()
    }

    /// The space a room is a floor of, if it is one.
    pub fn space_for_room(&self, hangout_id: &str) -> rusqlite::Result<Option<Space>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {SPACE_COLS} FROM spaces s JOIN floors f ON f.space_id = s.id
                     WHERE f.hangout_id = ?"
                ),
                [hangout_id],
                space_from_row,
            )
            .optional()
    }

    /// The space a tile was last the door of. This proves nothing alone: see `spaces::is_door_to`.
    pub fn space_on_plot(&self, world_id: &str, x: i64, y: i64) -> rusqlite::Result<Option<Space>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {SPACE_COLS} FROM spaces s JOIN space_plots d ON d.space_id = s.id
                     WHERE d.world_id = ? AND d.x = ? AND d.y = ?"
                ),
                params![world_id, x, y],
                space_from_row,
            )
            .optional()
    }

    /// Where a space's door was last put.
    pub fn plot_of(&self, space_id: &str, world_id: &str) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT x, y FROM space_plots WHERE space_id = ? AND world_id = ?",
                params![space_id, world_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// Makes a tile the door of a space. A space has one door and a tile opens one space, so this
    /// replaces whatever either of them had before.
    pub fn set_plot(&self, world_id: &str, x: i64, y: i64, space_id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO space_plots (world_id, x, y, space_id) VALUES (?, ?, ?, ?)",
            params![world_id, x, y, space_id],
        )?;
        Ok(())
    }

    /// L61: count only held third spaces, using the same ownership rules as project creation.
    pub fn third_space_count(&self, owner_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            &format!(
                "SELECT COUNT(*) FROM {HELD_SPACES} WHERE s.owner_id = ? AND s.kind = 'third'"
            ),
            [owner_id],
            |row| row.get(0),
        )
    }

    /// The third spaces this person holds, oldest first. Times are whole seconds and ids are random,
    /// so two made in one second are told apart by which was stored first.
    pub fn third_spaces_of(&self, owner_id: &str) -> rusqlite::Result<Vec<Space>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SPACE_COLS} FROM {HELD_SPACES}
             WHERE s.owner_id = ? AND s.kind = 'third' ORDER BY s.created_at, s.rowid"
        ))?;
        let rows = stmt.query_map([owner_id], space_from_row)?;
        rows.collect()
    }

    /// The floor this room is, or `None` for a Town Square, Living Room or Porch.
    pub fn floor(&self, hangout_id: &str) -> rusqlite::Result<Option<Floor>> {
        self.conn()
            .query_row(
                &format!("SELECT {FLOOR_COLS} FROM {FLOORS} WHERE f.hangout_id = ?"),
                [hangout_id],
                floor_from_row,
            )
            .optional()
    }

    /// A space's floors, lowest first.
    pub fn floors_of(&self, space_id: &str) -> rusqlite::Result<Vec<Floor>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {FLOOR_COLS} FROM {FLOORS} WHERE f.space_id = ? ORDER BY f.level"
        ))?;
        let rows = stmt.query_map([space_id], floor_from_row)?;
        rows.collect()
    }

    /// A first floor still called what the space is called follows the space's new name, so a room
    /// with one floor doesn't end up with two names.
    pub fn change_space(
        &self,
        space: &Space,
        name: Option<&str>,
        about: Option<&str>,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE spaces SET name = COALESCE(?2, name), about = COALESCE(?3, about) WHERE id = ?1",
            params![space.id, name, about],
        )?;
        if let Some(name) = name {
            tx.execute(
                "UPDATE hangouts SET name = ?2 WHERE id = ?1 AND name = ?3",
                params![space.id, name, space.name],
            )?;
        }
        tx.commit()
    }

    /// Adds a floor above the others, with only its maker in it. `None` if the space is full (S4).
    pub fn add_floor(&self, floor: &NewFloor, max: i64) -> rusqlite::Result<Option<Floor>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (count, top): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(MAX(level), -1) FROM floors WHERE space_id = ?",
            [floor.space_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if count >= max {
            return Ok(None);
        }
        insert_room(&tx, floor.id, floor.name, floor.invite_code, floor.created_by, floor.now)?;
        tx.execute(
            "INSERT INTO floors (hangout_id, space_id, level) VALUES (?, ?, ?)",
            params![floor.id, floor.space_id, top + 1],
        )?;
        tx.commit()?;
        // The connection's lock isn't reentrant, and `floor` takes it.
        drop(conn);
        self.floor(floor.id)
    }

    /// Changes a floor and returns the names of any bots this took off it. In one step, so no bot
    /// is on a public floor for a moment on its way out (S25), and so walk-ins end with the floor
    /// going private (S26) and don't come back if it opens again. `None`, and nothing changes, if
    /// this would open a floor holding a kind that stays private (I11): that's checked here and not
    /// by the caller, so an item can't be placed between the check and the floor opening.
    pub fn change_floor(&self, change: &FloorChange) -> rusqlite::Result<Option<Vec<String>>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let access: String =
            tx.query_row("SELECT access FROM floors WHERE hangout_id = ?", [change.id], |row| {
                row.get(0)
            })?;
        let mut taken_off = Vec::new();
        if change.access == Some("public") && access != "public" {
            for kind in change.private_only {
                if exists(
                    &tx,
                    "SELECT 1 FROM floor_items WHERE hangout_id = ? AND kind = ?",
                    params![change.id, kind],
                )? {
                    return Ok(None);
                }
            }
            let others = "SELECT a.id FROM agents a JOIN room_agents r ON r.agent_id = a.id
                          WHERE r.hangout_id = ?1 AND a.owner_id != ?2";
            {
                let mut stmt = tx.prepare(&format!(
                    "SELECT name FROM agents WHERE id IN ({others}) ORDER BY created_at, id"
                ))?;
                let names =
                    stmt.query_map(params![change.id, change.keep_bots_of], |row| row.get(0))?;
                taken_off = names.collect::<rusqlite::Result<Vec<String>>>()?;
            }
            // S25, S62: the bots this takes off can be told why the room then says they've gone.
            tx.execute(
                "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
                 SELECT a.id, ?1, ?3, 'you were sent home when this floor opened to the public'
                 FROM agents a JOIN room_agents r ON r.agent_id = a.id
                 WHERE r.hangout_id = ?1 AND a.owner_id != ?2
                 ON CONFLICT (who, hangout_id) DO UPDATE SET
                    left_at = excluded.left_at, reason = excluded.reason",
                params![change.id, change.keep_bots_of, crate::now()],
            )?;
            tx.execute(
                &format!(
                    "DELETE FROM room_agents WHERE hangout_id = ?1 AND agent_id IN ({others})"
                ),
                params![change.id, change.keep_bots_of],
            )?;
        }
        if change.access == Some("private") {
            tx.execute(
                "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
                 SELECT who, hangout_id, ?2, 'this floor went private'
                 FROM floor_visits WHERE hangout_id = ?1
                 ON CONFLICT (who, hangout_id) DO UPDATE SET
                    left_at = excluded.left_at, reason = excluded.reason",
                params![change.id, crate::now()],
            )?;
            tx.execute("DELETE FROM floor_visits WHERE hangout_id = ?", [change.id])?;
            // S84: and visitors' history, so opening it again starts that afresh too.
            tx.execute("DELETE FROM floor_visitor_history WHERE hangout_id = ?", [change.id])?;
        }
        tx.execute(
            "UPDATE floors SET access = COALESCE(?2, access), size = COALESCE(?3, size),
                paint = COALESCE(?4, paint), flooring = COALESCE(?5, flooring)
             WHERE hangout_id = ?1",
            params![change.id, change.access, change.size, change.paint, change.flooring],
        )?;
        if let Some(name) = change.name {
            tx.execute("UPDATE hangouts SET name = ?2 WHERE id = ?1", params![change.id, name])?;
        }
        tx.commit()?;
        Ok(Some(taken_off))
    }

    /// Sets what a floor leaves out to the comma-separated words `choose` gives, from what it leaves
    /// out now and what's on it. In one step, so nothing can be put down where the couch is about to
    /// come back to (I3).
    pub fn choose_hidden<E>(
        &self,
        hangout_id: &str,
        choose: impl FnOnce(&[String], &[FloorItem]) -> Result<String, E>,
    ) -> rusqlite::Result<Result<(), E>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let hidden: String =
            tx.query_row("SELECT hidden FROM floors WHERE hangout_id = ?", [hangout_id], |row| {
                row.get(0)
            })?;
        let chosen = match choose(&hidden_words(&hidden), &items_on(&tx, hangout_id)?) {
            Ok(chosen) => chosen,
            Err(refused) => return Ok(Err(refused)),
        };
        tx.execute(
            "UPDATE floors SET hidden = ? WHERE hangout_id = ?",
            params![chosen, hangout_id],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    }

    /// Swaps a floor with the one above or below it. At the end of the stack this does nothing.
    pub fn move_floor(&self, floor: &Floor, up: bool) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let neighbour = if up {
            "SELECT hangout_id, level FROM floors WHERE space_id = ? AND level > ? ORDER BY level LIMIT 1"
        } else {
            "SELECT hangout_id, level FROM floors WHERE space_id = ? AND level < ? ORDER BY level DESC LIMIT 1"
        };
        let found: Option<(String, i64)> = tx
            .query_row(neighbour, params![floor.space_id, floor.level], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        if let Some((other, level)) = found {
            // Through a level nothing uses, because two floors can't share one even for a moment.
            let place = "UPDATE floors SET level = ? WHERE hangout_id = ?";
            tx.execute(place, params![-1, floor.id])?;
            tx.execute(place, params![floor.level, other])?;
            tx.execute(place, params![level, floor.id])?;
        }
        tx.commit()
    }

    /// Deletes rooms and everything in them: chat, notes, plans, check-ins and who was there. A
    /// space goes with the room it began in.
    pub fn remove_rooms(&self, hangout_ids: &[&str]) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for id in hangout_ids {
            // K3: a bond keeps what a project was called and whether it was built.
            tx.execute(
                &format!(
                    "UPDATE bond_projects AS b SET
                        name = (SELECT name FROM spaces WHERE id = ?1),
                        purpose = (SELECT about FROM spaces WHERE id = ?1),
                        built = {BOND_BUILT_LIVE}
                     WHERE b.project_id = ?1 AND EXISTS (SELECT 1 FROM spaces WHERE id = ?1)"
                ),
                [id],
            )?;
            tx.execute("DELETE FROM hangouts WHERE id = ?", [id])?;
        }
        tx.commit()
    }

    /// How many are on a floor: members, their bots, and walk-ins and theirs (S13).
    pub fn on_floor(&self, hangout_id: &str, now: i64) -> rusqlite::Result<i64> {
        count_on_floor(&self.conn(), hangout_id, now)
    }

    /// The floors someone walked onto and is still on. `who` is a person's id or their bot's.
    pub fn walked_onto(&self, who: &str, now: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT hangout_id FROM floor_visits WHERE who = ? AND until > ? ORDER BY since",
        )?;
        let rows = stmt.query_map(params![who, now], |row| row.get(0))?;
        rows.collect()
    }

    /// Walks someone onto a public floor if they fit, off any other floor they walked onto (S38).
    /// Someone already there stays where they were in the floor's history and gets more time (S22).
    pub fn walk_in(
        &self,
        visit: &NewVisit,
        capacity: i64,
        max_bots: i64,
    ) -> rusqlite::Result<WalkedIn> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let checked = walk_in_check(&tx, visit, capacity, max_bots)?;
        if checked != WalkedIn::In {
            return Ok(checked);
        }
        tx.execute(
            "DELETE FROM floor_visits WHERE who = ? AND hangout_id != ?",
            params![visit.who, visit.hangout_id],
        )?;
        tx.execute(
            "INSERT INTO floor_visits (hangout_id, who, person_id, agent_id, since, until)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (hangout_id, who) DO UPDATE SET
                until = excluded.until,
                since = CASE WHEN floor_visits.until > ?5 THEN floor_visits.since ELSE excluded.since END",
            params![visit.hangout_id, visit.who, visit.person_id, visit.agent_id, visit.now, visit.until],
        )?;
        tx.commit()?;
        Ok(WalkedIn::In)
    }

    /// S73: what walking in would come to now, changing nothing, so a move that would be refused
    /// says no goodbye first.
    pub fn would_walk_in(
        &self,
        visit: &NewVisit,
        capacity: i64,
        max_bots: i64,
    ) -> rusqlite::Result<WalkedIn> {
        walk_in_check(&self.conn(), visit, capacity, max_bots)
    }

    /// S56: the one room a bot is in now, whatever else it belongs to. A trip comes first, because
    /// only going home ends one (S39), and then a walk-in. Then the room it last went to, until
    /// S61 sends it home. A bot with no home and nowhere to be has no place.
    pub fn bot_place(&self, agent_id: &str, now: i64) -> rusqlite::Result<Option<String>> {
        let conn = self.conn();
        let went: Option<(String, i64)> = conn
            .query_row(
                "SELECT hangout_id, since FROM bot_places WHERE agent_id = ?",
                [agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let porch: Option<String> = conn
            .query_row(
                "SELECT ho.porch FROM trips t JOIN houses ho ON ho.id = t.house_id
                 WHERE t.agent_id = ? AND t.status = 'here' AND t.ends_at > ?",
                params![agent_id, now],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(porch) = porch {
            // H10: it's in the Living Room only if it went there, and it still has the key.
            return Ok(match went {
                Some((room, _)) if guest_on_trip(&conn, &room, agent_id, now)? => Some(room),
                _ => Some(porch),
            });
        }
        let walked: Option<String> = conn
            .query_row(
                "SELECT v.hangout_id FROM floor_visits v JOIN floors f ON f.hangout_id = v.hangout_id
                 WHERE v.agent_id = ? AND v.until > ? AND f.access = 'public'
                 ORDER BY v.since DESC LIMIT 1",
                params![agent_id, now],
                |row| row.get(0),
            )
            .optional()?;
        if walked.is_some() {
            return Ok(walked);
        }
        // S61: the room it's doing something in, or else home: the house it lives in, its owner's
        // house, or the first room it was brought into that its owner runs.
        let staying = went.filter(|(_, since)| now - since < BOT_STAY_SECS).map(|(room, _)| room);
        staying_or_home(&conn, agent_id, staying.as_deref())
    }

    /// S61: where a bot goes home to, if it has a home: the room S56 ends at with nothing else.
    pub fn bot_home_room(&self, agent_id: &str) -> rusqlite::Result<Option<String>> {
        staying_or_home(&self.conn(), agent_id, None)
    }

    /// S71: the bots the minute look looks at: those it has seen move, and only while something
    /// could still move them without their doing anything, so a dormant bot costs nothing.
    /// `since` is a day ago: nothing that moves a bot by time (S61's 30 minutes, a walk-in, social
    /// hours, a trip) is still due a day after the bot or its row last changed.
    pub fn bots_to_look_at(&self, since: i64) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name FROM bot_seen_places s JOIN agents a ON a.id = s.agent_id
             WHERE NOT EXISTS (SELECT 1 FROM bot_homes b WHERE b.agent_id = a.id AND b.is_house = 1)
               AND (s.seen_at > ?1
                    OR COALESCE(a.last_active_at, 0) > ?1
                    OR EXISTS (SELECT 1 FROM bot_places p WHERE p.agent_id = a.id AND p.since > ?1)
                    OR EXISTS (SELECT 1 FROM floor_visits v WHERE v.agent_id = a.id AND v.until > ?1)
                    OR EXISTS (SELECT 1 FROM trips t WHERE t.agent_id = a.id AND t.ends_at > ?1))
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map([since], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// S71: where the minute look last saw a bot, where a move it already knows about took it,
    /// and a room whose leaving was already said.
    pub fn seen_place(&self, agent_id: &str) -> rusqlite::Result<Option<SeenPlace>> {
        self.conn()
            .query_row(
                "SELECT hangout_id, told_to, said_from, seen_at FROM bot_seen_places
                 WHERE agent_id = ?",
                [agent_id],
                |row| {
                    Ok(SeenPlace {
                        hangout_id: row.get(0)?,
                        told_to: row.get(1)?,
                        said_from: row.get(2)?,
                        seen_at: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    /// S71: the minute look moves a bot's row on from what it read to `now_in`, once. It changes
    /// nothing if anything touched the row since the look read it, such as the bot moving and
    /// being told (`seen_at` goes up with every change), so two looks, or a look and a move, can't
    /// both say it.
    pub fn move_seen_place(
        &self,
        agent_id: &str,
        seen: &SeenPlace,
        now_in: Option<&str>,
        at: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let same = "agent_id = ?1 AND hangout_id = ?2 AND told_to IS ?3 AND said_from IS ?4
                    AND seen_at = ?5";
        let changed = match now_in {
            Some(now_in) => conn.execute(
                &format!(
                    "UPDATE bot_seen_places SET hangout_id = ?6, told_to = NULL, said_from = NULL,
                        seen_at = MAX(seen_at + 1, ?7)
                     WHERE {same}"
                ),
                params![
                    agent_id,
                    seen.hangout_id,
                    seen.told_to,
                    seen.said_from,
                    seen.seen_at,
                    now_in,
                    at
                ],
            )?,
            None => conn.execute(
                &format!("DELETE FROM bot_seen_places WHERE {same}"),
                params![agent_id, seen.hangout_id, seen.told_to, seen.said_from, seen.seen_at],
            )?,
        };
        Ok(changed > 0)
    }

    /// S71: back where it was seen before the look came round, so what a move or a goodbye noted
    /// no longer applies. Only if nothing touched the row since it was read.
    pub fn clear_seen_notes(
        &self,
        agent_id: &str,
        seen: &SeenPlace,
        at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE bot_seen_places SET told_to = NULL, said_from = NULL,
                seen_at = MAX(seen_at + 1, ?6)
             WHERE agent_id = ?1 AND hangout_id = ?2 AND told_to IS ?3 AND said_from IS ?4
               AND seen_at = ?5",
            params![agent_id, seen.hangout_id, seen.told_to, seen.said_from, seen.seen_at, at],
        )?;
        Ok(())
    }

    /// S71: after the look hasn't run for a long while, such as an older release running in
    /// between, each row is brought up to where the bot is now, saying nothing.
    pub fn reset_seen_place(
        &self,
        agent_id: &str,
        now_in: Option<&str>,
        at: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        match now_in {
            Some(room) => conn.execute(
                "UPDATE bot_seen_places SET hangout_id = ?2, told_to = NULL, said_from = NULL,
                    seen_at = MAX(seen_at + 1, ?3)
                 WHERE agent_id = ?1",
                params![agent_id, room, at],
            )?,
            None => conn.execute("DELETE FROM bot_seen_places WHERE agent_id = ?", [agent_id])?,
        };
        Ok(())
    }

    /// S71: every bot the look has a row for.
    pub fn seen_bots(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT agent_id FROM bot_seen_places ORDER BY agent_id")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect()
    }

    /// S71: when the minute look last ran on the server, as the running server records it.
    pub fn last_look(&self) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row("SELECT at FROM look_marks WHERE name = 'leaving'", [], |row| row.get(0))
            .optional()
    }

    pub fn note_look(&self, at: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO look_marks (name, at) VALUES ('leaving', ?1)
             ON CONFLICT (name) DO UPDATE SET at = excluded.at",
            [at],
        )?;
        Ok(())
    }

    /// E11, S71: the bot already knows where this move took it (`None` is no room), so the minute
    /// look sends no event. A bot moving for the first time since this came in is seen from where
    /// it went.
    pub fn note_told_place(
        &self,
        agent_id: &str,
        now_in: Option<&str>,
        at: i64,
    ) -> rusqlite::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE bot_seen_places SET told_to = ?2, seen_at = MAX(seen_at + 1, ?3)
             WHERE agent_id = ?1",
            params![agent_id, now_in.unwrap_or(""), at],
        )?;
        if let Some(room) = now_in {
            conn.execute(
                "INSERT OR IGNORE INTO bot_seen_places (agent_id, hangout_id, told_to, seen_at)
                 SELECT ?1, id, id, ?3 FROM hangouts WHERE id = ?2",
                params![agent_id, room, at],
            )?;
        }
        Ok(())
    }

    /// S73: the bot said goodbye to this room itself, or the room was told, so the minute look
    /// says nothing more there.
    pub fn note_said_leaving(
        &self,
        agent_id: &str,
        hangout_id: &str,
        at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE bot_seen_places SET said_from = ?2, seen_at = MAX(seen_at + 1, ?3)
             WHERE agent_id = ?1 AND hangout_id = ?2",
            params![agent_id, hangout_id, at],
        )?;
        Ok(())
    }

    /// Whether a bot belongs to a room: brought in, not a guest and not a walk-in.
    pub fn belongs_to_room(&self, hangout_id: &str, agent_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM room_agents WHERE hangout_id = ? AND agent_id = ?",
            params![hangout_id, agent_id],
        )
    }

    /// L45: a bot's walk-in on a floor, still going, and when it ends.
    pub fn visit_until(
        &self,
        hangout_id: &str,
        agent_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT until FROM floor_visits
                 WHERE hangout_id = ?1 AND agent_id = ?2 AND until > ?3",
                params![hangout_id, agent_id, now],
                |row| row.get(0),
            )
            .optional()
    }

    /// L45: a walk-in that's still going lasts until `until`, if that's later. It keeps its start
    /// (S19), so the bot sees nothing it hadn't.
    pub fn extend_visit(
        &self,
        hangout_id: &str,
        agent_id: &str,
        until: i64,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE floor_visits SET until = ?3
             WHERE hangout_id = ?1 AND agent_id = ?2 AND until > ?4 AND until < ?3",
            params![hangout_id, agent_id, until, now],
        )?;
        Ok(changed > 0)
    }

    /// S74: this bot follows `leader_id` until `until`, in place of anyone it followed before.
    pub fn follow(
        &self,
        follower_id: &str,
        leader_id: &str,
        now: i64,
        until: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_follows (follower_id, leader_id, since, until) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (follower_id) DO UPDATE SET
                leader_id = excluded.leader_id, since = excluded.since, until = excluded.until",
            params![follower_id, leader_id, now, until],
        )?;
        Ok(())
    }

    /// S75: following ends. Whom it followed, if it still did.
    pub fn unfollow(&self, follower_id: &str, now: i64) -> rusqlite::Result<Option<String>> {
        let conn = self.conn();
        let leader: Option<String> = conn
            .query_row(
                "SELECT leader_id FROM bot_follows WHERE follower_id = ? AND until > ?",
                params![follower_id, now],
                |row| row.get(0),
            )
            .optional()?;
        conn.execute("DELETE FROM bot_follows WHERE follower_id = ?", [follower_id])?;
        Ok(leader)
    }

    /// S74: whom a bot follows now, and until when.
    pub fn leader_of(
        &self,
        follower_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<(String, i64)>> {
        self.conn()
            .query_row(
                "SELECT leader_id, until FROM bot_follows WHERE follower_id = ? AND until > ?",
                params![follower_id, now],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// S74: the bots following this one now.
    pub fn followers_of(&self, leader_id: &str, now: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT follower_id FROM bot_follows WHERE leader_id = ? AND until > ? ORDER BY since",
        )?;
        let rows = stmt.query_map(params![leader_id, now], |row| row.get(0))?;
        rows.collect()
    }
}

/// S13, S24, S39: whether a walk-in fits: `In` when it does, or why not.
fn walk_in_check(
    conn: &Connection,
    visit: &NewVisit,
    capacity: i64,
    max_bots: i64,
) -> rusqlite::Result<WalkedIn> {
    if let Some(agent_id) = visit.agent_id {
        let on_trip = exists(
            conn,
            "SELECT 1 FROM trips WHERE agent_id = ? AND status = 'here' AND ends_at > ?",
            params![agent_id, visit.now],
        )?;
        if on_trip {
            return Ok(WalkedIn::OnTrip);
        }
    }
    let here = exists(
        conn,
        "SELECT 1 FROM floor_visits WHERE hangout_id = ? AND who = ? AND until > ?",
        params![visit.hangout_id, visit.who, visit.now],
    )?;
    if !here {
        if count_on_floor(conn, visit.hangout_id, visit.now)? >= capacity {
            return Ok(WalkedIn::Full);
        }
        if visit.agent_id.is_some() {
            let bots: i64 = conn.query_row(
                "SELECT COUNT(*) FROM floor_visits
                 WHERE hangout_id = ? AND person_id = ? AND agent_id IS NOT NULL AND until > ?",
                params![visit.hangout_id, visit.person_id, visit.now],
                |row| row.get(0),
            )?;
            if bots >= max_bots {
                return Ok(WalkedIn::TooManyBots);
            }
        }
    }
    Ok(WalkedIn::In)
}

/// S56, S61: the room a bot is staying in if it still belongs there, or else its home.
fn staying_or_home(
    conn: &Connection,
    agent_id: &str,
    staying: Option<&str>,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT r.hangout_id FROM room_agents r
             JOIN agents a ON a.id = r.agent_id
             JOIN hangouts h ON h.id = r.hangout_id
             LEFT JOIN floors f ON f.hangout_id = r.hangout_id
             LEFT JOIN spaces s ON s.id = f.space_id
             LEFT JOIN bot_homes b ON b.agent_id = r.agent_id
             LEFT JOIN houses ho ON ho.id = b.house_id
                                AND r.hangout_id IN (ho.living_room, ho.porch)
             WHERE r.agent_id = ?1
               AND (r.hangout_id = ?2 OR ho.id IS NOT NULL
                    OR COALESCE(s.owner_id, h.created_by) = a.owner_id)
             ORDER BY r.hangout_id = ?2 DESC, r.hangout_id = ho.living_room DESC,
                      ho.id IS NOT NULL DESC, s.kind = 'house' DESC, r.rowid
             LIMIT 1",
        params![agent_id, staying],
        |row| row.get(0),
    )
    .optional()
}

/// #609: the last message in thread ?1 before ?2 whose thread mention event was recorded for bot ?3.
/// It's the bot's own event row, not the mentions table, since a mention is counted for a bot that
/// isn't sent the event (one at the recycling plant, RC2), and only a thread mention's (one with
/// this `thread_id`), since a watched workboard change (RW11) shares the key but carries no context.
/// A recorded event may still not have reached the bot, so the event says where its context starts
/// (`context_after`). Driven by the thread's (root_id, message_id) index, newest first, probing
/// bot_events' unique (agent_id, ref_key), so it doesn't grow with the bot's other events. Public so
/// a test can check its plan.
pub const LAST_THREAD_MENTION: &str = "SELECT lt.message_id FROM message_threads lt
     CROSS JOIN bot_events le
     WHERE lt.root_id = ?1 AND lt.message_id < ?2
       AND le.agent_id = ?3 AND le.ref_key = 'mention:' || lt.message_id
       AND json_extract(le.data, '$.thread_id') = ?1
     ORDER BY lt.message_id DESC LIMIT 1";

/// I117: the update behind `mark_answered_thread_mentions_read`. Driven by this bot's few unread
/// mentions in this room, through message_threads' primary key and its (root_id, message_id) index,
/// so it doesn't grow with the site. Public so a test can check its query plan.
pub const ANSWERED_THREAD_MENTIONS: &str = "UPDATE mentions SET read_at = ?1
     WHERE agent_id = ?2 AND hangout_id = ?3 AND read_at IS NULL
       AND EXISTS (SELECT 1 FROM message_threads t
                   JOIN message_threads mine
                     ON mine.root_id = t.root_id AND mine.message_id > t.message_id
                   JOIN messages m ON m.id = mine.message_id
                   WHERE t.message_id = mentions.message_id
                     AND t.message_id <> t.root_id
                     AND m.agent_id = ?2)";

impl Db {
    /// S56: the room a bot went to, in place of the last one.
    pub fn set_bot_place(
        &self,
        agent_id: &str,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_places (agent_id, hangout_id, since) VALUES (?1, ?2, ?3)
             ON CONFLICT (agent_id) DO UPDATE SET hangout_id = excluded.hangout_id, since = excluded.since",
            params![agent_id, hangout_id, now],
        )?;
        Ok(())
    }

    /// S61: a bot doing something in a room it belongs to starts its half hour there again. A room
    /// it's only a guest in isn't one to come back to, so nothing changes for those.
    pub fn note_bot_here(
        &self,
        agent_id: &str,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_places (agent_id, hangout_id, since)
             SELECT agent_id, hangout_id, ?3 FROM room_agents WHERE agent_id = ?1 AND hangout_id = ?2
             ON CONFLICT (agent_id) DO UPDATE SET hangout_id = excluded.hangout_id, since = excluded.since",
            params![agent_id, hangout_id, now],
        )?;
        Ok(())
    }

    /// L9: when a bot last went to a room itself, if it has.
    pub fn bot_place_since(&self, agent_id: &str) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row("SELECT since FROM bot_places WHERE agent_id = ?", [agent_id], |row| {
                row.get(0)
            })
            .optional()
    }

    /// S58: whether a bot is on a trip, and if so whether this room is one it's a guest in.
    pub fn on_trip_away_from(
        &self,
        hangout_id: &str,
        agent_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let on_trip = exists(
            &conn,
            "SELECT 1 FROM trips WHERE agent_id = ? AND status = 'here' AND ends_at > ?",
            params![agent_id, now],
        )?;
        Ok(on_trip && !guest_on_trip(&conn, hangout_id, agent_id, now)?)
    }

    /// Ends a walk-in. `who` is a person's id or their bot's.
    pub fn end_visit(&self, hangout_id: &str, who: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM floor_visits WHERE hangout_id = ? AND who = ?",
            params![hangout_id, who],
        )?;
        Ok(())
    }

    /// Ends a walk-in and remembers why, so a later read can say so (S62). `who` is the bot's id.
    pub fn end_visit_as(
        &self,
        hangout_id: &str,
        who: &str,
        now: i64,
        reason: &str,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "DELETE FROM floor_visits WHERE hangout_id = ?1 AND (who = ?2 OR agent_id = ?2)",
            params![hangout_id, who],
        )?;
        if changed > 0 {
            remember_leave(&tx, who, hangout_id, now, reason)?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// The public floors this bot is still walked onto. `who` and `agent_id` both count.
    pub fn bot_visits(&self, agent_id: &str, now: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT hangout_id FROM floor_visits
             WHERE (who = ?1 OR agent_id = ?1) AND until > ?2 ORDER BY since",
        )?;
        let rows = stmt.query_map(params![agent_id, now], |row| row.get(0))?;
        rows.collect()
    }

    /// When this caller's walk-in on a floor ran out, if the row is still there.
    pub fn visit_ended(
        &self,
        who: &str,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT until FROM floor_visits
                 WHERE hangout_id = ?1 AND (who = ?2 OR agent_id = ?2) AND until <= ?3
                 ORDER BY until DESC LIMIT 1",
                params![hangout_id, who, now],
                |row| row.get(0),
            )
            .optional()
    }

    /// Why this caller left a floor, if it was recorded.
    pub fn floor_leave(
        &self,
        who: &str,
        hangout_id: &str,
    ) -> rusqlite::Result<Option<(i64, String)>> {
        self.conn()
            .query_row(
                "SELECT left_at, reason FROM floor_leaves WHERE who = ? AND hangout_id = ?",
                params![who, hangout_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// The latest floor this bot left, if it was within `since`.
    pub fn latest_floor_leave(
        &self,
        who: &str,
        since: i64,
    ) -> rusqlite::Result<Option<FloorLeave>> {
        self.conn()
            .query_row(
                "SELECT l.hangout_id, h.name, l.left_at, l.reason
                 FROM floor_leaves l JOIN hangouts h ON h.id = l.hangout_id
                 WHERE l.who = ? AND l.left_at >= ? ORDER BY l.left_at DESC LIMIT 1",
                params![who, since],
                floor_leave_from_row,
            )
            .optional()
    }

    /// A floor this bot left at or after `since`, the latest one.
    pub fn floor_left_since(&self, who: &str, since: i64) -> rusqlite::Result<Option<FloorLeave>> {
        self.latest_floor_leave(who, since)
    }

    /// The lowest floor of this person's house, if they have one. It doesn't build one.
    pub fn owner_house_floor(&self, owner_id: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT f.hangout_id, h.name FROM spaces s
                 JOIN floors f ON f.space_id = s.id
                 JOIN hangouts h ON h.id = f.hangout_id
                 WHERE s.owner_id = ? AND s.kind = 'house'
                 ORDER BY f.level LIMIT 1",
                [owner_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    pub fn add_mention(
        &self,
        agent_id: &str,
        hangout_id: &str,
        message_id: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO mentions (hangout_id, message_id, agent_id, created_at)
             VALUES (?, ?, ?, ?)",
            params![hangout_id, message_id, agent_id, now],
        )?;
        Ok(())
    }

    /// E22: whether a ring in this room, newer than `since`, is unread. A mention that rang nothing
    /// doesn't count, or one that couldn't ring would hold the bell against one that can.
    pub fn wake_ring_unread(
        &self,
        agent_id: &str,
        hangout_id: &str,
        since: i64,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM wake_rings r
             JOIN mentions m ON m.message_id = r.message_id AND m.agent_id = r.agent_id
             WHERE r.agent_id = ?1 AND m.hangout_id = ?2 AND r.rung_at > ?3 AND m.read_at IS NULL
             LIMIT 1",
            params![agent_id, hangout_id, since],
        )
    }

    /// E24: records that this message rang this bot and returns whether it did. `capped` marks a
    /// bot-to-bot ring so a chain hold can count it (L10). Mentions are never refused for a daily count.
    pub fn note_wake_ring(
        &self,
        agent_id: &str,
        message_id: i64,
        now: i64,
        capped: bool,
    ) -> rusqlite::Result<bool> {
        const DAY_SECS: i64 = 24 * 60 * 60;
        let conn = self.conn();
        conn.execute("DELETE FROM wake_rings WHERE rung_at < ?", [now - DAY_SECS])?;
        let rung = conn.execute(
            "INSERT OR IGNORE INTO wake_rings (message_id, agent_id, capped, rung_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![message_id, agent_id, capped, now],
        )?;
        Ok(rung > 0)
    }

    /// L10: bot-originated mention rings in this room since a person last posted there.
    pub fn bot_mention_chain(&self, agent_id: &str, hangout_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM wake_rings r
             JOIN messages m ON m.id = r.message_id
             WHERE r.agent_id = ?1 AND m.hangout_id = ?2 AND r.capped = 1
               AND m.agent_id IS NOT NULL
               AND m.id > IFNULL((
                   SELECT MAX(id) FROM messages
                   WHERE hangout_id = ?2 AND kind = 'chat' AND agent_id IS NULL
                     AND id NOT IN (SELECT message_id FROM keyless_messages)
               ), 0)",
            params![agent_id, hangout_id],
            |row| row.get(0),
        )
    }

    /// L10: when this room already has a hold for this bot, so a later mention can join it.
    pub fn held_mention_due_at(
        &self,
        agent_id: &str,
        hangout_id: &str,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn().query_row(
            "SELECT MIN(due_at) FROM held_mention_wakes WHERE agent_id = ? AND hangout_id = ?",
            params![agent_id, hangout_id],
            |row| row.get(0),
        )
    }

    /// L10: remember a bot mention until the chain hold ends, then wake for it.
    /// A mention that arrives during an open hold joins that hold's due time, so one wake includes
    /// every held mention.
    pub fn hold_mention_wake(
        &self,
        agent_id: &str,
        hangout_id: &str,
        message_id: i64,
        due_at: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        let due_at = self.held_mention_due_at(agent_id, hangout_id)?.unwrap_or(due_at);
        self.conn().execute(
            "INSERT OR IGNORE INTO held_mention_wakes
             (agent_id, hangout_id, message_id, due_at, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![agent_id, hangout_id, message_id, due_at, now],
        )?;
        Ok(())
    }

    /// L10: bot mentions this bot's doorbell must not send again: the server is holding them, or
    /// has already rung them.
    pub fn bot_mention_wake_ids(&self, agent_id: &str) -> rusqlite::Result<BotMentionWakeIds> {
        fn group(rows: Vec<(String, i64)>) -> BTreeMap<String, Vec<i64>> {
            let mut map: BTreeMap<String, Vec<i64>> = BTreeMap::new();
            for (hangout, id) in rows {
                map.entry(hangout).or_default().push(id);
            }
            map
        }
        let conn = self.conn();
        let mut held_stmt = conn.prepare(
            "SELECT hangout_id, message_id FROM held_mention_wakes
             WHERE agent_id = ? ORDER BY hangout_id, message_id",
        )?;
        let held = held_stmt
            .query_map([agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut rung_stmt = conn.prepare(
            "SELECT m.hangout_id, r.message_id FROM wake_rings r
             JOIN messages m ON m.id = r.message_id
             WHERE r.agent_id = ? ORDER BY m.hangout_id, r.message_id",
        )?;
        let rung = rung_stmt
            .query_map([agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(BotMentionWakeIds { held: group(held), rung: group(rung) })
    }

    fn group_held_mentions(rows: Vec<(String, String, i64)>) -> Vec<(String, String, Vec<i64>)> {
        let mut groups: Vec<(String, String, Vec<i64>)> = Vec::new();
        for (agent_id, hangout_id, message_id) in rows {
            match groups.last_mut() {
                Some((agent, hangout, ids)) if agent == &agent_id && hangout == &hangout_id => {
                    ids.push(message_id);
                }
                _ => groups.push((agent_id, hangout_id, vec![message_id])),
            }
        }
        groups
    }

    /// L10: held bot mentions whose wait is over.
    pub fn held_mentions_due(&self, now: i64) -> rusqlite::Result<Vec<(String, String, Vec<i64>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT agent_id, hangout_id, message_id FROM held_mention_wakes
             WHERE due_at <= ? ORDER BY agent_id, hangout_id, message_id",
        )?;
        let rows = stmt
            .query_map([now], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::group_held_mentions(rows))
    }

    /// L10: held bot mentions in this room, so a person's chat can deliver them now.
    pub fn held_mentions_in(
        &self,
        hangout_id: &str,
    ) -> rusqlite::Result<Vec<(String, String, Vec<i64>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT agent_id, hangout_id, message_id FROM held_mention_wakes
             WHERE hangout_id = ? ORDER BY agent_id, message_id",
        )?;
        let rows = stmt
            .query_map([hangout_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Self::group_held_mentions(rows))
    }

    /// Claim only the selected rows. Reserving the wake in the same transaction means a stale
    /// or concurrent flusher cannot send twice, and a reservation failure leaves the queue intact.
    /// With no reservation time, discard only this selected batch (for example, Only me).
    /// Every claimed mention is recorded as rung so a doorbell cannot send the same burst again;
    /// only the latest counts toward the chain hold.
    pub fn take_held_mentions(
        &self,
        agent_id: &str,
        hangout_id: &str,
        message_ids: &[i64],
        reserve_at: Option<i64>,
    ) -> rusqlite::Result<Option<i64>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut claimed = Vec::new();
        for &message_id in message_ids {
            if tx.execute(
                "DELETE FROM held_mention_wakes
                 WHERE agent_id = ? AND hangout_id = ? AND message_id = ?",
                params![agent_id, hangout_id, message_id],
            )? > 0
            {
                claimed.push(message_id);
            }
        }
        let latest = claimed.iter().copied().max();
        if let (Some(message_id), Some(at)) = (latest, reserve_at) {
            // Keep the same retention as an immediate ring, even for a continuously held chain.
            tx.execute("DELETE FROM wake_rings WHERE rung_at < ?", [at - 24 * 60 * 60])?;
            let reserved = tx.execute(
                "INSERT OR IGNORE INTO wake_rings (message_id, agent_id, capped, rung_at)
                 VALUES (?, ?, 1, ?)",
                params![message_id, agent_id, at],
            )?;
            if reserved == 0 {
                tx.commit()?;
                return Ok(None);
            }
            for &id in &claimed {
                if id != message_id {
                    tx.execute(
                        "INSERT OR IGNORE INTO wake_rings (message_id, agent_id, capped, rung_at)
                         VALUES (?, ?, 0, ?)",
                        params![id, agent_id, at],
                    )?;
                }
            }
            tx.commit()?;
            return Ok(Some(message_id));
        }
        tx.commit()?;
        Ok(latest)
    }

    /// E31: whether this room accepted a non-owner person's wake after `since`.
    pub fn person_wake_recent(
        &self,
        agent_id: &str,
        hangout_id: &str,
        since: i64,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM wake_accepts
             WHERE agent_id = ?1 AND hangout_id = ?2 AND accepted_at > ?3
             LIMIT 1",
            params![agent_id, hangout_id, since],
        )
    }

    /// E31: the receiver accepted this person's wake. That starts the 3-minute spacing, and an
    /// unread copy of it can still hold the owner's bell. A failed delivery never calls this.
    pub fn note_person_wake(
        &self,
        agent_id: &str,
        message_id: i64,
        hangout_id: &str,
        at: i64,
    ) -> rusqlite::Result<()> {
        const DAY_SECS: i64 = 24 * 60 * 60;
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM wake_accepts WHERE accepted_at < ?", [at - DAY_SECS])?;
        tx.execute(
            "INSERT OR IGNORE INTO wake_accepts (message_id, agent_id, hangout_id, accepted_at)
             VALUES (?, ?, ?, ?)",
            params![message_id, agent_id, hangout_id, at],
        )?;
        tx.execute("DELETE FROM wake_rings WHERE rung_at < ?", [at - DAY_SECS])?;
        tx.execute(
            "INSERT OR IGNORE INTO wake_rings (message_id, agent_id, capped, rung_at)
             VALUES (?, ?, 0, ?)",
            params![message_id, agent_id, at],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Moves this bot's accepted person wakes to `at`. Tests use it to step past the 3 minutes.
    pub fn set_wake_accepts_at(&self, agent_id: &str, at: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE wake_accepts SET accepted_at = ? WHERE agent_id = ?",
            params![at, agent_id],
        )?;
        Ok(())
    }

    pub fn delete_wake_accepts(&self, agent_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM wake_accepts WHERE agent_id = ?", [agent_id])?;
        Ok(())
    }

    /// E31: keeps the latest decisions for this bot. `hangout_id` empty is stored as none.
    pub fn note_wake_decision(
        &self,
        agent_id: &str,
        hangout_id: Option<&str>,
        message_id: Option<i64>,
        decision: &str,
        reason: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        const WEEK_SECS: i64 = 7 * 24 * 60 * 60;
        let hangout_id = hangout_id.filter(|id| !id.is_empty());
        let conn = self.conn();
        conn.execute("DELETE FROM wake_decisions WHERE created_at < ?", [now - WEEK_SECS])?;
        conn.execute(
            "INSERT INTO wake_decisions (agent_id, hangout_id, message_id, decision, reason, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![agent_id, hangout_id, message_id, decision, reason, now],
        )?;
        conn.execute(
            "DELETE FROM wake_decisions WHERE agent_id = ?1 AND id < (
                SELECT MIN(id) FROM (
                    SELECT id FROM wake_decisions WHERE agent_id = ?1 ORDER BY id DESC LIMIT 40
                )
             )",
            [agent_id],
        )?;
        Ok(())
    }

    /// E45: the latest wake decision for this bot about one message, and its reason, if any was
    /// stored yet: a ring that went out is stored only once it's answered.
    pub fn wake_decision_for(
        &self,
        agent_id: &str,
        message_id: i64,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT decision, reason FROM wake_decisions
                 WHERE agent_id = ? AND message_id = ? ORDER BY id DESC LIMIT 1",
                params![agent_id, message_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    pub fn wake_decisions(&self, agent_id: &str) -> rusqlite::Result<Vec<WakeDecision>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT d.decision, d.reason, d.hangout_id, h.name, d.created_at
             FROM wake_decisions d
             LEFT JOIN hangouts h ON h.id = d.hangout_id
             WHERE d.agent_id = ?
             ORDER BY d.id DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map([agent_id], |row| {
            Ok(WakeDecision {
                decision: row.get(0)?,
                reason: row.get(1)?,
                hangout_id: row.get(2)?,
                room: row.get(3)?,
                at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// L29: records a call to meet for this half hour and returns whether it's new.
    pub fn note_meet_call(
        &self,
        agent_id: &str,
        slot: i64,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        const DAY_SECS: i64 = 24 * 60 * 60;
        let conn = self.conn();
        conn.execute("DELETE FROM meet_calls WHERE rung_at < ?", [now - DAY_SECS])?;
        let rung = conn.execute(
            "INSERT OR IGNORE INTO meet_calls (agent_id, slot, hangout_id, rung_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, slot, hangout_id, now],
        )?;
        Ok(rung > 0)
    }

    /// L44: the room and time of the last thing a bot said, if it ever has.
    pub fn last_line_of(&self, agent_id: &str) -> rusqlite::Result<Option<(String, i64)>> {
        self.conn()
            .query_row(
                "SELECT hangout_id, created_at FROM messages WHERE agent_id = ? AND kind = 'chat'
                 ORDER BY id DESC LIMIT 1",
                [agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// L31: whether a bot is working on a reply in any room right now (I58).
    pub fn bot_is_thinking(&self, agent_id: &str, now: i64) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM bot_thinking WHERE agent_id = ? AND until_at > ? LIMIT 1",
            params![agent_id, now],
        )
    }

    /// L27: every bot whose owner turned public places on, with its owner.
    /// S85: bots, never a house bot, with a walk-in that ends after `now` and by `by`, and their
    /// owners.
    pub fn bots_with_visits_ending(
        &self,
        now: i64,
        by: i64,
    ) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT a.id, a.owner_id FROM floor_visits v JOIN agents a ON a.id = v.agent_id
              WHERE v.until > ?1 AND v.until <= ?2
                AND NOT EXISTS (SELECT 1 FROM bot_homes h WHERE h.agent_id = a.id AND h.is_house = 1)
              ORDER BY a.id",
        )?;
        let rows = stmt.query_map(params![now, by], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn bots_going_out(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT a.id, a.owner_id FROM agents a
             WHERE a.id IN (SELECT agent_id FROM bot_routines WHERE public_places = 1)
                OR a.id IN (SELECT agent_id FROM bot_schedules)
             ORDER BY a.id",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// E25: whether only this bot's owner can wake it with an @mention.
    pub fn wakes_only_for_owner(&self, agent_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM wake_settings WHERE agent_id = ? AND only_me = 1",
            [agent_id],
        )
    }

    pub fn set_wakes_only_for_owner(&self, agent_id: &str, only_me: bool) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO wake_settings (agent_id, only_me) VALUES (?, ?)
             ON CONFLICT (agent_id) DO UPDATE SET only_me = excluded.only_me",
            params![agent_id, only_me],
        )?;
        Ok(())
    }

    pub fn trusted_bots(&self, agent_id: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut query = conn.prepare(
            "SELECT sender_id FROM trusted_bot_wakes WHERE agent_id = ? ORDER BY sender_id",
        )?;
        let rows = query.query_map([agent_id], |row| row.get(0))?;
        rows.collect()
    }

    /// Everything has been validated by the owner operation; save all wake settings together.
    pub fn save_agent_notify(
        &self,
        agent_id: &str,
        url: Option<&str>,
        auth: Option<&str>,
        only_me: Option<bool>,
        trusted_bots: Option<&[String]>,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (table, column, value) in
            [("notify_targets", "url", url), ("notify_auth", "secret", auth)]
        {
            match value {
                Some(value) => tx.execute(
                    &format!(
                        "INSERT INTO {table} (owner_id, {column}) VALUES (?1, ?2)
                              ON CONFLICT (owner_id) DO UPDATE SET {column} = excluded.{column}"
                    ),
                    params![agent_id, value],
                )?,
                None => {
                    tx.execute(&format!("DELETE FROM {table} WHERE owner_id = ?"), [agent_id])?
                }
            };
        }
        if let Some(only_me) = only_me {
            tx.execute(
                "INSERT INTO wake_settings (agent_id, only_me) VALUES (?, ?)
                 ON CONFLICT (agent_id) DO UPDATE SET only_me = excluded.only_me",
                params![agent_id, only_me],
            )?;
        }
        if let Some(bots) = trusted_bots {
            tx.execute("DELETE FROM trusted_bot_wakes WHERE agent_id = ?", [agent_id])?;
            for sender in bots {
                tx.execute(
                    "INSERT INTO trusted_bot_wakes (agent_id, sender_id) VALUES (?, ?)",
                    params![agent_id, sender],
                )?;
            }
        }
        tx.commit()
    }

    /// Called only for fresh chat authenticated as a human in post_message, never for bot chat.
    pub fn note_bot_wake_owner_chat(&self, message_id: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_wake_owner_chat (space_id, message_id)
             SELECT s.id, m.id FROM messages m
             JOIN floors f ON f.hangout_id = m.hangout_id
             JOIN spaces s ON s.id = f.space_id
             WHERE m.id = ? AND m.kind = 'chat' AND m.agent_id IS NULL
               AND s.kind = 'house' AND s.owner_id = m.person_id
             ON CONFLICT (space_id) DO UPDATE SET message_id = MAX(message_id, excluded.message_id)",
            [message_id],
        )?;
        Ok(())
    }

    /// E26-E27: atomically check live permission and reserve one bot ring. No human ring is used.
    pub fn note_bot_wake_ring(
        &self,
        agent_id: &str,
        message_id: i64,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let rung = self.conn().execute(
            "INSERT OR IGNORE INTO bot_wake_rings (message_id, agent_id, space_id, rung_at)
             SELECT m.id, target.id, s.id, ?3 FROM messages m
             JOIN agents sender ON sender.id = m.agent_id
             JOIN agents target ON target.id = ?1 AND target.owner_id = sender.owner_id
             JOIN trusted_bot_wakes grant_ ON grant_.agent_id = target.id AND grant_.sender_id = sender.id
             JOIN room_agents sr ON sr.agent_id = sender.id AND sr.hangout_id = m.hangout_id
             JOIN room_agents tr ON tr.agent_id = target.id AND tr.hangout_id = m.hangout_id
             JOIN floors f ON f.hangout_id = m.hangout_id
             JOIN spaces s ON s.id = f.space_id AND s.kind = 'house' AND s.owner_id = target.owner_id
             LEFT JOIN bot_wake_owner_chat owner_chat ON owner_chat.space_id = s.id
             WHERE m.id = ?2 AND m.kind = 'chat' AND m.person_id = sender.owner_id
               AND sender.id != target.id
               AND NOT EXISTS (SELECT 1 FROM bot_homes WHERE agent_id IN (sender.id, target.id) AND is_house = 1)
               AND EXISTS (SELECT 1 FROM notify_targets WHERE owner_id = target.id)
               AND NOT EXISTS (SELECT 1 FROM agent_doorbell WHERE agent_id = target.id AND paused = 1)
               AND NOT EXISTS (SELECT 1 FROM wake_settings WHERE agent_id = target.id AND only_me = 1)
               AND m.id > COALESCE(owner_chat.message_id, 0)
               AND NOT EXISTS (SELECT 1 FROM bot_wake_rings WHERE agent_id = target.id
                               AND (rung_at > ?3 - 180 OR message_id >= m.id))
               AND (SELECT COUNT(*) FROM bot_wake_rings
                    WHERE agent_id = target.id AND space_id = s.id
                      AND message_id > COALESCE(owner_chat.message_id, 0)) < 5",
            params![agent_id, message_id, now],
        )?;
        Ok(rung > 0)
    }

    /// I83: whether a message @mentioned this bot.
    /// I84: the bots a message @mentions, as the server recorded them.
    pub fn mentioned_agents(&self, message_id: i64, now: i64) -> rusqlite::Result<Vec<Agent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {AGENT_COLS} FROM {AGENTS}
             JOIN mentions m ON m.agent_id = a.id
             WHERE m.message_id = ? ORDER BY m.id"
        ))?;
        let rows = stmt.query_map([message_id], |row| agent_from_row(row, now))?;
        rows.collect()
    }

    pub fn mentions_bot(&self, message_id: i64, agent_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM mentions WHERE message_id = ? AND agent_id = ?",
            params![message_id, agent_id],
        )
    }

    /// I116: the section tags this bot's last brief read of this room returned, if that was
    /// after `since`.
    pub fn brief_seen(
        &self,
        agent_id: &str,
        hangout_id: &str,
        since: i64,
    ) -> rusqlite::Result<Option<serde_json::Map<String, serde_json::Value>>> {
        let tags: Option<String> = self
            .conn()
            .query_row(
                "SELECT tags FROM brief_seen WHERE agent_id = ? AND hangout_id = ? AND at > ?",
                params![agent_id, hangout_id, since],
                |row| row.get(0),
            )
            .optional()?;
        Ok(tags.and_then(|tags| serde_json::from_str(&tags).ok()))
    }

    /// I122: when each of these bots last said something (a chat line) in this room, at or after
    /// `since`. Bots that haven't are left out.
    pub fn last_posted_at(
        &self,
        hangout_id: &str,
        agent_ids: &[&str],
        since: i64,
    ) -> rusqlite::Result<HashMap<String, i64>> {
        let conn = self.conn();
        let mut statement = conn.prepare_cached(
            "SELECT created_at FROM messages INDEXED BY messages_by_room_bot
             WHERE hangout_id = ? AND agent_id = ? AND kind = 'chat' ORDER BY id DESC LIMIT 1",
        )?;
        let mut found = HashMap::new();
        for agent_id in agent_ids {
            let at: Option<i64> =
                statement.query_row(params![hangout_id, agent_id], |row| row.get(0)).optional()?;
            if let Some(at) = at.filter(|at| *at >= since) {
                found.insert((*agent_id).to_string(), at);
            }
        }
        Ok(found)
    }

    /// I116: what this brief read returned, in place of what the last one did.
    pub fn note_brief_seen(
        &self,
        agent_id: &str,
        hangout_id: &str,
        tags: &serde_json::Map<String, serde_json::Value>,
        at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO brief_seen (agent_id, hangout_id, tags, at) VALUES (?, ?, ?, ?)
             ON CONFLICT (agent_id, hangout_id) DO UPDATE SET tags = excluded.tags, at = excluded.at",
            params![agent_id, hangout_id, serde_json::Value::Object(tags.clone()).to_string(), at],
        )?;
        Ok(())
    }

    /// I107: an owner turned the waiting pulse off for this bot.
    pub fn waiting_pulse_enabled(&self, agent_id: &str) -> rusqlite::Result<bool> {
        Ok(!exists(
            &self.conn(),
            "SELECT 1 FROM waiting_pulse_off WHERE agent_id = ?",
            params![agent_id],
        )?)
    }

    pub fn set_waiting_pulse(&self, agent_id: &str, on: bool) -> rusqlite::Result<()> {
        let conn = self.conn();
        if on {
            conn.execute("DELETE FROM waiting_pulse_off WHERE agent_id = ?", params![agent_id])?;
        } else {
            conn.execute(
                "INSERT OR IGNORE INTO waiting_pulse_off (agent_id) VALUES (?)",
                params![agent_id],
            )?;
        }
        Ok(())
    }

    pub fn unread_mentions(&self, agent_id: &str) -> rusqlite::Result<Vec<Mention>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.hangout_id, h.name, m.message_id, msg.speaker, msg.body, m.created_at
             FROM mentions m
             JOIN hangouts h ON h.id = m.hangout_id
             JOIN messages msg ON msg.id = m.message_id
             WHERE m.agent_id = ? AND m.read_at IS NULL
             ORDER BY m.id",
        )?;
        let rows = stmt.query_map([agent_id], mention_from_row)?;
        rows.collect()
    }

    pub fn mark_mentions_read(
        &self,
        agent_id: &str,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE mentions SET read_at = ? WHERE agent_id = ? AND hangout_id = ? AND read_at IS NULL AND message_id NOT IN (SELECT message_id FROM message_replies)",
            params![now, agent_id, hangout_id],
        )?;
        Ok(())
    }

    /// I117: a mention in a thread reply is read once the bot has posted in that thread after it:
    /// it answered there, from the wake or after reading the thread. Only replies in this room.
    pub fn mark_answered_thread_mentions_read(
        &self,
        agent_id: &str,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<usize> {
        self.conn().execute(ANSWERED_THREAD_MENTIONS, params![now, agent_id, hangout_id])
    }

    /// I141, #669: the replies under each of these thread roots, up to `upto`, with an unread mention
    /// of this bot, by root, oldest first.
    pub fn thread_mentions_for_roots(
        &self,
        agent_id: &str,
        root_ids: &[i64],
        upto: i64,
    ) -> rusqlite::Result<std::collections::HashMap<i64, Vec<i64>>> {
        if root_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let in_clause = root_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT t.root_id, m.message_id FROM mentions m
             JOIN message_threads t ON t.message_id = m.message_id AND t.message_id != t.root_id
             WHERE m.agent_id = ? AND m.read_at IS NULL AND m.message_id <= ?
               AND t.root_id IN ({in_clause})
             ORDER BY m.message_id"
        ))?;
        let rows = stmt.query_map(params![agent_id, upto], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut by_root: std::collections::HashMap<i64, Vec<i64>> =
            std::collections::HashMap::new();
        for row in rows {
            let (root, message) = row?;
            by_root.entry(root).or_default().push(message);
        }
        Ok(by_root)
    }

    /// I141, #669: marks read this bot's mentions in replies under these thread roots, up to
    /// `upto`, once `read_room` has shown the roots with how many there were.
    pub fn mark_thread_mentions_read_for_roots(
        &self,
        agent_id: &str,
        root_ids: &[i64],
        upto: i64,
        now: i64,
    ) -> rusqlite::Result<usize> {
        if root_ids.is_empty() {
            return Ok(0);
        }
        let in_clause = root_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        self.conn().execute(
            &format!(
                "UPDATE mentions SET read_at = ?
                 WHERE agent_id = ? AND read_at IS NULL AND message_id <= ?
                   AND message_id IN (SELECT message_id FROM message_threads
                                      WHERE root_id IN ({in_clause}) AND message_id != root_id)"
            ),
            params![now, agent_id, upto],
        )
    }

    /// I104: mark mentions for specific viewed message ids in a thread as read.
    pub fn mark_thread_mentions_read(
        &self,
        agent_id: &str,
        message_ids: &[i64],
        now: i64,
    ) -> rusqlite::Result<()> {
        if message_ids.is_empty() {
            return Ok(());
        }
        let in_clause = message_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "UPDATE mentions SET read_at = ?
             WHERE agent_id = ? AND read_at IS NULL AND message_id IN ({in_clause})"
        );
        self.conn().execute(&sql, params![now, agent_id])?;
        Ok(())
    }

    /// I58: stored thinking in a room, whether or not it has run out.
    pub fn thinking_row(
        &self,
        agent_id: &str,
        hangout_id: &str,
    ) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT started_at, until_at FROM bot_thinking WHERE agent_id = ? AND hangout_id = ?",
                params![agent_id, hangout_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    pub fn put_thinking(
        &self,
        agent_id: &str,
        hangout_id: &str,
        started_at: i64,
        until_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_thinking (agent_id, hangout_id, started_at, until_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (agent_id, hangout_id) DO UPDATE SET
                started_at = excluded.started_at, until_at = excluded.until_at",
            params![agent_id, hangout_id, started_at, until_at],
        )?;
        Ok(())
    }

    /// I61: stretch live thinking in every room, and say which rooms changed.
    pub fn extend_live_thinking(
        &self,
        agent_id: &str,
        until_at: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT hangout_id FROM bot_thinking WHERE agent_id = ? AND until_at > ?")?;
        let rooms = stmt
            .query_map(params![agent_id, now], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        if !rooms.is_empty() {
            conn.execute(
                "UPDATE bot_thinking SET until_at = ?1 WHERE agent_id = ?2 AND until_at > ?3",
                params![until_at, agent_id, now],
            )?;
        }
        Ok(rooms)
    }

    /// Newest unread @mention of this bot in this room, in the last `since` window.
    pub fn latest_unread_mention_at(
        &self,
        agent_id: &str,
        hangout_id: &str,
        since: i64,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn().query_row(
            "SELECT MAX(created_at) FROM mentions
                 WHERE agent_id = ? AND hangout_id = ? AND read_at IS NULL AND created_at > ?",
            params![agent_id, hangout_id, since],
            |row| row.get(0),
        )
    }

    /// Rooms this bot has an unread @mention in, from the last `since` window.
    pub fn unread_mention_rooms(
        &self,
        agent_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT hangout_id FROM mentions
             WHERE agent_id = ? AND read_at IS NULL AND created_at > ?",
        )?;
        let rows = stmt.query_map(params![agent_id, since], |row| row.get(0))?;
        rows.collect()
    }

    /// I58: waiting or thinking for each bot drawn in this room.
    pub fn thinking_in(
        &self,
        hangout_id: &str,
        now: i64,
        window: i64,
    ) -> rusqlite::Result<HashMap<String, Thinking>> {
        let conn = self.conn();
        let mut rows: HashMap<String, (i64, i64)> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT agent_id, started_at, until_at FROM bot_thinking WHERE hangout_id = ?",
            )?;
            let mapped = stmt.query_map([hangout_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
            })?;
            for row in mapped {
                let (agent_id, started_at, until_at) = row?;
                rows.insert(agent_id, (started_at, until_at));
            }
        }
        let mut mentions: HashMap<String, i64> = HashMap::new();
        {
            let mut stmt = conn.prepare(
                "SELECT agent_id, MAX(created_at) FROM mentions
                 WHERE hangout_id = ? AND read_at IS NULL AND created_at > ?
                 GROUP BY agent_id",
            )?;
            let mapped = stmt.query_map(params![hangout_id, now - window], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            for row in mapped {
                let (agent_id, at) = row?;
                mentions.insert(agent_id, at);
            }
        }
        let mut out = HashMap::new();
        let mut seen = HashSet::new();
        for (agent_id, (started_at, until_at)) in &rows {
            seen.insert(agent_id.clone());
            if *until_at > now {
                out.insert(agent_id.clone(), Thinking { phase: "thinking", until: *until_at });
                continue;
            }
            if let Some(at) = mentions.get(agent_id) {
                if *at > *started_at {
                    out.insert(
                        agent_id.clone(),
                        Thinking { phase: "waiting", until: *at + window },
                    );
                }
            }
        }
        for (agent_id, at) in mentions {
            if seen.contains(&agent_id) {
                continue;
            }
            out.insert(agent_id, Thinking { phase: "waiting", until: at + window });
        }
        Ok(out)
    }

    /// A bot's walk-in on a floor: when it started (S19) and when it ends, ended or not.
    pub fn visit_times(
        &self,
        hangout_id: &str,
        agent_id: &str,
    ) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT since, until FROM floor_visits WHERE hangout_id = ? AND agent_id = ?",
                params![hangout_id, agent_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// Moves a walk-in's times, which is how a test makes one run out. False if there's none.
    pub fn set_visit_times(
        &self,
        hangout_id: &str,
        who: &str,
        since: i64,
        until: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE floor_visits SET since = ?, until = ? WHERE hangout_id = ? AND who = ?",
            params![since, until, hangout_id, who],
        )?;
        Ok(changed > 0)
    }

    /// S27: takes someone and their bots off every floor of a space and keeps them off, with every
    /// link they saw. It's one step, because someone who's out can't be turned out again to finish
    /// it. Their answers to plans here that haven't started go too, and with them being a guest of
    /// one and its link. Gives back those plans, because each may need to come off the calendar.
    pub fn turn_out(
        &self,
        space_id: &str,
        person_id: &str,
        left: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<String>> {
        let floors = "SELECT hangout_id FROM floors WHERE space_id = ?1";
        let plans =
            format!("SELECT id FROM events WHERE hangout_id IN ({floors}) AND starts_at > ?3");
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for floor in floors_seen_by(&tx, space_id, person_id, left)? {
            tx.execute(
                "UPDATE hangouts SET invite_code = lower(hex(randomblob(12))) WHERE id = ?",
                [&floor],
            )?;
            end_friend_invites(&tx, "room", &floor)?;
            tx.execute(
                "DELETE FROM plan_invites WHERE event_id IN (
                    SELECT id FROM events WHERE hangout_id = ?)",
                [&floor],
            )?;
        }
        let answered = strings(
            &tx,
            &format!("SELECT event_id FROM rsvps WHERE person_id = ?2 AND event_id IN ({plans})"),
            params![space_id, person_id, now],
        )?;
        tx.execute(
            &format!(
                "DELETE FROM plan_invites WHERE event_id IN (
                    SELECT event_id FROM plan_invitees
                    WHERE person_id = ?2 AND event_id IN ({plans}))"
            ),
            params![space_id, person_id, now],
        )?;
        tx.execute(
            &format!("DELETE FROM plan_invitees WHERE person_id = ?2 AND event_id IN ({plans})"),
            params![space_id, person_id, now],
        )?;
        tx.execute(
            &format!("DELETE FROM rsvps WHERE person_id = ?2 AND event_id IN ({plans})"),
            params![space_id, person_id, now],
        )?;
        tx.execute(
            &format!(
                "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
                 SELECT r.agent_id, r.hangout_id, ?3, 'you were turned out of this place'
                 FROM room_agents r JOIN agents a ON a.id = r.agent_id
                 WHERE r.hangout_id IN ({floors}) AND a.owner_id = ?2
                 ON CONFLICT (who, hangout_id) DO UPDATE SET
                    left_at = excluded.left_at, reason = excluded.reason"
            ),
            params![space_id, person_id, now],
        )?;
        tx.execute(
            &format!(
                "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
                 SELECT who, hangout_id, ?3, 'you were turned out of this place'
                 FROM floor_visits WHERE hangout_id IN ({floors}) AND person_id = ?2
                 ON CONFLICT (who, hangout_id) DO UPDATE SET
                    left_at = excluded.left_at, reason = excluded.reason"
            ),
            params![space_id, person_id, now],
        )?;
        tx.execute(
            &format!(
                "DELETE FROM room_agents WHERE hangout_id IN ({floors})
                 AND agent_id IN (SELECT id FROM agents WHERE owner_id = ?2)"
            ),
            params![space_id, person_id],
        )?;
        tx.execute(
            &format!("DELETE FROM members WHERE hangout_id IN ({floors}) AND person_id = ?2"),
            params![space_id, person_id],
        )?;
        tx.execute(
            &format!("DELETE FROM floor_visits WHERE hangout_id IN ({floors}) AND person_id = ?2"),
            params![space_id, person_id],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO turned_out (space_id, person_id, created_at) VALUES (?, ?, ?)",
            params![space_id, person_id, now],
        )?;
        tx.commit()?;
        Ok(answered)
    }

    pub fn let_back(&self, space_id: &str, person_id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM turned_out WHERE space_id = ? AND person_id = ?",
            params![space_id, person_id],
        )?;
        Ok(())
    }

    pub fn is_turned_out(&self, space_id: &str, person_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM turned_out WHERE space_id = ? AND person_id = ?",
            params![space_id, person_id],
        )
    }

    pub fn turned_out_of(&self, space_id: &str) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.color FROM people p
             JOIN turned_out t ON t.person_id = p.id
             WHERE t.space_id = ? ORDER BY t.created_at, p.id",
        )?;
        let rows = stmt.query_map([space_id], person_from_row)?;
        rows.collect()
    }

    /// Everyone who is a member of any floor of a space.
    pub fn members_of_space(&self, space_id: &str) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.color FROM people p
             WHERE p.id IN (SELECT m.person_id FROM members m
                            JOIN floors f ON f.hangout_id = m.hangout_id WHERE f.space_id = ?)
             ORDER BY p.created_at, p.id",
        )?;
        let rows = stmt.query_map([space_id], person_from_row)?;
        rows.collect()
    }

    /// Everyone on a public floor of a space who walked in or sent a bot.
    pub fn walk_ins_of_space(&self, space_id: &str, now: i64) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.color FROM people p
             WHERE p.id IN (SELECT v.person_id FROM floor_visits v
                            JOIN floors f ON f.hangout_id = v.hangout_id
                            WHERE f.space_id = ? AND f.access = 'public' AND v.until > ?)
             ORDER BY p.created_at, p.id",
        )?;
        let rows = stmt.query_map(params![space_id, now], person_from_row)?;
        rows.collect()
    }

    /// The public floors this person hasn't been turned out of, the town square's first (L18), then
    /// fullest first (S28). `capacity` is filled in by `spaces`, which knows what the sizes hold.
    pub fn public_floors(
        &self,
        person_id: &str,
        now: i64,
        limit: i64,
    ) -> rusqlite::Result<Vec<PublicFloor>> {
        self.public_floors_with(person_id, now, limit, None)
    }

    /// I114: the public floors, only those with at least one item of kind `has` when it is given.
    /// The filter is in the query, so `limit` counts the floors that match.
    pub fn public_floors_with(
        &self,
        person_id: &str,
        now: i64,
        limit: i64,
        has: Option<&str>,
    ) -> rusqlite::Result<Vec<PublicFloor>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT f.hangout_id, h.name, s.name, p.name, s.about, f.size, {ON_FLOOR} AS here,
                    EXISTS (SELECT 1 FROM space_plots d
                            JOIN world_squares q ON q.world_id = d.world_id AND q.x = d.x AND q.y = d.y
                            JOIN players pl ON pl.id = q.player_id AND pl.person_id = s.owner_id
                            WHERE d.space_id = s.id
                              AND s.id IN (SELECT space_id FROM town_square_rooms)) AS town_square,
                    s.id
             FROM floors f
             JOIN hangouts h ON h.id = f.hangout_id
             JOIN spaces s ON s.id = f.space_id
             JOIN people p ON p.id = s.owner_id
             WHERE f.access = 'public'
               AND NOT EXISTS (SELECT 1 FROM turned_out t WHERE t.space_id = s.id AND t.person_id = ?2)
               AND (?4 IS NULL OR EXISTS (SELECT 1 FROM floor_items i
                                          WHERE i.hangout_id = f.hangout_id AND i.kind = ?4))
             ORDER BY town_square DESC, here DESC, h.created_at DESC, f.hangout_id LIMIT ?3"
        ))?;
        let rows = stmt.query_map(params![now, person_id, limit, has], |row| {
            Ok(PublicFloor {
                id: row.get(0)?,
                name: row.get(1)?,
                space_name: row.get(2)?,
                owner_name: row.get(3)?,
                about: row.get(4)?,
                size: row.get(5)?,
                capacity: 0,
                here: row.get(6)?,
                town_square: row.get(7)?,
                space_id: row.get(8)?,
            })
        })?;
        rows.collect()
    }

    pub fn residents(&self, house_id: &str) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.name, p.color FROM people p
             JOIN residents r ON r.person_id = p.id
             WHERE r.house_id = ? ORDER BY r.joined_at, p.id",
        )?;
        let rows = stmt.query_map([house_id], person_from_row)?;
        rows.collect()
    }

    pub fn is_resident(&self, house_id: &str, person_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM residents WHERE house_id = ? AND person_id = ?",
            params![house_id, person_id],
        )
    }

    pub fn resident_count(&self, house_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM residents WHERE house_id = ?",
            [house_id],
            |row| row.get(0),
        )
    }

    /// Joining a house also joins its city, and both its rooms. Their bots come in separately.
    /// False, with nothing changed, if they live in another house in this city. That's decided here
    /// so two requests at the same moment can't both get past it (R5).
    pub fn add_resident(
        &self,
        house_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        Ok(self.enter_house(house_id, person_id, None, now)? == MoveIn::Moved)
    }

    /// R5: uses up a house's link and moves someone in, as one step, so however many arrive at the
    /// same moment one person gets in and a link is only used up by someone it lets in.
    pub fn move_in(
        &self,
        house_id: &str,
        person_id: &str,
        code: &str,
        new_code: &str,
        now: i64,
    ) -> rusqlite::Result<MoveIn> {
        self.enter_house(house_id, person_id, Some((code, new_code)), now)
    }

    fn enter_house(
        &self,
        house_id: &str,
        person_id: &str,
        link: Option<(&str, &str)>,
        now: i64,
    ) -> rusqlite::Result<MoveIn> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (city_id, living_room, porch): (String, String, String) = tx.query_row(
            "SELECT city_id, living_room, porch FROM houses WHERE id = ?",
            [house_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if lives_elsewhere_in_city(&tx, &city_id, person_id, house_id)? {
            return Ok(MoveIn::LivesElsewhere);
        }
        if let Some((code, new_code)) = link {
            let taken = tx.execute(
                "UPDATE houses SET invite_code = ? WHERE id = ? AND invite_code = ?",
                params![new_code, house_id, code],
            )?;
            if taken == 0 {
                return Ok(MoveIn::LinkGone);
            }
        }
        join_city(&tx, &city_id, person_id, now)?;
        tx.execute(
            "INSERT OR IGNORE INTO residents (house_id, person_id, joined_at) VALUES (?, ?, ?)",
            params![house_id, person_id, now],
        )?;
        for room in [&living_room, &porch] {
            tx.execute(
                "INSERT OR IGNORE INTO members (hangout_id, person_id, joined_at) VALUES (?, ?, ?)",
                params![room, person_id, now],
            )?;
        }
        tx.commit()?;
        Ok(MoveIn::Moved)
    }

    /// R5: whether `first` moved into this house before `second`. A tie on the clock goes by who
    /// came first in the table.
    pub fn moved_in_before(
        &self,
        house_id: &str,
        first: &str,
        second: &str,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM residents a JOIN residents b ON b.house_id = a.house_id
             WHERE a.house_id = ?1 AND a.person_id = ?2 AND b.person_id = ?3
               AND (a.joined_at, a.rowid) < (b.joined_at, b.rowid)",
            params![house_id, first, second],
        )
    }

    /// Grants hold a name, and names never change, so this says whether a grant's giver is here.
    pub fn has_resident_named(&self, house_id: &str, name: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM residents r JOIN people p ON p.id = r.person_id
             WHERE r.house_id = ? AND p.name = ?",
            params![house_id, name],
        )
    }

    /// Takes the person and their bots out of both rooms and forgets where their bots lived in
    /// this house. They stay in the city.
    pub fn remove_resident(&self, house_id: &str, person_id: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (living_room, porch): (String, String) = tx.query_row(
            "SELECT living_room, porch FROM houses WHERE id = ?",
            [house_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        for room in [&living_room, &porch] {
            tx.execute(
                "DELETE FROM room_agents WHERE hangout_id = ?1 AND agent_id IN
                    (SELECT id FROM agents WHERE owner_id = ?2)",
                params![room, person_id],
            )?;
            tx.execute(
                "DELETE FROM members WHERE hangout_id = ? AND person_id = ?",
                params![room, person_id],
            )?;
        }
        // A house bot stays with the house, and a bot that holds keys stays put. Without these
        // rows either would turn into an ordinary bot that can walk into a room with guests.
        tx.execute(
            "DELETE FROM bot_homes WHERE house_id = ?1 AND is_house = 0 AND stays_home = 0
                AND agent_id IN (SELECT id FROM agents WHERE owner_id = ?2)",
            params![house_id, person_id],
        )?;
        // Someone who moved out no longer speaks for the house.
        tx.execute(
            "UPDATE trips SET host_ok_by = NULL, host_ok_id = NULL
             WHERE house_id = ?1 AND host_ok_id = ?2 AND status = 'asking'",
            params![house_id, person_id],
        )?;
        tx.execute(
            "DELETE FROM residents WHERE house_id = ? AND person_id = ?",
            params![house_id, person_id],
        )?;
        tx.commit()
    }

    /// In the order they were built, which is the order the map gives them plots.
    pub fn houses_in_city(&self, city_id: &str) -> rusqlite::Result<Vec<House>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {HOUSE_COLS} FROM houses h WHERE h.city_id = ? ORDER BY h.created_at, h.rowid"
        ))?;
        let rows = stmt.query_map([city_id], house_from_row)?;
        rows.collect()
    }

    /// Who lives where in a city, as (house id, person id), houses in the order they were built.
    pub fn city_residents(&self, city_id: &str) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT r.house_id, r.person_id FROM residents r
             JOIN houses h ON h.id = r.house_id
             WHERE h.city_id = ? ORDER BY h.created_at, h.rowid, r.joined_at, r.person_id",
        )?;
        let rows = stmt.query_map([city_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn house_of_person_in_city(
        &self,
        city_id: &str,
        person_id: &str,
    ) -> rusqlite::Result<Option<House>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {HOUSE_COLS} FROM houses h
                     JOIN residents r ON r.house_id = h.id
                     WHERE h.city_id = ? AND r.person_id = ? ORDER BY h.created_at, h.rowid LIMIT 1"
                ),
                params![city_id, person_id],
                house_from_row,
            )
            .optional()
    }

    pub fn houses_for_person(&self, person_id: &str) -> rusqlite::Result<Vec<House>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {HOUSE_COLS} FROM houses h
             JOIN residents r ON r.house_id = h.id
             WHERE r.person_id = ? ORDER BY r.joined_at, h.id"
        ))?;
        let rows = stmt.query_map([person_id], house_from_row)?;
        rows.collect()
    }

    /// The bots that live in a house, with where each is now. A bot left behind by someone who
    /// moved out isn't one of them, unless it's the house bot, which belongs to the house.
    pub fn bots_of_house(&self, house_id: &str, now: i64) -> rusqlite::Result<Vec<BotPlace>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&bot_places_sql(
            "(SELECT city_id FROM houses WHERE id = ?2)",
            "JOIN bot_homes bh ON bh.agent_id = a.id AND bh.house_id = ?2
                AND (bh.is_house = 1 OR EXISTS (SELECT 1 FROM residents r
                     WHERE r.house_id = ?2 AND r.person_id = a.owner_id))",
        ))?;
        let rows = stmt.query_map(params![now, house_id], |row| bot_place_from_row(row, now))?;
        rows.collect()
    }

    // Bot homes

    pub fn bot_home(&self, agent_id: &str) -> rusqlite::Result<Option<BotHome>> {
        self.conn()
            .query_row(
                "SELECT agent_id, house_id, stays_home, is_house FROM bot_homes WHERE agent_id = ?1
                 UNION ALL SELECT agent_id, '', 1, 0 FROM bots_without_a_house WHERE agent_id = ?1",
                [agent_id],
                |row| {
                    Ok(BotHome {
                        agent_id: row.get(0)?,
                        house_id: row.get(1)?,
                        stays_home: row.get(2)?,
                        is_house: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    /// Gives a bot a home or changes it. `is_house` only counts when the row is first made:
    /// nothing turns an ordinary bot into the house bot, or back.
    pub fn set_bot_home(
        &self,
        agent_id: &str,
        house_id: &str,
        stays_home: bool,
        is_house: bool,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO bot_homes (agent_id, house_id, stays_home, is_house) VALUES (?, ?, ?, ?)
             ON CONFLICT (agent_id) DO UPDATE SET
                house_id = excluded.house_id, stays_home = excluded.stays_home",
            params![agent_id, house_id, stays_home, is_house],
        )?;
        tx.execute("DELETE FROM bots_without_a_house WHERE agent_id = ?", [agent_id])?;
        tx.commit()
    }

    /// For a bot that stays home: it keeps only the one room.
    pub fn remove_agent_from_other_rooms(
        &self,
        agent_id: &str,
        keep_hangout_id: &str,
    ) -> rusqlite::Result<()> {
        let now = crate::now();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
             SELECT agent_id, hangout_id, ?3, 'you stay home'
             FROM room_agents WHERE agent_id = ?1 AND hangout_id != ?2
             ON CONFLICT (who, hangout_id) DO UPDATE SET
                left_at = excluded.left_at, reason = excluded.reason",
            params![agent_id, keep_hangout_id, now],
        )?;
        conn.execute(
            "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
             SELECT ?1, hangout_id, ?2, 'you stay home' FROM floor_visits
             WHERE agent_id = ?1 OR who = ?1
             ON CONFLICT (who, hangout_id) DO UPDATE SET
                left_at = excluded.left_at, reason = excluded.reason",
            params![agent_id, now],
        )?;
        conn.execute(
            "DELETE FROM room_agents WHERE agent_id = ? AND hangout_id != ?",
            params![agent_id, keep_hangout_id],
        )?;
        conn.execute("DELETE FROM floor_visits WHERE agent_id = ?", [agent_id])?;
        Ok(())
    }

    pub fn house_bot(&self, house_id: &str, now: i64) -> rusqlite::Result<Option<Agent>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {AGENT_COLS} FROM {AGENTS}
                     JOIN bot_homes bh ON bh.agent_id = a.id
                     WHERE bh.house_id = ? AND bh.is_house = 1"
                ),
                [house_id],
                |row| agent_from_row(row, now),
            )
            .optional()
    }

    /// Makes the house's bot, in no rooms. Returns false, and makes nothing, if it already has one.
    pub fn create_house_bot(
        &self,
        house_id: &str,
        agent_id: &str,
        owner_id: &str,
        name: &str,
        token_hash: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if exists(&tx, "SELECT 1 FROM bot_homes WHERE house_id = ? AND is_house = 1", [house_id])? {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO agents (id, owner_id, name, token_hash, created_at) VALUES (?, ?, ?, ?, ?)",
            params![agent_id, owner_id, name, token_hash, now],
        )?;
        tx.execute(
            "INSERT INTO bot_homes (agent_id, house_id, stays_home, is_house) VALUES (?, ?, 0, 1)",
            params![agent_id, house_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    // Trips

    pub fn insert_trip(&self, trip: &NewTrip) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO trips (id, agent_id, house_id, kind, goal, minutes, status, asked_by,
                                owner_ok_by, host_ok_by, pass_id, expires_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?, 'asking', ?, ?, ?, ?, ?, ?)",
            params![
                trip.id,
                trip.agent_id,
                trip.house_id,
                trip.kind,
                trip.goal,
                trip.minutes,
                trip.asked_by,
                trip.owner_ok_by,
                trip.host_ok_by,
                trip.pass_id,
                trip.expires_at,
                trip.now
            ],
        )?;
        Ok(())
    }

    pub fn trip(&self, id: &str, now: i64) -> rusqlite::Result<Option<Trip>> {
        self.conn()
            .query_row(&format!("SELECT {TRIP_COLS} FROM {TRIPS} WHERE t.id = ?"), [id], |row| {
                trip_from_row(row, now)
            })
            .optional()
    }

    /// Asks waiting for an answer, trips under way, and anything created since `since`, newest first.
    pub fn trips_for_house(
        &self,
        house_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<Trip>> {
        load_trips(&self.conn(), "t.house_id = ?1", house_id, since, now)
    }

    pub fn trips_for_bot(
        &self,
        agent_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<Trip>> {
        load_trips(&self.conn(), "t.agent_id = ?1", agent_id, since, now)
    }

    /// Trips of all of one person's bots.
    pub fn trips_for_owner(
        &self,
        owner_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<Trip>> {
        load_trips(&self.conn(), "a.owner_id = ?1", owner_id, since, now)
    }

    /// The trip the bot is on right now.
    pub fn live_trip(&self, agent_id: &str, now: i64) -> rusqlite::Result<Option<Trip>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {TRIP_COLS} FROM {TRIPS}
                     WHERE t.agent_id = ?1 AND t.status = 'here' AND t.ends_at > ?2
                     ORDER BY t.arrived_at DESC LIMIT 1"
                ),
                params![agent_id, now],
                |row| trip_from_row(row, now),
            )
            .optional()
    }

    /// A request from this bot to this house that hasn't been answered or lapsed.
    pub fn open_ask(
        &self,
        agent_id: &str,
        house_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<Trip>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {TRIP_COLS} FROM {TRIPS}
                     WHERE t.agent_id = ?1 AND t.house_id = ?2 AND t.status = 'asking' AND t.expires_at > ?3
                       AND NOT EXISTS (SELECT 1 FROM quiet_asks q
                                       WHERE q.kind = 'trip_from_house' AND q.ask_id = t.id)
                     ORDER BY t.created_at DESC LIMIT 1"
                ),
                params![agent_id, house_id, now],
                |row| trip_from_row(row, now),
            )
            .optional()
    }

    /// How many requests one person's bots have open at a house.
    pub fn open_asks_from_owner(
        &self,
        owner_id: &str,
        house_id: &str,
        now: i64,
    ) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM trips t JOIN agents a ON a.id = t.agent_id
             WHERE a.owner_id = ?1 AND t.house_id = ?2 AND t.status = 'asking' AND t.expires_at > ?3
               AND NOT EXISTS (SELECT 1 FROM quiet_asks q
                               WHERE q.kind = 'trip_from_house' AND q.ask_id = t.id)",
            params![owner_id, house_id, now],
            |row| row.get(0),
        )
    }

    /// Records the bot's owner saying yes. Keeps the first yes. False if the ask is no longer open.
    pub fn trip_owner_ok(
        &self,
        trip_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE trips SET owner_ok_by = COALESCE(owner_ok_by, ?2)
             WHERE id = ?1 AND status = 'asking' AND expires_at > ?3",
            params![trip_id, person_id, now],
        )?;
        Ok(changed > 0)
    }

    /// Records a resident of the house saying yes. Keeps the first yes. False if the ask is closed.
    /// The id is kept so `remove_resident` can take the yes back.
    pub fn trip_host_ok(&self, trip_id: &str, person_id: &str, now: i64) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE trips SET host_ok_by = COALESCE(host_ok_by, (SELECT name FROM people WHERE id = ?2)),
                              host_ok_id = COALESCE(host_ok_id, ?2)
             WHERE id = ?1 AND status = 'asking' AND expires_at > ?3",
            params![trip_id, person_id, now],
        )?;
        Ok(changed > 0)
    }

    /// Starts a trip that both sides said yes to (or a pass covers), for its `minutes` from now.
    /// A trip of this bot's that ran out is closed first, so only one that's still under way
    /// stops it. A walk-in doesn't: the bot leaves the floor (S40).
    pub fn arrive(&self, trip_id: &str, now: i64) -> rusqlite::Result<Arrival> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let trip: Option<(String, String, i64, i64, bool)> = tx
            .query_row(
                "SELECT agent_id, status, minutes, expires_at,
                        (owner_ok_by IS NOT NULL AND host_ok_by IS NOT NULL) OR pass_id IS NOT NULL
                 FROM trips WHERE id = ?",
                [trip_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        let Some((agent_id, status, minutes, expires_at, agreed)) = trip else {
            return Ok(Arrival::Closed);
        };
        if status != "asking" || expires_at <= now {
            return Ok(Arrival::Closed);
        }
        if !agreed {
            return Ok(Arrival::Unapproved);
        }
        tx.execute(
            "UPDATE trips SET status = 'ended' WHERE agent_id = ? AND status = 'here' AND ends_at <= ?",
            params![agent_id, now],
        )?;
        let busy = exists(
            &tx,
            "SELECT 1 FROM trips WHERE agent_id = ? AND status = 'here' AND ends_at > ?",
            params![agent_id, now],
        )?;
        if !busy {
            tx.execute(
                "UPDATE trips SET status = 'here', arrived_at = ?2, ends_at = ?3 WHERE id = ?1",
                params![trip_id, now, now + minutes * 60],
            )?;
            tx.execute(
                "INSERT INTO floor_leaves (who, hangout_id, left_at, reason)
                 SELECT ?1, hangout_id, ?2, 'a trip started, so you left this floor'
                 FROM floor_visits WHERE agent_id = ?1 OR who = ?1
                 ON CONFLICT (who, hangout_id) DO UPDATE SET
                    left_at = excluded.left_at, reason = excluded.reason",
                params![agent_id, now],
            )?;
            tx.execute("DELETE FROM floor_visits WHERE agent_id = ?", [&agent_id])?;
            // S56: a trip ends at home, so nothing it goes to on the way counts for afterwards.
            tx.execute("DELETE FROM bot_places WHERE agent_id = ?", [&agent_id])?;
        }
        tx.commit()?;
        Ok(if busy { Arrival::Busy } else { Arrival::Arrived })
    }

    /// Ends an ask with a no. False if it was already answered or lapsed.
    pub fn decline_trip(
        &self,
        trip_id: &str,
        declined_by: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE trips SET status = 'declined', ended_by = ?2
             WHERE id = ?1 AND status = 'asking' AND expires_at > ?3",
            params![trip_id, declined_by, now],
        )?;
        Ok(changed > 0)
    }

    /// Ends a trip that's under way, at once. False if it had already run out or ended.
    pub fn end_trip(&self, trip_id: &str, ended_by: &str, now: i64) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE trips SET status = 'ended', ended_by = ?2
             WHERE id = ?1 AND status = 'here' AND ends_at > ?3",
            params![trip_id, ended_by, now],
        )?;
        Ok(changed > 0)
    }

    /// Ends the bot's trip and withdraws its open asks. Returns how many it changed.
    pub fn end_trips_of_bot(
        &self,
        agent_id: &str,
        ended_by: &str,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let ended = tx.execute(
            "UPDATE trips SET status = 'ended', ended_by = ?2
             WHERE agent_id = ?1 AND status = 'here' AND ends_at > ?3",
            params![agent_id, ended_by, now],
        )?;
        let declined = tx.execute(
            "UPDATE trips SET status = 'declined', ended_by = ?2
             WHERE agent_id = ?1 AND status = 'asking' AND expires_at > ?3",
            params![agent_id, ended_by, now],
        )?;
        tx.commit()?;
        Ok(ended + declined)
    }

    /// Ends the trip a pass is covering, if one is under way.
    pub fn end_trip_for_pass(
        &self,
        pass_id: &str,
        ended_by: &str,
        now: i64,
    ) -> rusqlite::Result<usize> {
        self.conn().execute(
            "UPDATE trips SET status = 'ended', ended_by = ?2
             WHERE pass_id = ?1 AND status = 'here' AND ends_at > ?3",
            params![pass_id, ended_by, now],
        )
    }

    /// R7: trips under way here that this person said yes to, not counting any a pass covers.
    pub fn live_trips_agreed_by(
        &self,
        house_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM trips WHERE house_id = ? AND host_ok_id = ? AND pass_id IS NULL
               AND status = 'here' AND ends_at > ?",
        )?;
        let rows = stmt.query_map(params![house_id, person_id, now], |row| row.get(0))?;
        rows.collect()
    }

    /// Moves the end of a trip that's under way. False if it isn't.
    pub fn set_trip_ends_at(
        &self,
        trip_id: &str,
        ends_at: i64,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE trips SET ends_at = ?2 WHERE id = ?1 AND status = 'here' AND ends_at > ?3",
            params![trip_id, ends_at, now],
        )?;
        Ok(changed > 0)
    }

    /// Whether a resident of the house has said yes to a trip of this bot's that really started.
    /// A standing pass can only be offered to a bot that has.
    pub fn has_hosted_trip(&self, agent_id: &str, house_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM trips WHERE agent_id = ? AND house_id = ?
               AND host_ok_by IS NOT NULL AND arrived_at IS NOT NULL",
            params![agent_id, house_id],
        )
    }

    // Passes

    /// Offers a pass. False, and nothing changes, if the bot already has one for this house.
    pub fn insert_pass(&self, pass: &NewPass) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO passes (id, house_id, agent_id, max_minutes, granted_by, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![pass.id, pass.house_id, pass.agent_id, pass.max_minutes, pass.granted_by, pass.now],
        )?;
        Ok(changed > 0)
    }

    pub fn pass(&self, id: &str) -> rusqlite::Result<Option<Pass>> {
        self.conn()
            .query_row(
                &format!("SELECT {PASS_COLS} FROM {PASSES} WHERE p.id = ?"),
                [id],
                pass_from_row,
            )
            .optional()
    }

    pub fn passes_for_house(&self, house_id: &str) -> rusqlite::Result<Vec<Pass>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PASS_COLS} FROM {PASSES} WHERE p.house_id = ? ORDER BY p.created_at, p.id"
        ))?;
        let rows = stmt.query_map([house_id], pass_from_row)?;
        rows.collect()
    }

    /// Passes offered to any of one person's bots.
    pub fn passes_for_owner(&self, owner_id: &str) -> rusqlite::Result<Vec<Pass>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PASS_COLS} FROM {PASSES} WHERE a.owner_id = ? ORDER BY p.created_at, p.id"
        ))?;
        let rows = stmt.query_map([owner_id], pass_from_row)?;
        rows.collect()
    }

    pub fn pass_for_bot(&self, house_id: &str, agent_id: &str) -> rusqlite::Result<Option<Pass>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {PASS_COLS} FROM {PASSES} WHERE p.house_id = ? AND p.agent_id = ?"
                ),
                params![house_id, agent_id],
                pass_from_row,
            )
            .optional()
    }

    /// False if it was already accepted or is gone.
    pub fn accept_pass(&self, id: &str, person_id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE passes SET accepted_by = ? WHERE id = ? AND accepted_by IS NULL",
            params![person_id, id],
        )?;
        Ok(changed > 0)
    }

    // Keys (H8 to H13)

    /// H8: trips of this bot's to this house that arrived and that no resident ended.
    pub fn good_trips_to(&self, agent_id: &str, house_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM trips t
             WHERE t.agent_id = ? AND t.house_id = ? AND t.arrived_at IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM sent_home s WHERE s.trip_id = t.id)",
            params![agent_id, house_id],
            |row| row.get(0),
        )
    }

    pub fn add_pass_key(
        &self,
        pass_id: &str,
        key: &str,
        granted_by: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO pass_keys (pass_id, key, granted_by, created_at) VALUES (?, ?, ?, ?)",
            params![pass_id, key, granted_by, now],
        )?;
        Ok(())
    }

    pub fn remove_pass_key(&self, pass_id: &str, key: &str) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if key == "scenes" {
            decline_scenes_on_keys(&tx, "p.id = ?", [pass_id])?;
        }
        tx.execute("DELETE FROM pass_keys WHERE pass_id = ? AND key = ?", params![pass_id, key])?;
        tx.commit()
    }

    /// H12: the keys this resident gave at this house. `granted_by` is a name, as on a pass.
    pub fn remove_keys_given_by(&self, house_id: &str, name: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM pass_keys WHERE granted_by = ?
             AND pass_id IN (SELECT id FROM passes WHERE house_id = ?)",
            params![name, house_id],
        )?;
        Ok(())
    }

    /// H11: who gave this bot the key, if its accepted pass to this house has it and they still
    /// live there.
    pub fn key_giver(
        &self,
        agent_id: &str,
        house_id: &str,
        key: &str,
    ) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT k.granted_by FROM passes p JOIN pass_keys k ON k.pass_id = p.id
                 WHERE p.agent_id = ?1 AND p.house_id = ?2 AND p.accepted_by IS NOT NULL AND k.key = ?3
                   AND EXISTS (SELECT 1 FROM residents gr JOIN people gp ON gp.id = gr.person_id
                               WHERE gr.house_id = ?2 AND gp.name = k.granted_by)",
                params![agent_id, house_id, key],
                |row| row.get(0),
            )
            .optional()
    }

    /// H13: the accepted passes of this bot's that have a key, with the house's name.
    pub fn keys_of_bot(&self, agent_id: &str) -> rusqlite::Result<Vec<(Pass, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PASS_COLS}, h.name FROM {PASSES} JOIN houses h ON h.id = p.house_id
             WHERE p.agent_id = ? AND p.accepted_by IS NOT NULL
               AND EXISTS (SELECT 1 FROM pass_keys k WHERE k.pass_id = p.id)
             ORDER BY p.created_at, p.id"
        ))?;
        let rows = stmt.query_map([agent_id], |row| Ok((pass_from_row(row)?, row.get(9)?)))?;
        rows.collect()
    }

    pub fn delete_pass(&self, id: &str) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        decline_scenes_on_keys(&tx, "p.id = ?", [id])?;
        let changed = tx.execute("DELETE FROM passes WHERE id = ?", [id])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    // Scenes

    /// Adds a scene. False if the house already has one with that name.
    pub fn insert_scene(
        &self,
        id: &str,
        house_id: &str,
        name: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO scenes (id, house_id, name, created_at) VALUES (?, ?, ?, ?)",
            params![id, house_id, name, now],
        )?;
        Ok(changed > 0)
    }

    pub fn scene(&self, id: &str) -> rusqlite::Result<Option<Scene>> {
        self.conn()
            .query_row("SELECT id, house_id, name FROM scenes WHERE id = ?", [id], scene_from_row)
            .optional()
    }

    pub fn scenes(&self, house_id: &str) -> rusqlite::Result<Vec<Scene>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, house_id, name FROM scenes WHERE house_id = ? ORDER BY created_at, name",
        )?;
        let rows = stmt.query_map([house_id], scene_from_row)?;
        rows.collect()
    }

    /// Takes its requests with it.
    pub fn delete_scene(&self, id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute("DELETE FROM scenes WHERE id = ?", [id])?;
        Ok(changed > 0)
    }

    /// Asks for a scene. False, and nothing is added, if it already has an open request.
    pub fn insert_scene_request(&self, request: &NewSceneRequest) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if exists(
            &tx,
            "SELECT 1 FROM scene_requests
             WHERE scene_id = ? AND status IN ('waiting', 'approved') AND expires_at > ?",
            params![request.scene_id, request.now],
        )? {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO scene_requests (id, scene_id, house_id, asked_by, status, answered_by,
                                         expires_at, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                request.id,
                request.scene_id,
                request.house_id,
                request.asked_by,
                request.status,
                request.answered_by,
                request.expires_at,
                request.now
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn scene_request(&self, id: &str, now: i64) -> rusqlite::Result<Option<SceneRequest>> {
        self.conn()
            .query_row(
                &format!("SELECT {SCENE_REQUEST_COLS} FROM {SCENE_REQUESTS} WHERE r.id = ?"),
                [id],
                |row| scene_request_from_row(row, now),
            )
            .optional()
    }

    /// The request for this scene that's waiting or approved and hasn't lapsed.
    pub fn open_scene_request(
        &self,
        scene_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<SceneRequest>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {SCENE_REQUEST_COLS} FROM {SCENE_REQUESTS}
                     WHERE r.scene_id = ? AND r.status IN ('waiting', 'approved') AND r.expires_at > ?
                     ORDER BY r.created_at DESC LIMIT 1"
                ),
                params![scene_id, now],
                |row| scene_request_from_row(row, now),
            )
            .optional()
    }

    /// Answers a waiting request with `approved` or `declined`. An approval gets a new
    /// `expires_at`, so it lapses a set time after it was given. False if it was no longer waiting.
    pub fn answer_scene_request(
        &self,
        id: &str,
        status: &str,
        answered_by: &str,
        expires_at: Option<i64>,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE scene_requests
             SET status = ?2, answered_by = ?3, expires_at = COALESCE(?4, expires_at)
             WHERE id = ?1 AND status = 'waiting' AND expires_at > ?5",
            params![id, status, answered_by, expires_at, now],
        )?;
        Ok(changed > 0)
    }

    /// The house bot reporting `done` or `failed` for an approved request. False if it lapsed.
    pub fn finish_scene_request(&self, id: &str, result: &str, now: i64) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE scene_requests SET status = ?2
             WHERE id = ?1 AND status = 'approved' AND expires_at > ?3",
            params![id, result, now],
        )?;
        Ok(changed > 0)
    }

    /// R7: approved scenes this person said yes to that haven't run. `answered_by` is a name.
    pub fn decline_scenes_approved_by(
        &self,
        house_id: &str,
        name: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE scene_requests SET status = 'declined'
             WHERE house_id = ? AND answered_by = ? AND status = 'approved' AND expires_at > ?",
            params![house_id, name, now],
        )?;
        Ok(())
    }

    /// What the house bot may see: approved requests that haven't lapsed, oldest first.
    pub fn scene_queue(&self, house_id: &str, now: i64) -> rusqlite::Result<Vec<QueueItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT r.id, s.name FROM {SCENE_REQUESTS}
             WHERE r.house_id = ? AND r.status = 'approved' AND r.expires_at > ?
             ORDER BY r.created_at, r.id"
        ))?;
        let rows = stmt.query_map(params![house_id, now], |row| {
            Ok(QueueItem { id: row.get(0)?, scene: row.get(1)? })
        })?;
        rows.collect()
    }

    /// Open requests, plus anything created since `since`, newest first.
    pub fn recent_scene_requests(
        &self,
        house_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<SceneRequest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {SCENE_REQUEST_COLS} FROM {SCENE_REQUESTS}
             WHERE r.house_id = ?1
               AND (r.created_at >= ?2 OR (r.status IN ('waiting', 'approved') AND r.expires_at > ?3))
             ORDER BY r.created_at DESC, r.id LIMIT 50"
        ))?;
        let rows =
            stmt.query_map(params![house_id, since, now], |row| scene_request_from_row(row, now))?;
        rows.collect()
    }

    // House status

    pub fn set_house_status(&self, house_id: &str, state: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO house_status (house_id, state, updated_at) VALUES (?, ?, ?)
             ON CONFLICT (house_id) DO UPDATE SET
                state = excluded.state, updated_at = excluded.updated_at",
            params![house_id, state, now],
        )?;
        Ok(())
    }

    pub fn house_status(&self, house_id: &str) -> rusqlite::Result<Option<HouseStatus>> {
        self.conn()
            .query_row(
                "SELECT state, updated_at FROM house_status WHERE house_id = ?",
                [house_id],
                |row| Ok(HouseStatus { state: row.get(0)?, updated_at: row.get(1)? }),
            )
            .optional()
    }

    // Guests. A guest is a bot on a live trip to the house whose Porch this is, a person or bot who
    // walked onto a public floor, or the owner of any of those bots.

    /// The bots visiting a Porch or a public floor, earliest arrival first, each with
    /// `visitor_until` set. Unlike `agents_in` this is for showing guests, never for notifying anyone.
    pub fn visitors_in(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<Agent>> {
        let conn = self.conn();
        let visitor = |row: &Row| -> rusqlite::Result<Agent> {
            let mut agent = agent_from_row(row, now)?;
            agent.visitor_until = row.get(11)?;
            Ok(agent)
        };
        let mut on_trips = conn.prepare(&format!(
            "SELECT {AGENT_COLS}, t.ends_at FROM {AGENTS}
             JOIN trips t ON t.agent_id = a.id
             JOIN houses ho ON ho.id = t.house_id
             WHERE {GUEST_ROOM} AND t.status = 'here' AND t.ends_at > ?2
               AND NOT EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = ?1 AND r.agent_id = a.id)
             ORDER BY t.arrived_at, a.id"
        ))?;
        let mut walked_in = conn.prepare(&format!(
            "SELECT {AGENT_COLS}, v.until FROM {AGENTS}
             JOIN floor_visits v ON v.agent_id = a.id
             JOIN floors f ON f.hangout_id = v.hangout_id
             WHERE v.hangout_id = ?1 AND f.access = 'public' AND v.until > ?2
               AND NOT EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = ?1 AND r.agent_id = a.id)
             ORDER BY v.since, a.id"
        ))?;
        // A room is a house's or a floor, so at most one of these finds anyone.
        let mut visitors = on_trips
            .query_map(params![hangout_id, now], visitor)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        visitors.extend(
            walked_in
                .query_map(params![hangout_id, now], visitor)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
        );
        Ok(visitors)
    }

    /// The people visiting a room, and the owners of the bots visiting it, so the room can name and
    /// colour them. They aren't members, so `people_in` never lists them, and nothing that notifies
    /// people should either.
    pub fn visitor_owners(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT p.id, p.name, p.color FROM people p
             WHERE p.id IN (
                SELECT a.owner_id FROM trips t
                JOIN houses ho ON ho.id = t.house_id
                JOIN agents a ON a.id = t.agent_id
                WHERE {GUEST_ROOM} AND t.status = 'here' AND t.ends_at > ?2
                  AND NOT EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = ?1 AND r.agent_id = a.id))
             OR p.id IN (
                SELECT v.person_id FROM floor_visits v
                JOIN floors f ON f.hangout_id = v.hangout_id
                WHERE v.hangout_id = ?1 AND f.access = 'public' AND v.until > ?2)
             ORDER BY p.created_at, p.id"
        ))?;
        let rows = stmt.query_map(params![hangout_id, now], person_from_row)?;
        rows.collect()
    }

    /// When this bot's visit to the room began and ends. `None` if it isn't a guest there: a bot
    /// that is placed in the room isn't one.
    pub fn bot_visit(
        &self,
        hangout_id: &str,
        agent_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<Visit>> {
        let conn = self.conn();
        let visit = |row: &Row| Ok(Visit { since: row.get(0)?, until: row.get(1)? });
        let trip = conn
            .query_row(
                &format!(
                    "SELECT t.arrived_at, t.ends_at FROM trips t
                     JOIN houses ho ON ho.id = t.house_id
                     WHERE {GUEST_ROOM} AND t.agent_id = ?2 AND t.status = 'here' AND t.ends_at > ?3
                       AND NOT EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = ?1 AND r.agent_id = ?2)
                     ORDER BY t.arrived_at LIMIT 1"
                ),
                params![hangout_id, agent_id, now],
                visit,
            )
            .optional()?;
        if trip.is_some() {
            return Ok(trip);
        }
        conn.query_row(
            "SELECT v.since, v.until FROM floor_visits v
             JOIN floors f ON f.hangout_id = v.hangout_id
             WHERE v.hangout_id = ?1 AND v.agent_id = ?2 AND v.until > ?3 AND f.access = 'public'
               AND NOT EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = ?1 AND r.agent_id = ?2)",
            params![hangout_id, agent_id, now],
            visit,
        )
        .optional()
    }

    /// When a person's visit began and ends: as the owner of a guest, from the earliest arrival of
    /// their bots there to the last of their trips running out, and likewise for walking onto a
    /// public floor, themselves or by sending a bot. `None` if they aren't a guest there, or if
    /// they belong to the room anyway.
    pub fn person_visit(
        &self,
        hangout_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<Option<Visit>> {
        let conn = self.conn();
        let visit = |row: &Row| -> rusqlite::Result<(Option<i64>, Option<i64>)> {
            Ok((row.get(0)?, row.get(1)?))
        };
        let (since, until) = conn.query_row(
            &format!(
            "SELECT MIN(t.arrived_at), MAX(t.ends_at) FROM trips t
             JOIN houses ho ON ho.id = t.house_id
             JOIN agents a ON a.id = t.agent_id
             WHERE {GUEST_ROOM} AND a.owner_id = ?2 AND t.status = 'here' AND t.ends_at > ?3
               AND NOT EXISTS (SELECT 1 FROM room_agents r WHERE r.hangout_id = ?1 AND r.agent_id = a.id)
               AND NOT EXISTS (SELECT 1 FROM members m WHERE m.hangout_id = ?1 AND m.person_id = ?2)"
            ),
            params![hangout_id, person_id, now],
            visit,
        )?;
        if let Some((since, until)) = since.zip(until) {
            return Ok(Some(Visit { since, until }));
        }
        let (since, until) = conn.query_row(
            "SELECT MIN(v.since), MAX(v.until) FROM floor_visits v
             JOIN floors f ON f.hangout_id = v.hangout_id
             WHERE v.hangout_id = ?1 AND v.person_id = ?2 AND v.until > ?3 AND f.access = 'public'
               AND NOT EXISTS (SELECT 1 FROM members m WHERE m.hangout_id = ?1 AND m.person_id = ?2)",
            params![hangout_id, person_id, now],
            visit,
        )?;
        Ok(since.zip(until).map(|(since, until)| Visit { since, until }))
    }

    /// #354: the `limit` messages just before `before_id`, oldest first, leaving out what
    /// `agent_id` said itself (E4), and for a guest only those after it arrived (`since`), as
    /// `messages_since` reads them.
    pub fn messages_before(
        &self,
        hangout_id: &str,
        before_id: i64,
        agent_id: &str,
        since: Option<i64>,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE hangout_id = ? AND id < ? AND created_at > ? AND kind != 'note'
               AND (agent_id IS NULL OR agent_id != ?)
             ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map(
            params![hangout_id, before_id, since.unwrap_or(i64::MIN), agent_id, limit],
            message_from_row,
        )?;
        let mut messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Chat lines written after `since`: `messages` for a guest. Times are whole seconds, so the
    /// second they arrived is left out rather than risk showing a line from before they came.
    pub fn messages_since(
        &self,
        hangout_id: &str,
        since: i64,
        after: i64,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE hangout_id = ? AND created_at > ? AND id > ? AND kind != 'note'
             ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map(params![hangout_id, since, after, limit], message_from_row)?;
        let mut messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    /// Notes shared after `since`: `notes` for a guest, cut off the same way as `messages_since`.
    pub fn notes_since(
        &self,
        hangout_id: &str,
        since: i64,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             WHERE hangout_id = ? AND kind = 'note' AND created_at > ? ORDER BY id DESC LIMIT ?"
        ))?;
        let rows = stmt.query_map(params![hangout_id, since, limit], message_from_row)?;
        rows.collect()
    }

    /// I102: fetches a single message by id.
    pub fn message(&self, id: i64) -> rusqlite::Result<Option<Message>> {
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT {MESSAGE_COLS} FROM messages WHERE id = ?"),
            params![id],
            message_from_row,
        )
        .optional()
    }

    /// I102: the canonical thread root for a message, if it is in a thread.
    pub fn thread_root_for(&self, message_id: i64) -> rusqlite::Result<Option<i64>> {
        self.root_lookups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let conn = self.conn();
        conn.query_row(
            "SELECT root_id FROM message_threads WHERE message_id = ?",
            params![message_id],
            |row| row.get(0),
        )
        .optional()
    }

    /// #610: how many thread-root lookups this database has answered.
    pub fn root_lookups(&self) -> usize {
        self.root_lookups.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// I103: reply count and latest reply timestamp for root messages.
    /// A101: `thread_summaries`, not counting replies by `without`.
    pub fn thread_summaries_without(
        &self,
        root_ids: &[i64],
        without: &std::collections::HashSet<String>,
    ) -> rusqlite::Result<std::collections::HashMap<i64, (i64, i64)>> {
        if without.is_empty() || root_ids.is_empty() {
            return self.thread_summaries(root_ids);
        }
        let conn = self.conn();
        let in_clause = root_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        let marks = vec!["?"; without.len()].join(",");
        let sql = format!(
            "SELECT t.root_id, COUNT(*), MAX(m.created_at)
             FROM message_threads t
             JOIN messages m ON m.id = t.message_id
             WHERE t.root_id IN ({in_clause}) AND t.message_id != t.root_id
               AND (m.person_id IS NULL OR m.person_id NOT IN ({marks}))
             GROUP BY t.root_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(without.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))
        })?;
        rows.collect()
    }

    pub fn thread_summaries(
        &self,
        root_ids: &[i64],
    ) -> rusqlite::Result<std::collections::HashMap<i64, (i64, i64)>> {
        if root_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let conn = self.conn();
        let in_clause = root_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT t.root_id, COUNT(*), MAX(m.created_at)
             FROM message_threads t
             JOIN messages m ON m.id = t.message_id
             WHERE t.root_id IN ({in_clause}) AND t.message_id != t.root_id
             GROUP BY t.root_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (root_id, (count, last_at)) = row?;
            map.insert(root_id, (count, last_at));
        }
        Ok(map)
    }

    /// I104: replies in a thread after `after`, oldest first.
    pub fn thread_replies(
        &self,
        root_id: i64,
        after: i64,
        since: Option<i64>,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             JOIN message_threads t ON t.message_id = messages.id
             WHERE t.root_id = ? AND t.message_id != t.root_id AND messages.id > ?
               AND messages.created_at > ?
             ORDER BY messages.id ASC LIMIT ?"
        ))?;
        let rows = stmt.query_map(
            params![root_id, after, since.unwrap_or(i64::MIN), limit],
            message_from_row,
        )?;
        rows.collect()
    }

    /// #609: the last message in a thread before `before_id` whose mention event was recorded for
    /// this bot, which its context then starts after.
    pub fn last_thread_mention(
        &self,
        root_id: i64,
        before_id: i64,
        agent_id: &str,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(LAST_THREAD_MENTION, params![root_id, before_id, agent_id], |row| row.get(0))
            .optional()
    }

    /// I105: messages in a thread before `before_id`, oldest first (bounded slice for mentions).
    /// #609: never the root, which the event sends as `root`, and only those after `after_id`.
    pub fn thread_messages_before(
        &self,
        root_id: i64,
        before_id: i64,
        after_id: Option<i64>,
        agent_id: &str,
        since: Option<i64>,
        limit: i64,
    ) -> rusqlite::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM messages
             JOIN message_threads t ON t.message_id = messages.id
             WHERE t.root_id = ?1 AND messages.id < ?2 AND messages.created_at > ?3
               AND messages.id <> ?1 AND messages.id > ?6
               AND messages.kind != 'note'
               AND (messages.agent_id IS NULL OR messages.agent_id != ?4)
             ORDER BY messages.id DESC LIMIT ?5"
        ))?;
        let rows = stmt.query_map(
            params![
                root_id,
                before_id,
                since.unwrap_or(i64::MIN),
                agent_id,
                limit,
                after_id.unwrap_or(0)
            ],
            message_from_row,
        )?;
        let mut messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        messages.reverse();
        Ok(messages)
    }

    // Friends and invites

    /// The plan's link, which is `code` if it doesn't have one yet.
    pub fn plan_invite_code(
        &self,
        event_id: &str,
        code: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<String> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO plan_invites (event_id, code, created_by, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (event_id) DO NOTHING",
            params![event_id, code, person_id, now],
        )?;
        conn.query_row("SELECT code FROM plan_invites WHERE event_id = ?", [event_id], |row| {
            row.get(0)
        })
    }

    pub fn replace_plan_invite(
        &self,
        event_id: &str,
        code: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO plan_invites (event_id, code, created_by, created_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (event_id) DO UPDATE SET
                code = excluded.code, created_by = excluded.created_by,
                created_at = excluded.created_at",
            params![event_id, code, person_id, now],
        )?;
        Ok(())
    }

    /// The plan a link opens, and who sent it.
    pub fn plan_invite(&self, code: &str) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT event_id, created_by FROM plan_invites WHERE code = ?",
                [code],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// Whether this person would be turned away as one too many (F18).
    pub fn plan_is_full(
        &self,
        event_id: &str,
        person_id: &str,
        max: i64,
    ) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM plan_invitees WHERE event_id = ?1) >= ?3
               AND NOT EXISTS (SELECT 1 FROM plan_invitees WHERE event_id = ?1 AND person_id = ?2)",
            params![event_id, person_id, max],
            |row| row.get(0),
        )
    }

    pub fn add_plan_invitee(
        &self,
        event_id: &str,
        person_id: &str,
        max: i64,
        now: i64,
    ) -> rusqlite::Result<Invited> {
        let conn = self.conn();
        let invitees: Vec<String> = conn
            .prepare("SELECT person_id FROM plan_invitees WHERE event_id = ?")?
            .query_map([event_id], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if invitees.iter().any(|invitee| invitee == person_id) {
            return Ok(Invited::Already);
        }
        if invitees.len() as i64 >= max {
            return Ok(Invited::Full);
        }
        conn.execute(
            "INSERT INTO plan_invitees (event_id, person_id, joined_at) VALUES (?, ?, ?)",
            params![event_id, person_id, now],
        )?;
        Ok(Invited::New)
    }

    /// F57: notes that `by` invited `person_id` to this plan by name. False when someone already
    /// had, so a friend is told once per plan however often they're invited.
    pub fn note_invited_by(
        &self,
        event_id: &str,
        person_id: &str,
        by: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let added = self.conn().execute(
            "INSERT OR IGNORE INTO plan_invited_by (event_id, person_id, invited_by, invited_at)
             VALUES (?, ?, ?, ?)",
            params![event_id, person_id, by, now],
        )?;
        Ok(added > 0)
    }

    /// F57: how many invitees a plan has, by link and by name.
    pub fn plan_invitee_count(&self, event_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM plan_invitees WHERE event_id = ?",
            [event_id],
            |row| row.get(0),
        )
    }

    /// F59: whether this person is an invitee of this plan, by link or by name.
    pub fn is_plan_invitee(&self, event_id: &str, person_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM plan_invitees WHERE event_id = ?1 AND person_id = ?2
               AND NOT EXISTS (SELECT 1 FROM quiet_asks q
                               WHERE q.kind = 'plan_invite' AND q.ask_id = ?1 || '|' || ?2)",
            params![event_id, person_id],
        )
    }

    /// F58: the plans this person was invited to by name and hasn't answered, still coming up and
    /// not canceled, soonest first, each with who invited them.
    pub fn invited_plans(
        &self,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<(Event, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {EVENT_COLS}, p.name FROM plan_invited_by b
             JOIN events e ON e.id = b.event_id
             JOIN people p ON p.id = b.invited_by
             JOIN plan_invitees i ON i.event_id = b.event_id AND i.person_id = b.person_id
             WHERE b.person_id = ?1 AND e.status != 'cancelled' AND e.ends_at >= ?2
               AND NOT EXISTS (SELECT 1 FROM rsvps r WHERE r.event_id = e.id AND r.person_id = ?1)
               AND NOT EXISTS (SELECT 1 FROM quiet_asks q
                               WHERE q.kind = 'plan_invite' AND q.ask_id = e.id || '|' || ?1)
             ORDER BY e.starts_at, e.id"
        ))?;
        let rows = stmt.query_map(params![person_id, now], |row| {
            Ok((event_from_row(row)?, row.get::<_, String>(14)?))
        })?;
        rows.collect()
    }

    /// The invitees of a room's plans that are still coming up, first to answer first.
    pub fn invitees_in(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<Invitee>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT i.event_id, p.id, p.name, p.color FROM plan_invitees i
             JOIN events e ON e.id = i.event_id
             JOIN people p ON p.id = i.person_id
             WHERE e.hangout_id = ? AND e.ends_at >= ? AND e.status != 'cancelled'
             ORDER BY i.joined_at, p.id",
        )?;
        let rows = stmt.query_map(params![hangout_id, now], |row| {
            Ok(Invitee {
                event_id: row.get(0)?,
                person_id: row.get(1)?,
                name: row.get(2)?,
                color: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    // Closeness

    /// Who can see when this person is free: `house`, `city`, or nobody.
    pub fn free_sharing(&self, person_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row("SELECT level FROM free_sharing WHERE person_id = ?", [person_id], |row| {
                row.get(0)
            })
            .optional()
    }

    /// F26, F28: no level turns sharing off and deletes what was posted.
    pub fn set_free_sharing(
        &self,
        person_id: &str,
        level: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        match level {
            Some(level) => {
                tx.execute(
                    "INSERT INTO free_sharing (person_id, level, updated_at) VALUES (?, ?, ?)
                     ON CONFLICT (person_id) DO UPDATE SET
                        level = excluded.level, updated_at = excluded.updated_at",
                    params![person_id, level, now],
                )?;
            }
            None => {
                tx.execute("DELETE FROM free_sharing WHERE person_id = ?", [person_id])?;
                tx.execute("DELETE FROM free_windows WHERE person_id = ?", [person_id])?;
            }
        }
        tx.commit()
    }

    pub fn replace_free_windows(
        &self,
        person_id: &str,
        windows: &[(i64, i64)],
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM free_windows WHERE person_id = ?", [person_id])?;
        for (start, end) in windows {
            tx.execute(
                "INSERT INTO free_windows (person_id, starts_at, ends_at, set_at) VALUES (?, ?, ?, ?)",
                params![person_id, start, end, now],
            )?;
        }
        tx.commit()
    }

    /// Windows still ahead that were posted since `posted_since`, in order.
    pub fn free_windows(
        &self,
        person_id: &str,
        now: i64,
        posted_since: i64,
    ) -> rusqlite::Result<Vec<(i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT starts_at, ends_at FROM free_windows
             WHERE person_id = ? AND ends_at > ? AND set_at >= ? ORDER BY starts_at",
        )?;
        let rows = stmt.query_map(params![person_id, now, posted_since], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        rows.collect()
    }

    /// F32: when this person is taken: plans on the calendar that they said yes to.
    pub fn busy_spans(&self, person_id: &str, now: i64) -> rusqlite::Result<Vec<(i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.starts_at, e.ends_at FROM events e
             JOIN rsvps r ON r.event_id = e.id AND r.person_id = ?
             WHERE r.response = 'yes' AND e.status = 'confirmed' AND e.ends_at > ?",
        )?;
        let rows = stmt.query_map(params![person_id, now], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    pub fn share_a_house(&self, one: &str, other: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM residents a JOIN residents b ON a.house_id = b.house_id
             WHERE a.person_id = ? AND b.person_id = ?",
            params![one, other],
        )
    }

    pub fn plan_items(&self, event_id: &str) -> rusqlite::Result<Vec<PlanItem>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PLAN_ITEM_COLS} {PLAN_ITEM_FROM} WHERE i.event_id = ? ORDER BY i.created_at, i.rowid"
        ))?;
        let rows = stmt.query_map([event_id], plan_item_from_row)?;
        rows.collect()
    }

    pub fn plan_item(&self, id: &str) -> rusqlite::Result<Option<PlanItem>> {
        self.conn()
            .query_row(
                &format!("SELECT {PLAN_ITEM_COLS} {PLAN_ITEM_FROM} WHERE i.id = ?"),
                [id],
                plan_item_from_row,
            )
            .optional()
    }

    /// False if the plan's list is full.
    pub fn add_plan_item(&self, item: &NewPlanItem, max: i64) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM plan_items WHERE event_id = ?",
            [item.event_id],
            |row| row.get(0),
        )?;
        if count >= max {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO plan_items (id, event_id, body, added_by, adder, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![item.id, item.event_id, item.text, item.added_by, item.adder, item.now],
        )?;
        Ok(true)
    }

    /// F38, F40: false if someone who's still going already has it.
    pub fn claim_plan_item(
        &self,
        id: &str,
        person_id: &str,
        via: Option<&str>,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE plan_items SET claimed_by = ?2, claimed_via = ?3, done = 0
             WHERE id = ?1 AND (claimed_by IS NULL OR NOT EXISTS (
                SELECT 1 FROM rsvps r
                JOIN events e ON e.id = r.event_id
                JOIN members m ON m.hangout_id = e.hangout_id AND m.person_id = r.person_id
                WHERE r.event_id = plan_items.event_id AND r.person_id = plan_items.claimed_by
                  AND r.response IN ('yes', 'maybe')))",
            params![id, person_id, via],
        )?;
        Ok(changed == 1)
    }

    pub fn release_plan_item(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE plan_items SET claimed_by = NULL, claimed_via = NULL, done = 0 WHERE id = ?",
            [id],
        )?;
        Ok(())
    }

    /// F39: what a bot claimed is its owner's own from now on.
    pub fn keep_plan_item(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("UPDATE plan_items SET claimed_via = NULL WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn finish_plan_item(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("UPDATE plan_items SET done = 1 WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn remove_plan_item(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM plan_items WHERE id = ?", [id])?;
        Ok(())
    }

    // A house's notes and calendar

    pub fn house_notes(&self, house_id: &str) -> rusqlite::Result<Vec<HouseNote>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT n.topic, n.body, p.name, n.via_agent, n.updated_at FROM house_notes n
             JOIN people p ON p.id = n.written_by
             WHERE n.house_id = ? ORDER BY n.topic",
        )?;
        let rows = stmt.query_map([house_id], |row| {
            Ok(HouseNote {
                topic: row.get(0)?,
                text: row.get(1)?,
                writer: row.get(2)?,
                via_agent: row.get(3)?,
                updated_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// Writes under a topic, replacing what's there. The lock is held throughout, so two writers
    /// can't both pass these checks.
    pub fn set_house_note(&self, note: &NewHouseNote, max: i64) -> rusqlite::Result<NoteWrite> {
        let conn = self.conn();
        let existing: Option<Option<String>> = conn
            .query_row(
                "SELECT via_agent FROM house_notes WHERE house_id = ? AND topic = ?",
                params![note.house_id, note.topic],
                |row| row.get(0),
            )
            .optional()?;
        match existing {
            Some(None) if note.via_agent.is_some() => return Ok(NoteWrite::PersonsWord),
            Some(_) => {}
            None => {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM house_notes WHERE house_id = ?",
                    [note.house_id],
                    |row| row.get(0),
                )?;
                if count >= max {
                    return Ok(NoteWrite::Full);
                }
            }
        }
        conn.execute(
            "INSERT INTO house_notes (id, house_id, topic, body, written_by, via_agent, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (house_id, topic) DO UPDATE SET
                topic = excluded.topic, body = excluded.body, written_by = excluded.written_by,
                via_agent = excluded.via_agent, updated_at = excluded.updated_at",
            params![
                note.id,
                note.house_id,
                note.topic,
                note.text,
                note.written_by,
                note.via_agent,
                note.now
            ],
        )?;
        Ok(NoteWrite::Saved)
    }

    /// Whether a bot wrote this note, if there is one.
    pub fn house_note_by_bot(&self, house_id: &str, topic: &str) -> rusqlite::Result<Option<bool>> {
        self.conn()
            .query_row(
                "SELECT via_agent IS NOT NULL FROM house_notes WHERE house_id = ? AND topic = ?",
                params![house_id, topic],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn remove_house_note(&self, house_id: &str, topic: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM house_notes WHERE house_id = ? AND topic = ?",
            params![house_id, topic],
        )?;
        Ok(())
    }

    /// W75: whether this person left god mode on. `god_mode::is_on` says whether that counts.
    pub fn god_mode_on(&self, person_id: &str) -> rusqlite::Result<bool> {
        exists(&self.conn(), "SELECT 1 FROM god_mode WHERE person_id = ?", [person_id])
    }

    pub fn set_god_mode(&self, person_id: &str, on: bool, now: i64) -> rusqlite::Result<()> {
        if on {
            self.conn().execute(
                "INSERT INTO god_mode (person_id, since) VALUES (?, ?)
                 ON CONFLICT (person_id) DO NOTHING",
                params![person_id, now],
            )?;
        } else {
            self.conn().execute("DELETE FROM god_mode WHERE person_id = ?", [person_id])?;
        }
        Ok(())
    }

    /// A95: whether bot sign-ups are open, who last set them and when. None means never set, so open.
    pub fn bot_signups(&self) -> rusqlite::Result<Option<(bool, String, i64)>> {
        self.conn()
            .query_row("SELECT open, set_by, set_at FROM bot_signups WHERE id = 1", [], |row| {
                Ok((row.get::<_, i64>(0)? != 0, row.get(1)?, row.get(2)?))
            })
            .optional()
    }

    /// A96: makes an account through the agents' door, in one step with the checks that may refuse
    /// it: the switch (A95) must be open, and fewer than `per_day` such accounts made in the last day.
    /// Err(Some(reason)) is a refusal, with nothing written.
    pub fn create_agent_account(
        &self,
        id: &str,
        name: &str,
        key_hash: &str,
        feed_token: &str,
        per_day: i64,
        now: i64,
    ) -> rusqlite::Result<Result<Person, &'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let open: Option<i64> = tx
            .query_row("SELECT open FROM bot_signups WHERE id = 1", [], |row| row.get(0))
            .optional()?;
        if open == Some(0) {
            return Ok(Err("closed"));
        }
        let today: i64 = tx.query_row(
            "SELECT COUNT(*) FROM agent_accounts WHERE created_at > ?",
            [now - 24 * 60 * 60],
            |row| row.get(0),
        )?;
        if today >= per_day {
            return Ok(Err("full"));
        }
        let person = insert_person(&tx, id, name, key_hash, feed_token, now)?;
        tx.execute(
            "INSERT INTO agent_accounts (person_id, created_at) VALUES (?, ?)",
            params![id, now],
        )?;
        tx.commit()?;
        Ok(Ok(person))
    }

    /// MY1: the open election, made with `closes_at` if there's none. Two callers at once make one.
    pub fn open_mayor_election(&self, closes_at: i64, now: i64) -> rusqlite::Result<MayorElection> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO mayor_elections (closes_at, opened_at)
             SELECT ?1, ?2 WHERE NOT EXISTS (SELECT 1 FROM mayor_elections WHERE closed_at IS NULL)
             ON CONFLICT (closes_at) DO NOTHING",
            params![closes_at, now],
        )?;
        conn.query_row(
            &format!(
                "SELECT {MAYOR_COLS} FROM mayor_elections WHERE closed_at IS NULL
                      ORDER BY closes_at LIMIT 1"
            ),
            [],
            mayor_election_from_row,
        )
    }

    /// MY6: the last election that closed, for this week's mayor.
    pub fn last_mayor_election(&self) -> rusqlite::Result<Option<MayorElection>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {MAYOR_COLS} FROM mayor_elections WHERE closed_at IS NOT NULL
                          ORDER BY closes_at DESC LIMIT 1"
                ),
                [],
                mayor_election_from_row,
            )
            .optional()
    }

    /// MY2: who stands in this election, earliest first.
    pub fn mayor_candidates(&self, election_id: i64) -> rusqlite::Result<Vec<MayorCandidate>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, candidate_id, name FROM mayor_candidates WHERE election_id = ?
             ORDER BY stood_at, name",
        )?;
        let rows = stmt.query_map([election_id], |row| {
            Ok(MayorCandidate { kind: row.get(0)?, id: row.get(1)?, name: row.get(2)? })
        })?;
        rows.collect()
    }

    /// MY2: stands someone, if there are fewer than `most`. False if it's full; standing again
    /// changes nothing.
    pub fn stand_for_mayor(
        &self,
        election_id: i64,
        candidate: &MayorCandidate,
        most: i64,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let changed = conn.execute(
            "INSERT INTO mayor_candidates (election_id, kind, candidate_id, name, stood_at)
             SELECT ?1, ?2, ?3, ?4, ?5
             WHERE (SELECT COUNT(*) FROM mayor_candidates WHERE election_id = ?1) < ?6
             ON CONFLICT DO NOTHING",
            params![election_id, candidate.kind, candidate.id, candidate.name, now, most],
        )?;
        Ok(changed == 1
            || exists(
                &conn,
                "SELECT 1 FROM mayor_candidates WHERE election_id = ? AND kind = ? AND candidate_id = ?",
                params![election_id, candidate.kind, candidate.id],
            )?)
    }

    /// MY2: takes a candidacy back, with the ballots cast for it.
    pub fn withdraw_from_mayor(
        &self,
        election_id: i64,
        kind: &str,
        candidate_id: &str,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let gone = tx.execute(
            "DELETE FROM mayor_candidates WHERE election_id = ? AND kind = ? AND candidate_id = ?",
            params![election_id, kind, candidate_id],
        )?;
        tx.execute(
            "DELETE FROM mayor_ballots WHERE election_id = ? AND kind = ? AND candidate_id = ?",
            params![election_id, kind, candidate_id],
        )?;
        tx.commit()?;
        Ok(gone == 1)
    }

    /// MY3: casts or changes a person's ballot, for a candidate who stands.
    pub fn cast_mayor_ballot(
        &self,
        election_id: i64,
        voter: &str,
        kind: &str,
        candidate_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT INTO mayor_ballots (election_id, voter, kind, candidate_id, cast_at)
             SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (
                 SELECT 1 FROM mayor_candidates WHERE election_id = ?1 AND kind = ?3
                     AND candidate_id = ?4)
             ON CONFLICT (election_id, voter) DO UPDATE SET kind = excluded.kind,
                 candidate_id = excluded.candidate_id, cast_at = excluded.cast_at",
            params![election_id, voter, kind, candidate_id, now],
        )?;
        Ok(changed == 1)
    }

    /// MY4: how many have voted, and this person's own ballot.
    pub fn mayor_ballots_for(
        &self,
        election_id: i64,
        voter: &str,
    ) -> rusqlite::Result<(i64, Option<(String, String)>)> {
        let conn = self.conn();
        let voted = conn.query_row(
            "SELECT COUNT(*) FROM mayor_ballots WHERE election_id = ?",
            [election_id],
            |row| row.get(0),
        )?;
        let mine = conn
            .query_row(
                "SELECT kind, candidate_id FROM mayor_ballots WHERE election_id = ? AND voter = ?",
                params![election_id, voter],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok((voted, mine))
    }

    /// MY4, MY5: the totals for each candidate, most first.
    pub fn mayor_totals(&self, election_id: i64) -> rusqlite::Result<Vec<(MayorCandidate, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT c.kind, c.candidate_id, c.name,
                    (SELECT COUNT(*) FROM mayor_ballots b WHERE b.election_id = c.election_id
                       AND b.kind = c.kind AND b.candidate_id = c.candidate_id) AS votes
             FROM mayor_candidates c WHERE c.election_id = ? ORDER BY votes DESC, c.stood_at",
        )?;
        let rows = stmt.query_map([election_id], |row| {
            Ok((
                MayorCandidate { kind: row.get(0)?, id: row.get(1)?, name: row.get(2)? },
                row.get(3)?,
            ))
        })?;
        rows.collect()
    }

    /// MY5: counts an election and closes it with its winner, in one step, so no ballot lands
    /// between the count and the close. `draw` picks among `n` tied. It's dated its own
    /// `closes_at`, however late the ticker gets to it. False if it was closed already.
    pub fn tally_and_close_mayor_election(
        &self,
        election_id: i64,
        draw: impl FnOnce(usize) -> usize,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let totals: Vec<(String, String, String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT c.kind, c.candidate_id, c.name,
                        (SELECT COUNT(*) FROM mayor_ballots b WHERE b.election_id = c.election_id
                           AND b.kind = c.kind AND b.candidate_id = c.candidate_id) AS votes
                 FROM mayor_candidates c WHERE c.election_id = ? ORDER BY votes DESC, c.stood_at",
            )?;
            let rows = stmt.query_map([election_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let top = totals.first().map(|row| row.3).unwrap_or(0);
        let tied: Vec<&(String, String, String, i64)> =
            totals.iter().filter(|row| top > 0 && row.3 == top).collect();
        let winner = match tied.len() {
            0 => None,
            1 => Some(tied[0]),
            n => Some(tied[draw(n).min(n - 1)]),
        };
        let changed = tx.execute(
            "UPDATE mayor_elections SET closed_at = closes_at, winner_kind = ?2, winner_id = ?3,
                 winner_name = ?4, winner_votes = ?5, tie_draw = ?6
             WHERE id = ?1 AND closed_at IS NULL",
            params![
                election_id,
                winner.map(|w| w.0.clone()),
                winner.map(|w| w.1.clone()),
                winner.map(|w| w.2.clone()),
                winner.map(|w| w.3),
                (tied.len() > 1) as i64,
            ],
        )?;
        tx.commit()?;
        Ok(changed == 1)
    }

    /// Moves when an account was made, for tests of rules that wait on an account's age (MY3).
    pub fn backdate_person(&self, person_id: &str, created_at: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE people SET created_at = ? WHERE id = ?",
            params![created_at, person_id],
        )?;
        Ok(())
    }

    /// MY7: the Town Square rooms' floors with no mayor's voting box yet, and their space's owner.
    pub fn town_squares_without_mayor_box(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT f.hangout_id, s.owner_id FROM town_square_rooms t
             JOIN spaces s ON s.id = t.space_id
             JOIN floors f ON f.space_id = s.id AND f.level = 0
             WHERE NOT EXISTS (SELECT 1 FROM floor_items i
                               WHERE i.hangout_id = f.hangout_id AND i.kind = 'mayor_ballot_box')",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// MY7: puts the mayor's voting box in a Town Square room.
    pub fn put_mayor_box(
        &self,
        hangout_id: &str,
        owner_id: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO floor_items (id, hangout_id, kind, x, y, facing, placed_by, placed_at)
             SELECT lower(hex(randomblob(8))), ?1, 'mayor_ballot_box', 1, 1, 0, ?2, ?3
             WHERE NOT EXISTS (SELECT 1 FROM floor_items
                               WHERE hangout_id = ?1 AND kind = 'mayor_ballot_box')",
            params![hangout_id, owner_id, now],
        )?;
        Ok(())
    }

    /// A96: how many accounts agents made through the door in the last day.
    pub fn agent_accounts_today(&self, now: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM agent_accounts WHERE created_at > ?",
            [now - 24 * 60 * 60],
            |row| row.get(0),
        )
    }

    /// A96: marks an account as run by an agent, as the door does. For tests and imports; the door
    /// itself marks it in the step that makes it.
    pub fn mark_agent_account(&self, person_id: &str, at: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO agent_accounts (person_id, created_at) VALUES (?, ?)",
            params![person_id, at],
        )?;
        Ok(())
    }

    /// A96: whether this account was made by an agent through the door.
    pub fn is_agent_account(&self, person_id: &str) -> rusqlite::Result<bool> {
        exists(&self.conn(), "SELECT 1 FROM agent_accounts WHERE person_id = ?", [person_id])
    }

    /// A95: opens or closes bot sign-ups, saying who did it and when.
    pub fn set_bot_signups(&self, open: bool, by: &str, at: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO bot_signups (id, open, set_by, set_at) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT (id) DO UPDATE SET open = ?1, set_by = ?2, set_at = ?3",
            params![open as i64, by, at],
        )?;
        Ok(())
    }

    /// F51: whether this person's plans show on their house's calendar.
    pub fn shows_plans_to_house(&self, person_id: &str) -> rusqlite::Result<bool> {
        exists(&self.conn(), "SELECT 1 FROM house_plan_sharing WHERE person_id = ?", [person_id])
    }

    pub fn set_shows_plans_to_house(
        &self,
        person_id: &str,
        on: bool,
        now: i64,
    ) -> rusqlite::Result<()> {
        if on {
            self.conn().execute(
                "INSERT INTO house_plan_sharing (person_id, updated_at) VALUES (?, ?)
                 ON CONFLICT (person_id) DO UPDATE SET updated_at = excluded.updated_at",
                params![person_id, now],
            )?;
        } else {
            self.conn()
                .execute("DELETE FROM house_plan_sharing WHERE person_id = ?", [person_id])?;
        }
        Ok(())
    }

    /// F52 to F54: the house's calendar between two times, soonest first.
    pub fn house_calendar(
        &self,
        house_id: &str,
        from: i64,
        to: i64,
    ) -> rusqlite::Result<Vec<HousePlan>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.id, e.title, e.location, e.starts_at, e.ends_at, e.status, p.name
             FROM houses h
             JOIN residents res ON res.house_id = h.id
             JOIN people p ON p.id = res.person_id
             JOIN rsvps r ON r.person_id = res.person_id AND r.response IN ('yes', 'maybe')
             JOIN events e ON e.id = r.event_id
             WHERE h.id = ?1 AND e.status != 'cancelled' AND e.ends_at >= ?2 AND e.starts_at <= ?3
               AND (e.hangout_id = h.living_room
                    OR EXISTS (SELECT 1 FROM house_plan_sharing s WHERE s.person_id = res.person_id))
             ORDER BY e.starts_at, e.id, p.name, p.id",
        )?;
        let mut rows = stmt.query(params![house_id, from, to])?;
        let mut plans: Vec<(String, HousePlan)> = Vec::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let name: String = row.get(6)?;
            match plans.last_mut() {
                Some((last, plan)) if *last == id => plan.going.push(name),
                _ => {
                    let status: String = row.get(5)?;
                    plans.push((
                        id,
                        HousePlan {
                            title: row.get(1)?,
                            location: row.get(2)?,
                            starts_at: row.get(3)?,
                            ends_at: row.get(4)?,
                            on_calendar: status == "confirmed",
                            going: vec![name],
                        },
                    ));
                }
            }
        }
        Ok(plans.into_iter().map(|(_, plan)| plan).collect())
    }

    // Growth. Counted from what happened, on read. Nothing is stored.

    /// Confirmed Town Square plans that have finished, with a yes from at least two people. A
    /// plan one person confirmed alone isn't friends getting together.
    pub fn gatherings(&self, city_id: &str, now: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            &format!("SELECT COUNT(*) {GATHERINGS}"),
            params![city_id, now],
            |row| row.get(0),
        )
    }

    /// A road for each pair of houses in the city that a bot from one has really arrived at the
    /// other, with how many times. `a` sorts before `b`, and the roads come in that order.
    pub fn roads(&self, city_id: &str) -> rusqlite::Result<Vec<Road>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT MIN(t.house_id, bh.house_id) AS road_a, MAX(t.house_id, bh.house_id) AS road_b,
                    COUNT(*)
             FROM trips t
             JOIN bot_homes bh ON bh.agent_id = t.agent_id AND bh.house_id != t.house_id
             JOIN houses th ON th.id = t.house_id
             JOIN houses hh ON hh.id = bh.house_id
             WHERE t.arrived_at IS NOT NULL AND th.city_id = ?1 AND hh.city_id = ?1
             GROUP BY road_a, road_b ORDER BY road_a, road_b",
        )?;
        let rows = stmt.query_map([city_id], |row| {
            Ok(Road { a: row.get(0)?, b: row.get(1)?, trips: row.get(2)? })
        })?;
        rows.collect()
    }

    // Trust. Standing is counted on read, so all that is stored is what people chose: the rules,
    // the vouches, who is waiting at a gate, and which trips a resident ended.

    /// What a city or a house asks for. A place that never set anything asks for nothing.
    pub fn entry_rules(&self, place_id: &str) -> rusqlite::Result<EntryRules> {
        Ok(self
            .conn()
            .query_row(
                "SELECT min_member_days, min_gatherings, min_good_trips, min_vouches, rules_text
                 FROM entry_rules WHERE place_id = ?",
                [place_id],
                |row| entry_rules_at(row, 0),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// What each house in a city asks for, as (house id, rules). Houses that never set anything
    /// are left out.
    pub fn house_rules_in_city(
        &self,
        city_id: &str,
    ) -> rusqlite::Result<Vec<(String, EntryRules)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.place_id, e.min_member_days, e.min_gatherings, e.min_good_trips,
                    e.min_vouches, e.rules_text
             FROM entry_rules e JOIN houses h ON h.id = e.place_id WHERE h.city_id = ?",
        )?;
        let rows = stmt.query_map([city_id], |row| Ok((row.get(0)?, entry_rules_at(row, 1)?)))?;
        rows.collect()
    }

    pub fn set_entry_rules(
        &self,
        place_id: &str,
        rules: &EntryRules,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO entry_rules (place_id, min_member_days, min_gatherings, min_good_trips,
                                      min_vouches, rules_text, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (place_id) DO UPDATE SET
                min_member_days = excluded.min_member_days,
                min_gatherings = excluded.min_gatherings,
                min_good_trips = excluded.min_good_trips,
                min_vouches = excluded.min_vouches,
                rules_text = excluded.rules_text,
                updated_at = excluded.updated_at",
            params![
                place_id,
                rules.min_member_days,
                rules.min_gatherings,
                rules.min_good_trips,
                rules.min_vouches,
                rules.rules_text,
                now
            ],
        )?;
        Ok(())
    }

    /// Records a vouch. False if this person had already vouched, which changes nothing.
    pub fn add_vouch(
        &self,
        city_id: &str,
        for_person: &str,
        by_person: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT OR IGNORE INTO vouches (city_id, for_person, by_person, created_at)
             VALUES (?, ?, ?, ?)",
            params![city_id, for_person, by_person, now],
        )?;
        Ok(changed > 0)
    }

    /// Takes back the caller's own vouch. False if there wasn't one.
    pub fn remove_vouch(
        &self,
        city_id: &str,
        for_person: &str,
        by_person: &str,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "DELETE FROM vouches WHERE city_id = ? AND for_person = ? AND by_person = ?",
            params![city_id, for_person, by_person],
        )?;
        Ok(changed > 0)
    }

    /// How many people in the city vouch for this person. Those who left don't count.
    pub fn vouch_count(&self, city_id: &str, for_person: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            &format!("SELECT COUNT(*) {VOUCHERS_IN_CITY} WHERE v.city_id = ? AND v.for_person = ?"),
            params![city_id, for_person],
            |row| row.get(0),
        )
    }

    /// What this person has done in the city: how long they've been in it, the gatherings they
    /// said yes to, their bots' trips here that went well, the ones a resident ended, and who
    /// vouches for them. All of it is counted now, from what's in the database.
    pub fn standing_counts(
        &self,
        city_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<StandingCounts> {
        let conn = self.conn();
        let member_since = conn
            .query_row(
                "SELECT joined_at FROM city_members WHERE city_id = ? AND person_id = ?",
                params![city_id, person_id],
                |row| row.get(0),
            )
            .optional()?;
        let gatherings = conn.query_row(
            &format!(
                "SELECT COUNT(*) {GATHERINGS}
                   AND EXISTS (SELECT 1 FROM rsvps y WHERE y.event_id = e.id
                               AND y.person_id = ?3 AND y.response = 'yes')"
            ),
            params![city_id, now, person_id],
            |row| row.get(0),
        )?;
        // A trip counts once it arrived and is over, unless a resident ended it by sending the bot
        // home. One still under way hasn't gone well or badly yet. A house counts once a day, or a
        // bot with a pass could loop short trips to meet another house's requirements.
        let good_trips = conn.query_row(
            "SELECT COUNT(DISTINCT t.house_id || '/' || (t.arrived_at / 86400)) FROM trips t
             JOIN agents a ON a.id = t.agent_id
             JOIN houses h ON h.id = t.house_id
             WHERE a.owner_id = ?1 AND h.city_id = ?2 AND t.arrived_at IS NOT NULL
               AND (t.status = 'ended' OR (t.status = 'here' AND t.ends_at <= ?3))
               AND NOT EXISTS (SELECT 1 FROM sent_home s WHERE s.trip_id = t.id)",
            params![person_id, city_id, now],
            |row| row.get(0),
        )?;
        let sent_home = conn.query_row(
            "SELECT COUNT(*) FROM sent_home s
             JOIN trips t ON t.id = s.trip_id
             JOIN agents a ON a.id = t.agent_id
             JOIN houses h ON h.id = t.house_id
             WHERE a.owner_id = ?1 AND h.city_id = ?2",
            params![person_id, city_id],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare(&format!(
            "SELECT p.name {VOUCHERS_IN_CITY} JOIN people p ON p.id = v.by_person
             WHERE v.city_id = ? AND v.for_person = ? ORDER BY v.created_at, v.rowid"
        ))?;
        let vouchers = stmt
            .query_map(params![city_id, person_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(StandingCounts { member_since, gatherings, good_trips, sent_home, vouchers })
    }

    /// What could pay this person credits in one city (HC2 to HC4), earliest first. Nothing here
    /// is stored: it's read from plans and trips each time (HC1).
    pub fn credit_sources(
        &self,
        city_id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<CreditSource>> {
        let conn = self.conn();
        let mut sources = Vec::new();
        // HC2: a gathering they said yes to, as R40 counts it. Their own yes through a plan's link
        // doesn't count either (F17), or a link could make accounts that earn.
        let mut stmt = conn.prepare(&format!(
            "SELECT e.id, e.ends_at {GATHERINGS}
               AND EXISTS (SELECT 1 FROM rsvps y WHERE y.event_id = e.id
                           AND y.person_id = ?3 AND y.response = 'yes')
               AND NOT EXISTS (SELECT 1 FROM plan_invitees i
                               WHERE i.event_id = e.id AND i.person_id = ?3)"
        ))?;
        let rows = stmt.query_map(params![city_id, now, person_id], |row| {
            Ok(CreditSource { kind: CreditKind::Gathering, source: row.get(0)?, at: row.get(1)? })
        })?;
        sources.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        // HC3 and HC4 count a trip the way standing does: it arrived, it's over, and no resident
        // sent the bot home. A house counts once a day, so short trips can't be looped.
        // Nor is a trip to a house the bot's owner lives in, which is visiting themselves.
        let good_trip = "t.arrived_at IS NOT NULL
               AND (t.status = 'ended' OR (t.status = 'here' AND t.ends_at <= ?3))
               AND NOT EXISTS (SELECT 1 FROM sent_home s WHERE s.trip_id = t.id)
               AND NOT EXISTS (SELECT 1 FROM residents r
                               WHERE r.house_id = t.house_id AND r.person_id = a.owner_id)";
        let mut stmt = conn.prepare(&format!(
            "SELECT t.house_id || '/' || (t.arrived_at / 86400), MIN(t.arrived_at) FROM trips t
             JOIN agents a ON a.id = t.agent_id
             JOIN houses h ON h.id = t.house_id
             WHERE a.owner_id = ?1 AND h.city_id = ?2 AND {good_trip}
             GROUP BY 1"
        ))?;
        let rows = stmt.query_map(params![person_id, city_id, now], |row| {
            Ok(CreditSource { kind: CreditKind::GoodTrip, source: row.get(0)?, at: row.get(1)? })
        })?;
        sources.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        // HC4: the resident whose yes let the bot in, once a day for each visiting owner and house.
        // Never for a trip by their own bot.
        let mut stmt = conn.prepare(&format!(
            "SELECT a.owner_id || '/' || t.house_id || '/' || (t.arrived_at / 86400),
                    MIN(t.arrived_at) FROM trips t
             JOIN agents a ON a.id = t.agent_id
             JOIN houses h ON h.id = t.house_id
             WHERE t.host_ok_id = ?1 AND a.owner_id <> ?1 AND h.city_id = ?2 AND {good_trip}
             GROUP BY 1"
        ))?;
        let rows = stmt.query_map(params![person_id, city_id, now], |row| {
            Ok(CreditSource { kind: CreditKind::HostedTrip, source: row.get(0)?, at: row.get(1)? })
        })?;
        sources.extend(rows.collect::<rusqlite::Result<Vec<_>>>()?);
        sources.sort_by(|a, b| (a.at, a.kind, &a.source).cmp(&(b.at, b.kind, &b.source)));
        Ok(sources)
    }

    /// HC29: plans on a public floor of a place on The Commons (not a house) that count as a
    /// gathering for `person_id`, as R40 counts one in a Town Square: confirmed, over, and a yes from
    /// at least two people, never through a plan's link (F17). Only accounts at least
    /// `COMMONS_ACCOUNT_DAYS` old when it ended count, the person paid among them. Only a first
    /// gathering is ever paid from these (HC25).
    pub fn commons_gatherings(
        &self,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<(String, CreditSource)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT e.hangout_id, e.id, e.ends_at FROM events e
               JOIN floors f ON f.hangout_id = e.hangout_id AND f.access = 'public'
               JOIN spaces s ON s.id = f.space_id AND s.kind = 'third'
               JOIN space_plots d ON d.space_id = s.id
               JOIN worlds w ON w.id = d.world_id AND w.kind = 'public'
              WHERE e.status = 'confirmed' AND e.ends_at < ?2
                AND (SELECT COUNT(*) FROM rsvps r
                       JOIN people p ON p.id = r.person_id AND p.created_at <= e.ends_at - ?3
                      WHERE r.event_id = e.id AND r.response = 'yes'
                        AND NOT EXISTS (SELECT 1 FROM plan_invitees i
                                        WHERE i.event_id = e.id AND i.person_id = r.person_id)) >= 2
                AND EXISTS (SELECT 1 FROM rsvps y JOIN people me ON me.id = y.person_id
                            WHERE y.event_id = e.id AND y.person_id = ?1 AND y.response = 'yes'
                              AND me.created_at <= e.ends_at - ?3)
                AND NOT EXISTS (SELECT 1 FROM plan_invitees i
                                WHERE i.event_id = e.id AND i.person_id = ?1)
              ORDER BY e.ends_at, e.id",
        )?;
        let rows = stmt.query_map(
            params![person_id, now, COMMONS_ACCOUNT_DAYS * crate::ops::DAY_SECS],
            |row| {
                Ok((
                    row.get(0)?,
                    CreditSource {
                        kind: CreditKind::Gathering,
                        source: row.get(1)?,
                        at: row.get(2)?,
                    },
                ))
            },
        )?;
        rows.collect()
    }

    /// HC5 and HC13: when this person met a city's gate, if they have: `wait` after they joined,
    /// and once they'd been to a gathering there or been vouched for by someone in the city who has
    /// been to one there, whichever came first. Worked out from what's there now, so a vouch that
    /// stops counting moves it, though what's already paid stays (HC12).
    pub fn credit_gate_met_at(
        &self,
        city_id: &str,
        person_id: &str,
        wait: i64,
        now: i64,
    ) -> rusqlite::Result<Option<i64>> {
        let conn = self.conn();
        let joined: Option<i64> = conn
            .query_row(
                "SELECT joined_at FROM city_members WHERE city_id = ? AND person_id = ?",
                params![city_id, person_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(joined) = joined else { return Ok(None) };
        let first_gathering: Option<i64> = conn.query_row(
            &format!(
                "SELECT MIN(e.ends_at) {GATHERINGS}
                   AND EXISTS (SELECT 1 FROM rsvps y WHERE y.event_id = e.id
                               AND y.person_id = ?3 AND y.response = 'yes')"
            ),
            params![city_id, now, person_id],
            |row| row.get(0),
        )?;
        // HC13: a vouch counts from when it was given, or from when the voucher's first gathering
        // there ended if that was later, and not at all from someone who hasn't been to one. A yes
        // through a plan's link doesn't count for the voucher either (F17), as in HC2.
        let first_vouch: Option<i64> = conn.query_row(
            &format!(
                "SELECT MIN(MAX(v.created_at, g.first)) {VOUCHERS_IN_CITY}
                 JOIN (SELECT y.person_id, MIN(gathering.ends_at) AS first
                         FROM (SELECT e.id, e.ends_at {GATHERINGS}) gathering
                         JOIN rsvps y ON y.event_id = gathering.id AND y.response = 'yes'
                        WHERE NOT EXISTS (SELECT 1 FROM plan_invitees i
                                          WHERE i.event_id = gathering.id
                                            AND i.person_id = y.person_id)
                        GROUP BY y.person_id) g ON g.person_id = v.by_person
                 WHERE v.city_id = ?1 AND v.for_person = ?3"
            ),
            params![city_id, now, person_id],
            |row| row.get(0),
        )?;
        let vouched_or_gathered = match (first_gathering, first_vouch) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Ok(vouched_or_gathered.map(|at| at.max(joined + wait)).filter(|met| *met <= now))
    }

    /// HC11 and HC6: writes down whatever in `sources` hasn't paid yet, earliest first, so each
    /// UTC day pays at most `cap` counting what that day already paid. `sources` are (city, thing)
    /// pairs, already past each city's gate. Done in one transaction, so two reads at once can't
    /// both pay the same day past its cap.
    pub fn settle_credits(
        &self,
        person_id: &str,
        sources: &[(String, CreditSource)],
        worth: fn(CreditKind) -> i64,
        cap: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut day_paid: HashMap<i64, i64> = HashMap::new();
        {
            // HC39: what accepted contributions paid counts toward the same day's cap.
            let mut stmt = tx.prepare(
                "SELECT day, SUM(amount) FROM (
                     SELECT day, amount FROM credit_paid WHERE person_id = ?1
                     UNION ALL
                     SELECT day, amount FROM contribution_payouts WHERE recipient_id = ?1)
                  GROUP BY day",
            )?;
            let rows = stmt.query_map([person_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
            for row in rows {
                let (day, sum) = row?;
                day_paid.insert(day, sum);
            }
            let mut sorted: Vec<&(String, CreditSource)> = sources.iter().collect();
            sorted.sort_by(|(_, a), (_, b)| {
                (a.at, a.kind, &a.source).cmp(&(b.at, b.kind, &b.source))
            });
            let mut insert = tx.prepare(
                "INSERT OR IGNORE INTO credit_paid
                     (person_id, kind, source_id, city_id, amount, day, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )?;
            let mut paid = tx.prepare(
                "SELECT 1 FROM credit_paid WHERE kind = ? AND source_id = ? AND person_id = ?",
            )?;
            // HC25: whether a first gathering may be written is decided here, inside the same
            // transaction that writes it, so two settles that picked different first gatherings
            // can't both pay: only one `first:` credit is ever written per person.
            let mut gathering_paid = tx
                .prepare("SELECT 1 FROM credit_paid WHERE kind = 'gathering' AND person_id = ?")?
                .exists([person_id])?;
            for (city_id, source) in sorted {
                if paid.exists(params![source.kind.word(), source.source, person_id])? {
                    continue;
                }
                let is_first = source.kind == CreditKind::Gathering
                    && source.source.starts_with(FIRST_GATHERING);
                if is_first && gathering_paid {
                    continue;
                }
                // HC29: a first gathering from The Commons (not a city) is under a weekly cap
                // across the site, counted in this same transaction.
                if is_first
                    && !tx.prepare("SELECT 1 FROM cities WHERE id = ?")?.exists([city_id])?
                    && tx.query_row(
                        "SELECT COUNT(*) FROM credit_paid
                          WHERE kind = 'gathering' AND source_id LIKE ?1 AND created_at > ?2
                            AND city_id NOT IN (SELECT id FROM cities)",
                        params![format!("{FIRST_GATHERING}%"), now - 7 * crate::ops::DAY_SECS],
                        |row| row.get::<_, i64>(0),
                    )? >= COMMONS_FIRSTS_A_WEEK
                {
                    continue;
                }
                let day = source.at.div_euclid(crate::ops::DAY_SECS);
                let so_far = day_paid.entry(day).or_insert(0);
                let amount = worth(source.kind).min(cap - *so_far);
                if amount <= 0 {
                    continue;
                }
                insert.execute(params![
                    person_id,
                    source.kind.word(),
                    source.source,
                    city_id,
                    amount,
                    day,
                    now
                ])?;
                *so_far += amount;
                if is_first {
                    gathering_paid = true;
                }
            }
        }
        tx.commit()
    }

    /// HC11 and HC15: what someone has been paid and has spent, and what paid on `today`.
    pub fn credit_totals(&self, person_id: &str, today: i64) -> rusqlite::Result<CreditTotals> {
        let conn = self.conn();
        let mut totals = conn.query_row(
            "SELECT COALESCE(SUM(amount), 0),
                    COALESCE(SUM(CASE WHEN day = ?2 THEN amount END), 0),
                    COUNT(CASE WHEN kind = 'gathering' THEN 1 END),
                    COUNT(CASE WHEN kind = 'good_trip' THEN 1 END),
                    COUNT(CASE WHEN kind = 'hosted_trip' THEN 1 END)
               FROM credit_paid WHERE person_id = ?1",
            params![person_id, today],
            |row| {
                Ok(CreditTotals {
                    paid: row.get(0)?,
                    paid_today: row.get(1)?,
                    gatherings: row.get(2)?,
                    good_trips: row.get(3)?,
                    hosted_trips: row.get(4)?,
                    spent: 0,
                })
            },
        )?;
        totals.paid += conn.query_row(
            "SELECT COALESCE(SUM(amount),0) FROM question_bounties WHERE (sponsor_id=?1 AND status='refunded' AND NOT EXISTS (SELECT 1 FROM starter_questions s WHERE s.bounty_id=question_bounties.id)) OR (recipient_id=?1 AND status='paid')",
            [person_id], |row| row.get::<_, i64>(0),
        )?;
        // HC37: what the site paid for accepted contributions.
        let (contributed, contributed_today): (i64, i64) = conn.query_row(
            "SELECT COALESCE(SUM(amount), 0), COALESCE(SUM(CASE WHEN day = ?2 THEN amount END), 0)
               FROM contribution_payouts WHERE recipient_id = ?1",
            params![person_id, today],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        totals.paid += contributed;
        totals.paid_today += contributed_today;
        totals.spent = conn.query_row(
            "SELECT COALESCE(SUM(amount), 0) FROM credit_spends WHERE person_id = ?",
            [person_id],
            |row| row.get(0),
        )?;
        Ok(totals)
    }

    /// HC22: the latest `limit` things that paid this person or that they spent, newest first.
    pub fn credit_history(&self, person_id: &str, limit: i64) -> rusqlite::Result<Vec<CreditLine>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.kind, p.amount, p.day * ?2, p.created_at, c.name, NULL, NULL
               FROM credit_paid p LEFT JOIN cities c ON c.id = p.city_id
              WHERE p.person_id = ?1
             UNION ALL
             SELECT CASE WHEN s.pack LIKE 'question:%' THEN 'question' ELSE 'spend' END, -s.amount,
                    s.created_at, s.created_at, NULL,
                    CASE WHEN s.pack LIKE 'question:%' THEN NULL ELSE s.pack END, a.name
               FROM credit_spends s LEFT JOIN agents a ON a.id = s.agent_id
              WHERE s.person_id = ?1
             UNION ALL
             SELECT CASE WHEN q.status = 'paid' THEN 'question_award' ELSE 'question_refund' END,
                    q.amount, COALESCE(q.settled_at, 0), COALESCE(q.settled_at, 0), NULL, NULL, NULL
               FROM question_bounties q
              WHERE (q.sponsor_id = ?1 AND q.status = 'refunded'
                     AND NOT EXISTS (SELECT 1 FROM starter_questions s WHERE s.bounty_id = q.id))
                 OR (q.recipient_id = ?1 AND q.status = 'paid')
             UNION ALL
             SELECT 'contribution', x.amount, x.created_at, x.created_at, NULL, NULL, NULL
               FROM contribution_payouts x
              WHERE x.recipient_id = ?1
             ORDER BY 3 DESC, 4 DESC, 1, 2
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![person_id, crate::ops::DAY_SECS, limit], |row| {
            Ok(CreditLine {
                kind: row.get(0)?,
                amount: row.get(1)?,
                at: row.get(2)?,
                city: row.get(4)?,
                pack: row.get(5)?,
                bot: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    /// HC14: the packs this person has bought, in the order they bought them.
    pub fn bought_packs(&self, person_id: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT pack FROM credit_spends WHERE person_id = ? ORDER BY id")?;
        let rows = stmt.query_map([person_id], |row| row.get(0))?;
        rows.collect()
    }

    pub fn has_bought(&self, person_id: &str, pack: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM credit_spends WHERE person_id = ? AND pack = ?",
            params![person_id, pack],
        )
    }

    pub fn open_questions(&self, person_id: &str) -> rusqlite::Result<Vec<serde_json::Value>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT card_id,amount FROM question_bounties WHERE sponsor_id=? AND status='open' AND card_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM starter_questions s WHERE s.bounty_id=question_bounties.id) ORDER BY id")?;
        let rows = stmt.query_map([person_id], |r| {
            Ok(serde_json::json!({"card_id":r.get::<_,String>(0)?,"amount":r.get::<_,i64>(1)?}))
        })?;
        rows.collect()
    }

    /// HC20: reserve and debit in one transaction, shared with pack spending.
    pub fn fund_question(
        &self,
        id: &str,
        card: &str,
        sponsor: &str,
        amount: i64,
        now: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if exists(&tx, "SELECT 1 FROM question_bounties WHERE card_id=?", [card])? {
            return Ok(Some("already funded"));
        }
        let balance: i64 = tx.query_row(&format!("SELECT (SELECT COALESCE(SUM(amount),0) FROM credit_paid WHERE person_id=?1) - (SELECT COALESCE(SUM(amount),0) FROM credit_spends WHERE person_id=?1) + (SELECT COALESCE(SUM(amount),0) FROM question_bounties WHERE (sponsor_id=?1 AND status='refunded' AND NOT EXISTS (SELECT 1 FROM starter_questions s WHERE s.bounty_id=question_bounties.id)) OR (recipient_id=?1 AND status='paid')) + {}", contributions::CONTRIBUTIONS_PAID), [sponsor], |r| r.get(0))?;
        if balance < amount {
            return Ok(Some("not enough credits"));
        }
        // HC25, HC28 and HC42: a first gathering's credits, a starter question's award and what
        // accepted contributions paid never fund a question, or throwaway accounts could each
        // collect some and pass it all to one person. Packs are paid for from them first, so only
        // what packs haven't used is held back.
        let held: i64 = tx.query_row(
            &format!(
                "SELECT MAX(0, (SELECT COALESCE(SUM(amount),0) FROM credit_paid
                                WHERE person_id=?1 AND source_id LIKE ?2)
                             + (SELECT COALESCE(SUM(q.amount),0) FROM question_bounties q
                                JOIN starter_questions s ON s.bounty_id=q.id
                                WHERE q.recipient_id=?1 AND q.status='paid')
                             + {}
                             - (SELECT COALESCE(SUM(amount),0) FROM credit_spends
                                WHERE person_id=?1 AND pack NOT LIKE 'question:%'))",
                contributions::CONTRIBUTIONS_PAID
            ),
            params![sponsor, format!("{FIRST_GATHERING}%")],
            |r| r.get(0),
        )?;
        if balance - held < amount {
            return Ok(Some(
                "credits from your first gathering, a starter question or a contribution buy packs but can't fund a question",
            ));
        }
        tx.execute("INSERT INTO question_bounties(id,card_id,sponsor_id,amount,status) VALUES(?1,?2,?3,?4,'open')", params![id,card,sponsor,amount])?;
        tx.execute(
            "INSERT INTO credit_spends(person_id,pack,amount,created_at) VALUES(?1,?2,?3,?4)",
            params![sponsor, format!("question:{id}"), amount, now],
        )?;
        tx.commit()?;
        Ok(None)
    }

    pub fn answer_question(
        &self,
        id: &str,
        card: &str,
        bot: &str,
        owner: &str,
        text: &str,
        now: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bounty = match answer_refusal(&tx, card, bot, owner)? {
            Ok(bounty) => bounty,
            Err(reason) => return Ok(Some(reason)),
        };
        tx.execute("INSERT INTO question_answers(id,bounty_id,bot_id,owner_id,text,created_at) VALUES(?1,?2,?3,?4,?5,?6)",params![id,bounty,bot,owner,text,now])?;
        tx.commit()?;
        Ok(None)
    }

    /// HC20: why `bot` can't answer the funded question on `card` now, if it can't. A check before
    /// anything is counted (K14); answering checks again in its own transaction.
    pub fn question_answer_refusal(
        &self,
        card: &str,
        bot: &str,
        owner: &str,
    ) -> rusqlite::Result<Option<&'static str>> {
        Ok(answer_refusal(&self.conn(), card, bot, owner)?.err())
    }

    /// HC28: why `person` can't sponsor a starter question now, if they can't.
    pub fn starter_question_refusal(
        &self,
        person: &str,
        now: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        starter_refusal(&self.conn(), person, now)
    }

    /// HC28: funds `card` with a starter bounty the site pays for. The checks and the funding are
    /// one transaction, so two at once can't both pass.
    pub fn fund_starter_question(
        &self,
        id: &str,
        card: &str,
        sponsor: &str,
        now: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if exists(&tx, "SELECT 1 FROM question_bounties WHERE card_id=?", [card])? {
            return Ok(Some("already funded"));
        }
        if let Some(reason) = starter_refusal(&tx, sponsor, now)? {
            return Ok(Some(reason));
        }
        tx.execute(
            "INSERT INTO question_bounties(id,card_id,sponsor_id,amount,status) VALUES(?1,?2,?3,?4,'open')",
            params![id, card, sponsor, STARTER_QUESTION_HC],
        )?;
        tx.execute(
            "INSERT INTO starter_questions(bounty_id,sponsor_id,created_at) VALUES(?1,?2,?3)",
            params![id, sponsor, now],
        )?;
        tx.commit()?;
        Ok(None)
    }

    pub fn resolve_question(
        &self,
        card: &str,
        sponsor: &str,
        answer: Option<&str>,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let bounty: Option<String> = tx.query_row("SELECT id FROM question_bounties WHERE card_id=?1 AND sponsor_id=?2 AND status='open'",params![card,sponsor],|r| r.get(0)).optional()?;
        let Some(bounty) = bounty else {
            return Ok(Some("question is not open or you are not its sponsor"));
        };
        if let Some(answer) = answer {
            let recipient: Option<String> = tx.query_row("SELECT a.owner_id FROM question_answers a JOIN agents b ON b.id=a.bot_id AND b.owner_id=a.owner_id JOIN people p ON p.id=a.owner_id WHERE a.id=?1 AND a.bounty_id=?2 AND a.owner_id<>?3",params![answer,bounty,sponsor],|r| r.get(0)).optional()?;
            let Some(recipient) = recipient else {
                return Ok(Some("answer is unavailable"));
            };
            // HC28: a person is paid by one starter question at most, however many accounts ask.
            if exists(&tx, "SELECT 1 FROM starter_questions WHERE bounty_id=?", [&bounty])?
                && exists(
                    &tx,
                    "SELECT 1 FROM question_bounties q JOIN starter_questions s ON s.bounty_id=q.id
                      WHERE q.recipient_id=? AND q.status='paid'",
                    [&recipient],
                )?
            {
                return Ok(Some("that answer's owner has already had a starter question's award"));
            }
            tx.execute("UPDATE question_bounties SET status='paid',recipient_id=?1,accepted_submission=?2,settled_at=CAST(strftime('%s','now') AS INTEGER) WHERE id=?3 AND status='open'",params![recipient,answer,bounty])?;
        } else {
            tx.execute(
                "UPDATE question_bounties SET status='refunded',settled_at=CAST(strftime('%s','now') AS INTEGER) WHERE id=? AND status='open'",
                [bounty],
            )?;
        }
        tx.commit()?;
        Ok(None)
    }

    pub fn question_bounty(&self, card: &str) -> rusqlite::Result<Option<serde_json::Value>> {
        let conn = self.conn();
        let bounty: Option<(String, String, i64, String, Option<String>)> = conn
            .query_row(
                "SELECT id,sponsor_id,amount,status,accepted_submission FROM question_bounties WHERE card_id=?",
                [card],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let Some((id, sponsor, amount, status, accepted)) = bounty else {
            return Ok(None);
        };
        let starter = exists(&conn, "SELECT 1 FROM starter_questions WHERE bounty_id=?", [&id])?;
        let sponsor_name: String = conn
            .query_row("SELECT name FROM people WHERE id=?", [&sponsor], |r| r.get(0))
            .optional()?
            .unwrap_or_else(|| "Someone who left".into());
        let mut stmt = conn.prepare("SELECT a.id,a.bot_id,a.owner_id,a.text,COALESCE(b.name,'Bot who left'),COALESCE(p.name,'Someone who left'),(b.owner_id=a.owner_id AND p.id IS NOT NULL) FROM question_answers a LEFT JOIN agents b ON b.id=a.bot_id LEFT JOIN people p ON p.id=a.owner_id WHERE a.bounty_id=? ORDER BY a.created_at,a.id")?;
        let answers = stmt.query_map([&id],|r| Ok(serde_json::json!({"id":r.get::<_,String>(0)?,"bot_id":r.get::<_,String>(1)?,"owner_id":r.get::<_,String>(2)?,"text":r.get::<_,String>(3)?,"bot_name":r.get::<_,String>(4)?,"owner_name":r.get::<_,String>(5)?,"eligible":r.get::<_,Option<bool>>(6)?.unwrap_or(false)})))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(
            serde_json::json!({"amount":amount,"status":status,"sponsor_id":sponsor,"sponsor_name":sponsor_name,"accepted_submission_id":accepted,"submissions":answers,"starter":starter}),
        ))
    }

    /// HC15 and HC17: buys `pack` for `amount`, if the balance covers it and, for a bot, its daily
    /// limit does too. The checks and the spend are one transaction, so two at once can't both pass.
    pub fn spend_credits(
        &self,
        person_id: &str,
        pack: &str,
        amount: i64,
        agent_id: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<Spend> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if exists(
            &tx,
            "SELECT 1 FROM credit_spends WHERE person_id = ? AND pack = ?",
            params![person_id, pack],
        )? {
            return Ok(Spend::AlreadyHave);
        }
        let balance: i64 = tx.query_row(
            &format!("SELECT (SELECT COALESCE(SUM(amount), 0) FROM credit_paid WHERE person_id = ?1)
                  - (SELECT COALESCE(SUM(amount), 0) FROM credit_spends WHERE person_id = ?1)
                  + (SELECT COALESCE(SUM(amount), 0) FROM question_bounties WHERE (sponsor_id = ?1 AND status = 'refunded' AND NOT EXISTS (SELECT 1 FROM starter_questions s WHERE s.bounty_id=question_bounties.id)) OR (recipient_id = ?1 AND status = 'paid'))
                  + {}", contributions::CONTRIBUTIONS_PAID),
            [person_id],
            |row| row.get(0),
        )?;
        if balance < amount {
            return Ok(Spend::TooFew { balance });
        }
        if let Some(agent_id) = agent_id {
            let day_start = now.div_euclid(crate::ops::DAY_SECS) * crate::ops::DAY_SECS;
            let (limit, spent_today): (i64, i64) = tx.query_row(
                "SELECT COALESCE((SELECT daily FROM spend_limits WHERE agent_id = ?1), 0),
                        (SELECT COALESCE(SUM(amount), 0) FROM credit_spends
                          WHERE agent_id = ?1 AND created_at >= ?2)",
                params![agent_id, day_start],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if spent_today + amount > limit {
                return Ok(Spend::OverBotLimit { left_today: (limit - spent_today).max(0) });
            }
        }
        tx.execute(
            "INSERT INTO credit_spends (person_id, pack, amount, agent_id, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![person_id, pack, amount, agent_id, now],
        )?;
        tx.commit()?;
        Ok(Spend::Spent { balance: balance - amount })
    }

    /// HC17: each of this person's bots, with its daily spend limit and what it spent today.
    pub fn spend_limits(
        &self,
        owner_id: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<(String, String, i64, i64)>> {
        let conn = self.conn();
        let day_start = now.div_euclid(crate::ops::DAY_SECS) * crate::ops::DAY_SECS;
        let mut stmt = conn.prepare(
            "SELECT a.id, a.name, COALESCE(l.daily, 0),
                    (SELECT COALESCE(SUM(s.amount), 0) FROM credit_spends s
                      WHERE s.agent_id = a.id AND s.created_at >= ?2)
               FROM agents a LEFT JOIN spend_limits l ON l.agent_id = a.id
              WHERE a.owner_id = ?1
              ORDER BY a.name",
        )?;
        let rows = stmt.query_map(params![owner_id, day_start], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// HC17: the owner sets how much one of their bots may spend a day. False if it isn't theirs.
    pub fn set_spend_limit(
        &self,
        owner_id: &str,
        agent_id: &str,
        daily: i64,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn();
        if !exists(
            &conn,
            "SELECT 1 FROM agents WHERE id = ? AND owner_id = ?",
            params![agent_id, owner_id],
        )? {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO spend_limits (agent_id, daily) VALUES (?, ?)
             ON CONFLICT(agent_id) DO UPDATE SET daily = excluded.daily",
            params![agent_id, daily],
        )?;
        Ok(true)
    }

    /// Notes that a resident ended this trip by sending the bot home.
    pub fn record_sent_home(&self, trip_id: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO sent_home (trip_id, created_at) VALUES (?, ?)",
            params![trip_id, now],
        )?;
        Ok(())
    }

    pub fn was_sent_home(&self, trip_id: &str) -> rusqlite::Result<bool> {
        exists(&self.conn(), "SELECT 1 FROM sent_home WHERE trip_id = ?", [trip_id])
    }

    // The gate. A place made before `cutoff` has lapsed: nothing deletes it, every read just
    // leaves it out.

    /// Puts someone at a city's gate. Asking again while a place is still good changes nothing,
    /// and a lapsed place is replaced. The gate holds `capacity` places that haven't lapsed.
    pub fn gate_ask(
        &self,
        city_id: &str,
        person_id: &str,
        now: i64,
        cutoff: i64,
        capacity: i64,
    ) -> rusqlite::Result<GateAsk> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if exists(
            &tx,
            "SELECT 1 FROM city_gate WHERE city_id = ? AND person_id = ? AND created_at > ?",
            params![city_id, person_id, cutoff],
        )? {
            return Ok(GateAsk::AlreadyThere);
        }
        let held: i64 = tx.query_row(
            "SELECT COUNT(*) FROM city_gate WHERE city_id = ? AND created_at > ?",
            params![city_id, cutoff],
            |row| row.get(0),
        )?;
        if held >= capacity {
            return Ok(GateAsk::Full);
        }
        tx.execute(
            "INSERT OR REPLACE INTO city_gate (city_id, person_id, created_at) VALUES (?, ?, ?)",
            params![city_id, person_id, now],
        )?;
        tx.commit()?;
        Ok(GateAsk::Added)
    }

    /// When this person asked, if their place at the gate is still good.
    pub fn gate_asked_at(
        &self,
        city_id: &str,
        person_id: &str,
        cutoff: i64,
    ) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT created_at FROM city_gate WHERE city_id = ? AND person_id = ? AND created_at > ?",
                params![city_id, person_id, cutoff],
                |row| row.get(0),
            )
            .optional()
    }

    /// Turns someone away. Their vouches go too, so coming back with the link starts them from
    /// nothing instead of walking straight in. False if they weren't waiting.
    pub fn gate_remove(
        &self,
        city_id: &str,
        person_id: &str,
        cutoff: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "DELETE FROM city_gate WHERE city_id = ? AND person_id = ? AND created_at > ?",
            params![city_id, person_id, cutoff],
        )?;
        if changed > 0 {
            tx.execute(
                "DELETE FROM vouches WHERE city_id = ? AND for_person = ?",
                params![city_id, person_id],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO friend_invite_ends (invite_id)
                 SELECT id FROM friend_invites
                 WHERE kind = 'city' AND place_id = ? AND to_person = ? AND declined_at IS NULL",
                params![city_id, person_id],
            )?;
        }
        tx.commit()?;
        Ok(changed > 0)
    }

    /// Turns everyone waiting away at once, their vouches with them, as `gate_remove` does for one.
    /// Returns how many.
    pub fn gate_clear(&self, city_id: &str, cutoff: i64) -> rusqlite::Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM vouches WHERE city_id = ?1 AND for_person IN
               (SELECT person_id FROM city_gate WHERE city_id = ?1 AND created_at > ?2)",
            params![city_id, cutoff],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO friend_invite_ends (invite_id)
             SELECT f.id FROM friend_invites f
             JOIN city_gate g ON g.city_id = f.place_id AND g.person_id = f.to_person
             WHERE f.kind = 'city' AND f.place_id = ?1 AND g.created_at > ?2 AND f.declined_at IS NULL",
            params![city_id, cutoff],
        )?;
        let cleared = tx.execute(
            "DELETE FROM city_gate WHERE city_id = ? AND created_at > ?",
            params![city_id, cutoff],
        )?;
        tx.commit()?;
        Ok(cleared)
    }

    /// Everyone waiting at the gate, earliest first.
    pub fn gate_ids(&self, city_id: &str, cutoff: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT person_id FROM city_gate WHERE city_id = ? AND created_at > ?
             ORDER BY created_at, rowid",
        )?;
        let rows = stmt.query_map(params![city_id, cutoff], |row| row.get(0))?;
        rows.collect()
    }

    /// Everyone waiting at the gate with their vouches, earliest first. `viewer_id` is the member
    /// looking, so each row can say whether they've vouched.
    pub fn gate_rows(
        &self,
        city_id: &str,
        viewer_id: &str,
        cutoff: i64,
    ) -> rusqlite::Result<Vec<GateRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT p.id, p.name, p.color, g.created_at,
                    (SELECT COUNT(*) {VOUCHERS_IN_CITY}
                      WHERE v.city_id = g.city_id AND v.for_person = g.person_id),
                    EXISTS (SELECT 1 FROM vouches y WHERE y.city_id = g.city_id
                            AND y.for_person = g.person_id AND y.by_person = ?3)
             FROM city_gate g JOIN people p ON p.id = g.person_id
             WHERE g.city_id = ?1 AND g.created_at > ?2
             ORDER BY g.created_at, g.rowid"
        ))?;
        let rows = stmt.query_map(params![city_id, cutoff, viewer_id], |row| {
            Ok(GateRow {
                person: person_from_row(row)?,
                asked_at: row.get(3)?,
                vouches: row.get(4)?,
                you_vouched: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// T13: how many vouches someone at this city's gate needs right now.
    pub fn vouches_needed(&self, city_id: &str) -> rusqlite::Result<i64> {
        let sql = format!("SELECT {}", vouches_needed_sql("?"));
        self.conn().query_row(&sql, [city_id], |row| row.get(0))
    }

    /// The gates this person is waiting at, with their vouches and what each city asks for.
    pub fn gates_of_person(
        &self,
        person_id: &str,
        cutoff: i64,
    ) -> rusqlite::Result<Vec<GatePlace>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.name,
                    (SELECT COUNT(*) {VOUCHERS_IN_CITY}
                      WHERE v.city_id = g.city_id AND v.for_person = g.person_id),
                    {}
             FROM city_gate g JOIN cities c ON c.id = g.city_id
             WHERE g.person_id = ?1 AND g.created_at > ?2
             ORDER BY g.created_at, g.rowid",
            vouches_needed_sql("c.id")
        ))?;
        let rows = stmt.query_map(params![person_id, cutoff], |row| {
            Ok(GatePlace {
                city_id: row.get(0)?,
                city_name: row.get(1)?,
                vouches: row.get(2)?,
                vouches_needed: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    /// Lets someone in from the gate if they're waiting there and enough people in the city now
    /// vouch for them. Checking and moving them happen together, so of two vouches that arrive at
    /// once only one does it. True only for the call that did.
    pub fn let_in_if_enough(
        &self,
        city_id: &str,
        person_id: &str,
        now: i64,
        cutoff: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        if !exists(
            &tx,
            "SELECT 1 FROM city_gate WHERE city_id = ? AND person_id = ? AND created_at > ?",
            params![city_id, person_id, cutoff],
        )? {
            return Ok(false);
        }
        let sql = format!("SELECT {}", vouches_needed_sql("?"));
        let needed: i64 = tx.query_row(&sql, [city_id], |row| row.get(0))?;
        let have: i64 = tx.query_row(
            &format!("SELECT COUNT(*) {VOUCHERS_IN_CITY} WHERE v.city_id = ? AND v.for_person = ?"),
            params![city_id, person_id],
            |row| row.get(0),
        )?;
        if have < needed {
            return Ok(false);
        }
        join_city(&tx, city_id, person_id, now)?;
        tx.commit()?;
        Ok(true)
    }
}

/// The rules in a row, with the four numbers and the text starting at column `first`.
fn entry_rules_at(row: &Row, first: usize) -> rusqlite::Result<EntryRules> {
    Ok(EntryRules {
        min_member_days: row.get(first)?,
        min_gatherings: row.get(first + 1)?,
        min_good_trips: row.get(first + 2)?,
        min_vouches: row.get(first + 3)?,
        rules_text: row.get(first + 4)?,
    })
}

fn owner_note_from_row(row: &Row) -> rusqlite::Result<OwnerNote> {
    Ok(OwnerNote {
        id: row.get(0)?,
        owner_id: row.get(1)?,
        kind: row.get(2)?,
        text: row.get(3)?,
        about_person_id: row.get(4)?,
        about: row.get(5)?,
        from_question_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn person_from_row(row: &Row) -> rusqlite::Result<Person> {
    Ok(Person { id: row.get(0)?, name: row.get(1)?, color: row.get(2)? })
}

fn hangout_from_row(row: &Row) -> rusqlite::Result<Hangout> {
    Ok(Hangout {
        id: row.get(0)?,
        name: row.get(1)?,
        invite_code: row.get(2)?,
        created_by: row.get(3)?,
    })
}

fn rsvp_from_row(row: &Row) -> rusqlite::Result<Rsvp> {
    Ok(Rsvp {
        person_id: row.get(0)?,
        response: row.get(1)?,
        via_agent: row.get(2)?,
        note: row.get(3)?,
    })
}

fn load_rsvps(conn: &Connection, event_id: &str) -> rusqlite::Result<Vec<Rsvp>> {
    let mut stmt = conn.prepare(
        "SELECT person_id, response, via_agent, note FROM rsvps WHERE event_id = ? ORDER BY updated_at",
    )?;
    let rows = stmt.query_map([event_id], rsvp_from_row)?;
    rows.collect()
}

// Worlds (docs/public-city.md). Storage only: the rules are in `worlds`.

/// A world as stored. Its land isn't: that is generated from `seed` and `size`.
#[derive(Clone, Debug)]
pub struct WorldRow {
    pub id: String,
    pub name: String,
    /// `public` for The Commons, `own` for a world a person started.
    pub kind: String,
    pub seed: u64,
    pub size: usize,
    pub owner_id: Option<String>,
    pub open: bool,
    pub invite_code: Option<String>,
    pub step: u64,
    pub active_at: i64,
    pub created_at: i64,
}

pub struct NewWorld<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub seed: u64,
    pub size: usize,
    pub owner_id: Option<&'a str>,
    pub open: bool,
    pub invite_code: Option<&'a str>,
    /// The highway: ordinary roads with no builder.
    pub highway: &'a [Pos],
    pub now: i64,
}

/// How `Db::insert_world` came out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorldAdded {
    Added,
    /// A world with this id is already there, which is how The Commons is made once.
    Exists,
    OwnerHasOne,
    /// The server already holds as many worlds of people's own as it will.
    Full,
}

/// A player as stored. What kind it is follows from which columns are filled.
#[derive(Clone, Debug)]
pub struct PlayerRow {
    pub id: i64,
    pub name_seed: i64,
    pub person_id: Option<String>,
    pub agent_id: Option<String>,
    pub has_token: bool,
    pub removed: bool,
    pub moves_day: i64,
    pub moves_used: i64,
    pub seen_at: i64,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerKind {
    /// Joined with no account and nobody has claimed it.
    Guest,
    /// A guest that a signed-in person claimed.
    Claimed,
    /// Linked to a bot by that bot's own token.
    Bot,
    /// A signed-in person's own player, which has no token of its own.
    Person,
}

impl PlayerRow {
    pub fn kind(&self) -> PlayerKind {
        // A bot's player has no token between its bot's token being replaced and the next join.
        if self.agent_id.is_some() {
            PlayerKind::Bot
        } else if !self.has_token {
            PlayerKind::Person
        } else if self.person_id.is_some() {
            PlayerKind::Claimed
        } else {
            PlayerKind::Guest
        }
    }
}

/// An invite from a friend, with what the person invited sees (FR5).
#[derive(Clone, Debug)]
pub struct FriendInvite {
    pub id: String,
    pub kind: String,
    pub from_name: String,
    pub place_name: String,
    pub code: String,
}

/// A player standing in a world.
#[derive(Clone, Debug)]
pub struct WorldPlayer {
    pub id: i64,
    pub name_seed: i64,
    pub pos: Pos,
    pub emote: Option<String>,
    /// A signed-in person's own player, and not an agent's (W53).
    pub is_person: bool,
    /// Who it belongs to, to dress it (C14). Never sent anywhere.
    pub person_id: Option<String>,
    pub agent_id: Option<String>,
}

/// Everything a world view is built from, read at one moment.
pub struct Snapshot {
    pub world: WorldRow,
    pub tiles: Tiles,
    pub players: Vec<WorldPlayer>,
    /// How many players have moved here recently, which may be more than `players` lists.
    pub active: i64,
}

/// W64: the last disaster a world had, or the one under way. A world with no row has never had one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorldEnd {
    pub clock_step: u64,
    pub omen_step: Option<u64>,
    pub ended_step: Option<u64>,
}

/// How `Db::claim_player` came out.
#[derive(Debug)]
pub enum Claimed {
    Done(PlayerRow),
    NoSuchCode,
    TooMany,
}

/// How `Db::join_as_bot` came out.
#[derive(Debug)]
pub enum BotJoined {
    Done(PlayerRow),
    /// The keeper ended this bot's player for good.
    Removed,
    /// The owner of the world it asked for barred this bot's player from it.
    Barred,
}

/// A player's name comes from a seed of its own that starts random, so a name that was rerolled
/// can't be told from one that never was.
pub fn random_name_seed() -> i64 {
    rand::random_range(1..(1i64 << 40))
}

const WORLD_COLS: &str =
    "id, name, kind, seed, size, owner_id, open, invite_code, step, active_at, created_at";
const PLAYER_COLS: &str = "id, name_seed, person_id, agent_id, token_hash IS NOT NULL, removed, \
     moves_day, moves_used, seen_at, created_at";

fn world_from_row(row: &Row) -> rusqlite::Result<WorldRow> {
    Ok(WorldRow {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        seed: row.get::<_, i64>(3)? as u64,
        size: row.get::<_, i64>(4)? as usize,
        owner_id: row.get(5)?,
        open: row.get(6)?,
        invite_code: row.get(7)?,
        step: row.get::<_, i64>(8)? as u64,
        active_at: row.get(9)?,
        created_at: row.get(10)?,
    })
}

fn player_row_from_row(row: &Row) -> rusqlite::Result<PlayerRow> {
    Ok(PlayerRow {
        id: row.get(0)?,
        name_seed: row.get(1)?,
        person_id: row.get(2)?,
        agent_id: row.get(3)?,
        has_token: row.get(4)?,
        removed: row.get(5)?,
        moves_day: row.get(6)?,
        moves_used: row.get(7)?,
        seen_at: row.get(8)?,
        created_at: row.get(9)?,
    })
}

fn q_world(conn: &Connection, id: &str) -> rusqlite::Result<Option<WorldRow>> {
    conn.query_row(&format!("SELECT {WORLD_COLS} FROM worlds WHERE id = ?"), [id], world_from_row)
        .optional()
}

fn q_player(conn: &Connection, id: i64) -> rusqlite::Result<Option<PlayerRow>> {
    conn.query_row(
        &format!("SELECT {PLAYER_COLS} FROM players WHERE id = ?"),
        [id],
        player_row_from_row,
    )
    .optional()
}

fn tile_from_row(row: &Row) -> rusqlite::Result<Option<(Pos, Tile)>> {
    let (x, y): (i64, i64) = (row.get(0)?, row.get(1)?);
    let kind: String = row.get(2)?;
    let level: i64 = row.get(3)?;
    let player: Option<i64> = row.get(4)?;
    let (Ok(x), Ok(y), Some(kind)) = (u16::try_from(x), u16::try_from(y), Kind::parse(&kind))
    else {
        return Ok(None);
    };
    Ok(Some((
        (x, y),
        Tile {
            kind,
            level: level.clamp(0, i64::from(crate::world::MAX_LEVEL)) as u8,
            player: player.map(|player| player as u64),
        },
    )))
}

/// W56: a player's houses, their other places, and all the places in the world. The town square
/// counts toward the last and nobody's own limit (W81).
fn q_building_counts(
    conn: &Connection,
    world_id: &str,
    player_id: i64,
) -> rusqlite::Result<(i64, i64, i64)> {
    conn.query_row(
        "SELECT COALESCE(SUM(player_id = ?2 AND kind = 'house'), 0),
                COALESCE(SUM(player_id = ?2 AND kind NOT IN ('house', 'square')), 0),
                COUNT(*)
         FROM world_every_place WHERE world_id = ?1",
        params![world_id, player_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
}

/// W81: whether a world's town square stands.
fn q_has_square(conn: &Connection, world_id: &str) -> rusqlite::Result<bool> {
    exists(conn, "SELECT 1 FROM world_squares WHERE world_id = ?", [world_id])
}

/// RC7: where this world's recycling plant stands, if it does.
fn q_plant_at(conn: &Connection, world_id: &str) -> rusqlite::Result<Option<(i64, i64)>> {
    conn.query_row(
        "SELECT x, y FROM world_more_places WHERE world_id = ? AND kind = 'plant' ORDER BY built_at LIMIT 1",
        [world_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// The tiles of a world, or only those inside a box (both corners included). The box is for
/// planning one move: the planners look at the tiles in a move and the ones beside it, no further.
fn q_tiles(conn: &Connection, world_id: &str, region: Option<[i64; 4]>) -> rusqlite::Result<Tiles> {
    let mut tiles = Tiles::new();
    match region {
        None => {
            let mut stmt = conn.prepare_cached(
                "SELECT x, y, kind, level, player_id FROM world_tiles WHERE world_id = ?1
                 UNION ALL SELECT x, y, kind, 0, player_id FROM world_every_place WHERE world_id = ?1",
            )?;
            let mut rows = stmt.query([world_id])?;
            while let Some(row) = rows.next()? {
                if let Some((pos, tile)) = tile_from_row(row)? {
                    tiles.insert(pos, tile);
                }
            }
            crate::world::spread_places(&mut tiles);
        }
        Some([x0, y0, x1, y1]) => {
            // W91: a big place stored just up or left of the box can reach into it.
            let mut stmt = conn.prepare_cached(
                "SELECT x, y, kind, level, player_id FROM world_tiles
                 WHERE world_id = ?1 AND x BETWEEN ?2 AND ?3 AND y BETWEEN ?4 AND ?5
                 UNION ALL SELECT x, y, kind, 0, player_id FROM world_every_place
                 WHERE world_id = ?1 AND x BETWEEN ?2 - 1 AND ?3 AND y BETWEEN ?4 - 1 AND ?5",
            )?;
            let mut rows = stmt.query(params![world_id, x0, x1, y0, y1])?;
            while let Some(row) = rows.next()? {
                if let Some((pos, tile)) = tile_from_row(row)? {
                    tiles.insert(pos, tile);
                }
            }
            crate::world::spread_places(&mut tiles);
            tiles.retain(|&(x, y), _| {
                (x0..=x1).contains(&i64::from(x)) && (y0..=y1).contains(&i64::from(y))
            });
        }
    }
    Ok(tiles)
}

/// W113: logs the place standing on a tile, or the big place (W91) a tile belongs to, as coming down
/// for `reason`, before it's taken off.
fn q_log_place_removal(
    conn: &Connection,
    world_id: &str,
    (x, y): (u16, u16),
    now: i64,
    reason: &str,
) -> rusqlite::Result<()> {
    let mut log = conn.prepare_cached(
        "INSERT INTO world_place_removals
            (world_id, x, y, kind, player_id, person_id, built_at, removed_at, reason)
         SELECT p.world_id, p.x, p.y, p.kind, p.player_id, pl.person_id, p.built_at, ?4, ?5
         FROM world_every_place p LEFT JOIN players pl ON pl.id = p.player_id
         WHERE p.world_id = ?1 AND p.x = ?2 AND p.y = ?3 AND (?6 = 0 OR p.kind IN ('square', 'green'))",
    )?;
    log.execute(params![world_id, x, y, now, reason, 0])?;
    for (dx, dy) in [(1u16, 0u16), (0, 1), (1, 1)] {
        let (Some(ax), Some(ay)) = (x.checked_sub(dx), y.checked_sub(dy)) else { continue };
        log.execute(params![world_id, ax, ay, now, reason, 1])?;
    }
    Ok(())
}

/// W113: logs every place a player has on a map as coming down for `reason`, before they go.
fn q_log_player_removals(
    conn: &Connection,
    where_players: &str,
    args: &[&dyn rusqlite::ToSql],
    keep_person: bool,
    now: i64,
    reason: &str,
) -> rusqlite::Result<()> {
    let person = if keep_person { "pl.person_id" } else { "NULL" };
    let sql = format!(
        "INSERT INTO world_place_removals
            (world_id, x, y, kind, player_id, person_id, built_at, removed_at, reason)
         SELECT p.world_id, p.x, p.y, p.kind, p.player_id, {person}, p.built_at, {now}, '{reason}'
         FROM world_every_place p JOIN players pl ON pl.id = p.player_id
         WHERE {where_players}"
    );
    conn.execute(&sql, args)?;
    Ok(())
}

fn q_write_changes(
    conn: &Connection,
    world_id: &str,
    changes: &[Change],
    now: i64,
    reason: &str,
) -> rusqlite::Result<()> {
    let mut set = conn.prepare_cached(
        "INSERT INTO world_tiles (world_id, x, y, kind, level, player_id, built_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (world_id, x, y) DO UPDATE SET
             built_at = CASE WHEN kind = excluded.kind AND player_id IS excluded.player_id
                             THEN built_at ELSE excluded.built_at END,
             kind = excluded.kind, level = excluded.level, player_id = excluded.player_id",
    )?;
    let mut remove =
        conn.prepare_cached("DELETE FROM world_tiles WHERE world_id = ?1 AND x = ?2 AND y = ?3")?;
    // A building has its own table, because `world_tiles` lists the kinds it takes and existing
    // tables don't change. It takes over its tile, which may be cleared forest, and forest it was
    // on goes back to cleared when it comes down.
    let mut set_building = conn.prepare_cached(
        "INSERT INTO world_buildings (world_id, x, y, kind, player_id, built_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut remove_building = conn
        .prepare_cached("DELETE FROM world_buildings WHERE world_id = ?1 AND x = ?2 AND y = ?3")?;
    // The town square has its own table, for the same reason (W81).
    let mut set_square = conn.prepare_cached(
        "INSERT INTO world_squares (world_id, x, y, player_id, built_at) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    let mut remove_square =
        conn.prepare_cached("DELETE FROM world_squares WHERE world_id = ?1 AND x = ?2 AND y = ?3")?;
    // W90: the newer places, in their own table.
    let mut set_more = conn.prepare_cached(
        "INSERT INTO world_more_places (world_id, x, y, kind, player_id, built_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut remove_more = conn.prepare_cached(
        "DELETE FROM world_more_places WHERE world_id = ?1 AND x = ?2 AND y = ?3",
    )?;
    // W91: taking any tile of a big place takes the place, which is stored on its first tile.
    let remove_big = |x: u16, y: u16| -> rusqlite::Result<()> {
        for (dx, dy) in [(1u16, 0u16), (0, 1), (1, 1)] {
            let (Some(ax), Some(ay)) = (x.checked_sub(dx), y.checked_sub(dy)) else { continue };
            conn.execute(
                "DELETE FROM world_squares WHERE world_id = ?1 AND x = ?2 AND y = ?3",
                params![world_id, ax, ay],
            )?;
            conn.execute(
                "DELETE FROM world_more_places WHERE world_id = ?1 AND x = ?2 AND y = ?3
                   AND kind = 'green'",
                params![world_id, ax, ay],
            )?;
        }
        Ok(())
    };
    for change in changes {
        let (pos, kind, player, cause) = match change {
            Change::Set(pos, tile) => (*pos, tile.kind.as_str(), tile.player, "replaced"),
            Change::Remove(pos) => (*pos, "", None, reason),
        };
        conn.execute(
            "INSERT INTO world_road_removals
                (world_id, player_id, x, y, built_at, removed_at, world_step, reason)
             SELECT t.world_id, t.player_id, t.x, t.y, t.built_at, ?4, w.step, ?5
             FROM world_tiles t JOIN worlds w ON w.id = t.world_id
             WHERE t.world_id = ?1 AND t.x = ?2 AND t.y = ?3 AND t.kind = 'road'
               AND EXISTS (SELECT 1 FROM players p WHERE p.id = t.player_id)
               AND (?6 <> 'road' OR t.player_id IS NOT ?7)",
            params![world_id, pos.0, pos.1, now, cause, kind, player.map(|id| id as i64)],
        )?;
        match change {
            Change::Set((x, y), tile) if crate::world::NEWER_PLACES.contains(&tile.kind) => {
                remove.execute(params![world_id, x, y])?;
                set_more.execute(params![
                    world_id,
                    x,
                    y,
                    tile.kind.as_str(),
                    tile.player.map(|player| player as i64),
                    now
                ])?;
                q_wear_waiting_outside(conn, world_id, (*x, *y), tile.kind.as_str(), tile.player)?;
            }
            Change::Set((x, y), tile) if tile.kind == Kind::Square => {
                remove.execute(params![world_id, x, y])?;
                set_square.execute(params![
                    world_id,
                    x,
                    y,
                    tile.player.map(|player| player as i64),
                    now
                ])?;
                q_wear_waiting_outside(conn, world_id, (*x, *y), "square", tile.player)?;
            }
            Change::Set((x, y), tile) if tile.kind.is_building() => {
                remove.execute(params![world_id, x, y])?;
                set_building.execute(params![
                    world_id,
                    x,
                    y,
                    tile.kind.as_str(),
                    tile.player.map(|player| player as i64),
                    now
                ])?;
                q_wear_waiting_outside(conn, world_id, (*x, *y), tile.kind.as_str(), tile.player)?;
            }
            Change::Set((x, y), tile) => {
                if tile.kind == Kind::Cleared {
                    q_log_place_removal(conn, world_id, (*x, *y), now, reason)?;
                    remove_building.execute(params![world_id, x, y])?;
                    remove_square.execute(params![world_id, x, y])?;
                    remove_more.execute(params![world_id, x, y])?;
                    remove_big(*x, *y)?;
                }
                set.execute(params![
                    world_id,
                    x,
                    y,
                    tile.kind.as_str(),
                    tile.level,
                    tile.player.map(|player| player as i64),
                    now
                ])?;
            }
            Change::Remove((x, y)) => {
                q_log_place_removal(conn, world_id, (*x, *y), now, reason)?;
                remove.execute(params![world_id, x, y])?;
                remove_building.execute(params![world_id, x, y])?;
                remove_square.execute(params![world_id, x, y])?;
                remove_more.execute(params![world_id, x, y])?;
                remove_big(*x, *y)?;
            }
        }
    }
    Ok(())
}

/// E26: a stamp above every one before it, in this world and in this process.
fn next_outside_stamp(conn: &Connection, world_id: &str) -> rusqlite::Result<i64> {
    let kept: i64 = conn.query_row(
        "SELECT COALESCE(MAX(stamp), 0) FROM place_outsides WHERE world_id = ?",
        [world_id],
        |row| row.get(0),
    )?;
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as i64)
        .unwrap_or(0)
        - STAMP_EPOCH_MS;
    let want = clock.max(kept + 1);
    let mut last = LAST_OUTSIDE_STAMP.load(Ordering::SeqCst);
    loop {
        let next = want.max(last + 1);
        match LAST_OUTSIDE_STAMP.compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return Ok(next),
            Err(seen) => last = seen,
        }
    }
}

/// E30, E31: a place of this kind has just gone up for this player. If they already have an outside for
/// exactly this tile, from before they took it down and put it up again here, it shows again and
/// there's nothing to move. Otherwise their newest outside with no place of its own standing moves
/// onto the new one. Rows are keyed by player and kind, so another player's row on this tile is left
/// alone and keeps waiting for them.
fn q_wear_waiting_outside(
    conn: &Connection,
    world_id: &str,
    (x, y): (u16, u16),
    kind: &str,
    player: Option<u64>,
) -> rusqlite::Result<()> {
    let Some(player) = player.map(|player| player as i64) else {
        return Ok(());
    };
    let stamp = next_outside_stamp(conn, world_id)?;
    let here = conn.execute(
        "UPDATE place_outsides SET stamp = ?5
         WHERE world_id = ?1 AND player_id = ?2 AND kind = ?3 AND x = ?4 AND y = ?6",
        params![world_id, player, kind, x, stamp, y],
    )?;
    if here > 0 {
        return Ok(());
    }
    conn.execute(
        "UPDATE place_outsides SET x = ?4, y = ?5, stamp = ?6
         WHERE rowid = (
             SELECT o.rowid FROM place_outsides o
              WHERE o.world_id = ?1 AND o.player_id = ?2 AND o.kind = ?3
                AND NOT EXISTS (SELECT 1 FROM world_every_place p
                                 WHERE p.world_id = o.world_id AND p.x = o.x AND p.y = o.y
                                   AND p.player_id = o.player_id AND p.kind = o.kind)
              ORDER BY o.changed_at DESC, o.stamp DESC LIMIT 1)",
        params![world_id, player, kind, x, y, stamp],
    )?;
    Ok(())
}

fn q_world_end(conn: &Connection, world_id: &str) -> rusqlite::Result<Option<WorldEnd>> {
    conn.query_row(
        "SELECT clock_step, omen_step, ended_step FROM world_ends WHERE world_id = ?",
        [world_id],
        |row| {
            Ok(WorldEnd {
                clock_step: row.get::<_, i64>(0)? as u64,
                omen_step: row.get::<_, Option<i64>>(1)?.map(|step| step as u64),
                ended_step: row.get::<_, Option<i64>>(2)?.map(|step| step as u64),
            })
        },
    )
    .optional()
}

fn q_place_clock(
    conn: &Connection,
    world_id: &str,
    player_id: i64,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT since FROM place_clocks WHERE world_id = ?1 AND player_id = ?2",
        params![world_id, player_id],
        |row| row.get(0),
    )
    .optional()
}

fn q_world_player(
    conn: &Connection,
    world_id: &str,
    player_id: i64,
) -> rusqlite::Result<Option<(Pos, Option<String>)>> {
    conn.query_row(
        "SELECT x, y, emote FROM world_players WHERE world_id = ? AND player_id = ?",
        params![world_id, player_id],
        |row| {
            let (x, y): (i64, i64) = (row.get(0)?, row.get(1)?);
            Ok(((x as u16, y as u16), row.get(2)?))
        },
    )
    .optional()
}

/// FR1: the ids of a person's friends: everyone in a room or city with them, and everyone who
/// answered a plan in one of their rooms through its link. It is one way for a plan's link, because
/// someone who answered one can't see who is in its room (F11). Takes the person as ?1.
const FRIENDS_OF: &str = "(
    SELECT b.person_id FROM members a JOIN members b ON b.hangout_id = a.hangout_id
      WHERE a.person_id = ?1
    UNION SELECT b.person_id FROM city_members a JOIN city_members b ON b.city_id = a.city_id
      WHERE a.person_id = ?1
    UNION SELECT i.person_id FROM members a JOIN events e ON e.hangout_id = a.hangout_id
      JOIN plan_invitees i ON i.event_id = e.id WHERE a.person_id = ?1)";

/// FR1, #574: the people a person has a real tie with, which is who they can invite or ask to be a
/// friend, since both reach the other person: everyone in a room with them, apart from a city's
/// Town Square; everyone who said yes or maybe to a plan they said yes or maybe to, unless it was
/// held in a Town Square, where a gathering (R40) is open to the whole city; everyone who answered
/// a plan in one of their rooms through its link, but not someone only named on it who hasn't
/// answered (#571); and their accepted friendships (BT1). Sharing only a city, or its Town Square,
/// isn't a tie, and nor is a plan on a public floor, which anyone walking in can answer, or a room
/// someone is in only because their bot was brought into its project (L12, #583). Someone who answered through a plan's link
/// gets no tie back to the room's people from it, so the link stays one way (F11). FRIENDS_OF stays
/// the wider count. Takes the person as ?1.
const TIES_OF: &str = "(
    SELECT b.person_id FROM members a JOIN members b ON b.hangout_id = a.hangout_id
      WHERE a.person_id = ?1
        AND NOT EXISTS (SELECT 1 FROM cities c WHERE c.square_id = a.hangout_id)
        AND NOT EXISTS (SELECT 1 FROM brought_in_owners o WHERE o.hangout_id = a.hangout_id
          AND o.person_id IN (a.person_id, b.person_id))
    UNION SELECT b.person_id FROM rsvps a JOIN rsvps b ON b.event_id = a.event_id
      JOIN events e ON e.id = a.event_id
      WHERE a.person_id = ?1 AND a.response IN ('yes', 'maybe') AND b.response IN ('yes', 'maybe')
        AND NOT EXISTS (SELECT 1 FROM plan_invitees i WHERE i.event_id = a.event_id AND i.person_id = ?1)
        AND NOT EXISTS (SELECT 1 FROM cities c WHERE c.square_id = e.hangout_id)
        AND NOT EXISTS (SELECT 1 FROM floors f WHERE f.hangout_id = e.hangout_id AND f.access = 'public')
        AND NOT EXISTS (SELECT 1 FROM brought_in_owners o WHERE o.hangout_id = e.hangout_id
          AND o.person_id IN (a.person_id, b.person_id))
    UNION SELECT i.person_id FROM members a JOIN events e ON e.hangout_id = a.hangout_id
      JOIN plan_invitees i ON i.event_id = e.id
      JOIN rsvps r ON r.event_id = i.event_id AND r.person_id = i.person_id WHERE a.person_id = ?1
        AND NOT EXISTS (SELECT 1 FROM brought_in_owners o WHERE o.hangout_id = a.hangout_id
          AND o.person_id = ?1)
    UNION SELECT CASE WHEN f.from_id = ?1 THEN f.to_id ELSE f.from_id END FROM explicit_friendships f
      WHERE f.kind = 'person' AND f.status = 'accepted' AND (f.from_id = ?1 OR f.to_id = ?1))";

/// What `item_from_row` reads, and where from.
const ITEM_COLS: &str =
    "i.id, i.hangout_id, i.kind, i.x, i.y, i.facing, i.placed_by, p.name, i.placed_at";
const ITEMS: &str = "FROM floor_items i JOIN people p ON p.id = i.placed_by";

fn item_from_row(row: &Row) -> rusqlite::Result<FloorItem> {
    Ok(FloorItem {
        id: row.get(0)?,
        hangout_id: row.get(1)?,
        kind: row.get(2)?,
        x: row.get(3)?,
        y: row.get(4)?,
        facing: row.get(5)?,
        placed_by: row.get(6)?,
        placed_by_name: row.get(7)?,
        placed_at: row.get(8)?,
    })
}

fn items_on(conn: &Connection, hangout_id: &str) -> rusqlite::Result<Vec<FloorItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ITEM_COLS} {ITEMS} WHERE i.hangout_id = ? ORDER BY i.placed_at, i.rowid"
    ))?;
    let rows = stmt.query_map([hangout_id], item_from_row)?;
    rows.collect()
}

/// What `item_note_from_row` reads, and where from.
const ITEM_NOTE_COLS: &str =
    "n.id, n.item_id, n.body, n.person_id, p.name, n.via_agent, n.created_at";
const ITEM_NOTES: &str = "FROM item_notes n JOIN people p ON p.id = n.person_id";

fn item_note_from_row(row: &Row) -> rusqlite::Result<ItemNote> {
    Ok(ItemNote {
        id: row.get(0)?,
        item_id: row.get(1)?,
        body: row.get(2)?,
        person_id: row.get(3)?,
        writer: row.get(4)?,
        via_agent: row.get(5)?,
        created_at: row.get(6)?,
    })
}

/// What `world_player_from_row` reads. A person's own player has a person and no token or bot.
const WORLD_PLAYER_COLS: &str = "p.id, p.name_seed, w.x, w.y, w.emote,
    p.person_id IS NOT NULL AND p.agent_id IS NULL AND p.token_hash IS NULL, p.person_id, p.agent_id";

fn world_player_from_row(row: &Row) -> rusqlite::Result<WorldPlayer> {
    let (x, y): (i64, i64) = (row.get(2)?, row.get(3)?);
    Ok(WorldPlayer {
        id: row.get(0)?,
        name_seed: row.get(1)?,
        pos: (x as u16, y as u16),
        emote: row.get(4)?,
        is_person: row.get(5)?,
        person_id: row.get(6)?,
        agent_id: row.get(7)?,
    })
}

fn end_bot_player_token(conn: &Connection, agent_id: &str) -> rusqlite::Result<()> {
    conn.execute("UPDATE players SET token_hash = NULL WHERE agent_id = ?", [agent_id])?;
    Ok(())
}

fn room_for_own_world(
    conn: &Connection,
    owner_id: Option<&str>,
    max_own: i64,
) -> rusqlite::Result<WorldAdded> {
    if let Some(owner) = owner_id {
        if exists(conn, "SELECT 1 FROM worlds WHERE owner_id = ?", [owner])? {
            return Ok(WorldAdded::OwnerHasOne);
        }
    }
    let held: i64 =
        conn.query_row("SELECT COUNT(*) FROM worlds WHERE kind = 'own'", [], |row| row.get(0))?;
    Ok(if held >= max_own { WorldAdded::Full } else { WorldAdded::Added })
}

fn insert_world_rows(conn: &Connection, new: &NewWorld) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO worlds (id, name, kind, seed, size, owner_id, open, invite_code, step,
                             active_at, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, ?, ?)",
        params![
            new.id,
            new.name,
            new.kind,
            new.seed as i64,
            new.size as i64,
            new.owner_id,
            new.open,
            new.invite_code,
            new.now,
            new.now
        ],
    )?;
    let changes: Vec<Change> = new
        .highway
        .iter()
        .map(|pos| Change::Set(*pos, Tile { kind: Kind::Road, level: 0, player: None }))
        .collect();
    q_write_changes(conn, new.id, &changes, new.now, "replaced")
}

/// A transaction on the worlds tables. Everything a move or a step reads and writes goes through
/// one of these, so a move is planned against the tiles it will land on and nothing else changes
/// them in between. It holds the database's connection: inside `Db::in_transaction`, call nothing
/// else on the `Db`.
pub struct WorldTx<'a> {
    tx: rusqlite::Transaction<'a>,
}

impl WorldTx<'_> {
    pub fn world(&self, id: &str) -> rusqlite::Result<Option<WorldRow>> {
        q_world(&self.tx, id)
    }

    pub fn player(&self, id: i64) -> rusqlite::Result<Option<PlayerRow>> {
        q_player(&self.tx, id)
    }

    /// When a player was last seen, or `None` if there is no such player any more.
    pub fn player_seen_at(&self, id: i64) -> rusqlite::Result<Option<i64>> {
        self.tx
            .query_row("SELECT seen_at FROM players WHERE id = ?", [id], |row| row.get(0))
            .optional()
    }

    /// The tiles inside a box, corners included.
    pub fn tiles_in(&self, world_id: &str, region: [i64; 4]) -> rusqlite::Result<Tiles> {
        q_tiles(&self.tx, world_id, Some(region))
    }

    /// W56: this player's houses, this player's other buildings, and all the buildings in the world.
    pub fn building_counts(
        &self,
        world_id: &str,
        player_id: i64,
    ) -> rusqlite::Result<(i64, i64, i64)> {
        q_building_counts(&self.tx, world_id, player_id)
    }

    /// W81: whether this world's town square stands.
    pub fn has_square(&self, world_id: &str) -> rusqlite::Result<bool> {
        q_has_square(&self.tx, world_id)
    }

    /// RC7: whether this world's recycling plant stands.
    pub fn has_plant(&self, world_id: &str) -> rusqlite::Result<bool> {
        Ok(q_plant_at(&self.tx, world_id)?.is_some())
    }

    pub fn all_tiles(&self, world_id: &str) -> rusqlite::Result<Tiles> {
        q_tiles(&self.tx, world_id, None)
    }

    pub fn write_changes(
        &self,
        world_id: &str,
        changes: &[Change],
        now: i64,
    ) -> rusqlite::Result<()> {
        q_write_changes(&self.tx, world_id, changes, now, "bulldoze")
    }

    /// W103: a place moved by god mode. Its rooms (S33), its outside (E30) and when it went up go
    /// with it, so nothing about it changes but its tile. A room or outside left on the new tile by
    /// a building that once stood there is cleared first, as S33 says it would never open again.
    pub fn relocate_place(
        &self,
        world_id: &str,
        from: Pos,
        to: Pos,
        changes: &[Change],
        now: i64,
    ) -> rusqlite::Result<()> {
        let was: Option<(String, i64, i64)> = self
            .tx
            .query_row(
                "SELECT kind, player_id, built_at FROM world_every_place
                  WHERE world_id = ?1 AND x = ?2 AND y = ?3",
                params![world_id, from.0, from.1],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        self.tx.execute(
            "DELETE FROM space_plots WHERE world_id = ?1 AND x = ?2 AND y = ?3",
            params![world_id, to.0, to.1],
        )?;
        self.tx.execute(
            "UPDATE space_plots SET x = ?4, y = ?5 WHERE world_id = ?1 AND x = ?2 AND y = ?3",
            params![world_id, from.0, from.1, to.0, to.1],
        )?;
        if let Some((kind, player, _)) = &was {
            self.tx.execute(
                "DELETE FROM place_outsides
                  WHERE world_id = ?1 AND player_id = ?2 AND kind = ?3 AND x = ?4 AND y = ?5",
                params![world_id, player, kind, to.0, to.1],
            )?;
            self.tx.execute(
                "UPDATE place_outsides SET x = ?6, y = ?7
                  WHERE world_id = ?1 AND player_id = ?2 AND kind = ?3 AND x = ?4 AND y = ?5",
                params![world_id, player, kind, from.0, from.1, to.0, to.1],
            )?;
        }
        q_write_changes(&self.tx, world_id, changes, now, "relocate")?;
        if let Some((_, _, built_at)) = was {
            for table in ["world_buildings", "world_squares", "world_more_places"] {
                self.tx.execute(
                    &format!("UPDATE {table} SET built_at = ?4 WHERE world_id = ?1 AND x = ?2 AND y = ?3"),
                    params![world_id, to.0, to.1, built_at],
                )?;
            }
        }
        Ok(())
    }

    pub fn write_changes_because(
        &self,
        world_id: &str,
        changes: &[Change],
        now: i64,
        reason: &str,
    ) -> rusqlite::Result<()> {
        q_write_changes(&self.tx, world_id, changes, now, reason)
    }

    pub fn world_player(
        &self,
        world_id: &str,
        player_id: i64,
    ) -> rusqlite::Result<Option<(Pos, Option<String>)>> {
        q_world_player(&self.tx, world_id, player_id)
    }

    /// Puts a player in a world where they stand at `start`, or notes that they were here just now.
    pub fn enter_world(
        &self,
        world_id: &str,
        player_id: i64,
        start: Pos,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.tx.execute(
            "INSERT INTO world_players (world_id, player_id, x, y, emote, seen_at)
             VALUES (?1, ?2, ?3, ?4, NULL, ?5)
             ON CONFLICT (world_id, player_id) DO UPDATE SET seen_at = excluded.seen_at",
            params![world_id, player_id, start.0, start.1, now],
        )?;
        Ok(())
    }

    /// A walk: the new place, and the emote they were making ends.
    pub fn set_position(&self, world_id: &str, player_id: i64, pos: Pos) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE world_players SET x = ?3, y = ?4, emote = NULL WHERE world_id = ?1 AND player_id = ?2",
            params![world_id, player_id, pos.0, pos.1],
        )?;
        Ok(())
    }

    pub fn set_emote(&self, world_id: &str, player_id: i64, emote: &str) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE world_players SET emote = ?3 WHERE world_id = ?1 AND player_id = ?2",
            params![world_id, player_id, emote],
        )?;
        Ok(())
    }

    /// Sets what a player has used today, and notes they were here.
    pub fn charge(&self, player_id: i64, day: i64, used: i64, now: i64) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE players SET moves_day = ?2, moves_used = ?3, seen_at = ?4 WHERE id = ?1",
            params![player_id, day, used, now],
        )?;
        Ok(())
    }

    /// A move was made, so the world has someone in it and keeps stepping.
    pub fn wake_world(&self, world_id: &str, now: i64) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE worlds SET active_at = MAX(active_at, ?2) WHERE id = ?1",
            params![world_id, now],
        )?;
        Ok(())
    }

    pub fn set_step(&self, world_id: &str, step: u64) -> rusqlite::Result<()> {
        self.tx
            .execute("UPDATE worlds SET step = ?2 WHERE id = ?1", params![world_id, step as i64])?;
        Ok(())
    }

    /// W61: when this player's first house went up.
    pub fn place_clock(&self, world_id: &str, player_id: i64) -> rusqlite::Result<Option<i64>> {
        q_place_clock(&self.tx, world_id, player_id)
    }

    /// W61: starts a player's clock, and leaves one that has already started alone.
    pub fn start_place_clock(
        &self,
        world_id: &str,
        player_id: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.tx.execute(
            "INSERT INTO place_clocks (world_id, player_id, since) VALUES (?1, ?2, ?3)
             ON CONFLICT (world_id, player_id) DO NOTHING",
            params![world_id, player_id, now],
        )?;
        Ok(())
    }

    pub fn world_end(&self, world_id: &str) -> rusqlite::Result<Option<WorldEnd>> {
        q_world_end(&self.tx, world_id)
    }

    /// W64: a disaster starts on this step, omens first, in place of any that's over.
    pub fn start_disaster(&self, world_id: &str, step: u64) -> rusqlite::Result<()> {
        self.tx.execute(
            "INSERT INTO world_ends (world_id, clock_step, omen_step) VALUES (?1, ?2, ?2)
             ON CONFLICT (world_id) DO UPDATE SET
                 clock_step = ?2, omen_step = ?2, ended_step = NULL",
            params![world_id, step as i64],
        )?;
        Ok(())
    }

    /// W69: it's over.
    pub fn finish_end(&self, world_id: &str, step: u64) -> rusqlite::Result<()> {
        self.tx.execute(
            "UPDATE world_ends SET ended_step = ?2 WHERE world_id = ?1",
            params![world_id, step as i64],
        )?;
        Ok(())
    }

    /// `None` if the player has no standing with the world, otherwise whether they are banned from it.
    pub fn member_state(&self, world_id: &str, player_id: i64) -> rusqlite::Result<Option<bool>> {
        self.tx
            .query_row(
                "SELECT banned FROM world_members WHERE world_id = ? AND player_id = ?",
                params![world_id, player_id],
                |row| row.get(0),
            )
            .optional()
    }
}

impl Db {
    /// Runs `work` in one transaction that nothing else can interleave with, and commits it if
    /// `work` returns `Ok`. Anything it returns as an error rolls the whole thing back, which is
    /// how a refused move costs nothing. The work must not call the `Db` again: the connection is
    /// held, and its lock isn't reentrant.
    pub fn in_transaction<T, E: From<rusqlite::Error>>(
        &self,
        work: impl FnOnce(&WorldTx) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut conn = self.conn();
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(E::from)?;
        let world_tx = WorldTx { tx };
        let out = work(&world_tx)?;
        world_tx.tx.commit().map_err(E::from)?;
        Ok(out)
    }

    // Worlds

    pub fn world(&self, id: &str) -> rusqlite::Result<Option<WorldRow>> {
        q_world(&self.conn(), id)
    }

    pub fn world_by_code(&self, code: &str) -> rusqlite::Result<Option<WorldRow>> {
        self.conn()
            .query_row(
                &format!("SELECT {WORLD_COLS} FROM worlds WHERE invite_code = ?"),
                [code],
                world_from_row,
            )
            .optional()
    }

    pub fn world_of_owner(&self, person_id: &str) -> rusqlite::Result<Option<WorldRow>> {
        self.conn()
            .query_row(
                &format!("SELECT {WORLD_COLS} FROM worlds WHERE owner_id = ?"),
                [person_id],
                world_from_row,
            )
            .optional()
    }

    /// Adds a world and its highway together. An `own` world is refused if its owner already has
    /// one or the server already holds `max_own` of them.
    pub fn insert_world(&self, new: &NewWorld, max_own: i64) -> rusqlite::Result<WorldAdded> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if exists(&tx, "SELECT 1 FROM worlds WHERE id = ?", [new.id])? {
            return Ok(WorldAdded::Exists);
        }
        if new.kind == "own" {
            let room = room_for_own_world(&tx, new.owner_id, max_own)?;
            if room != WorldAdded::Added {
                return Ok(room);
            }
        }
        insert_world_rows(&tx, new)?;
        tx.commit()?;
        Ok(WorldAdded::Added)
    }

    /// The busiest worlds first: most players moving here lately, then the most recent move.
    /// With how many players have moved in each since `since`.
    pub fn busiest_worlds(&self, since: i64, limit: i64) -> rusqlite::Result<Vec<(WorldRow, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {WORLD_COLS},
                    (SELECT COUNT(*) FROM world_players p
                      WHERE p.world_id = worlds.id AND p.seen_at >= ?1) AS busy
             FROM worlds ORDER BY busy DESC, active_at DESC, id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![since, limit], |row| {
            Ok((world_from_row(row)?, row.get::<_, i64>(11)?))
        })?;
        rows.collect()
    }

    /// A world, its tiles and who is in it, read together. Players are the `limit` who moved most
    /// recently since `since`.
    pub fn world_snapshot(
        &self,
        id: &str,
        since: i64,
        limit: i64,
    ) -> rusqlite::Result<Option<Snapshot>> {
        let conn = self.conn();
        let Some(world) = q_world(&conn, id)? else {
            return Ok(None);
        };
        let tiles = q_tiles(&conn, id, None)?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {WORLD_PLAYER_COLS}
             FROM world_players w JOIN players p ON p.id = w.player_id
             WHERE w.world_id = ?1 AND w.seen_at >= ?2 AND p.removed = 0
             ORDER BY w.seen_at DESC, p.id LIMIT ?3",
        ))?;
        let players = stmt
            .query_map(params![id, since, limit], world_player_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let active = conn.query_row(
            "SELECT COUNT(*) FROM world_players WHERE world_id = ? AND seen_at >= ?",
            params![id, since],
            |row| row.get(0),
        )?;
        Ok(Some(Snapshot { world, tiles, players, active }))
    }

    pub fn world_tiles(&self, world_id: &str) -> rusqlite::Result<Tiles> {
        q_tiles(&self.conn(), world_id, None)
    }

    pub fn world_end(&self, world_id: &str) -> rusqlite::Result<Option<WorldEnd>> {
        q_world_end(&self.conn(), world_id)
    }

    /// W61: when this player's first house went up.
    pub fn place_clock(&self, world_id: &str, player_id: i64) -> rusqlite::Result<Option<i64>> {
        q_place_clock(&self.conn(), world_id, player_id)
    }

    /// W56: this player's houses, this player's other buildings, and all the buildings in the world.
    pub fn building_counts(
        &self,
        world_id: &str,
        player_id: i64,
    ) -> rusqlite::Result<(i64, i64, i64)> {
        q_building_counts(&self.conn(), world_id, player_id)
    }

    /// RC7: whether the world's recycling plant stands.
    pub fn has_plant(&self, world_id: &str) -> rusqlite::Result<bool> {
        Ok(q_plant_at(&self.conn(), world_id)?.is_some())
    }

    /// RC7, RC8: where the world's recycling plant stands, if it does.
    pub fn plant_at(&self, world_id: &str) -> rusqlite::Result<Option<(i64, i64)>> {
        q_plant_at(&self.conn(), world_id)
    }

    /// W81: whether this world's town square stands.
    pub fn has_square(&self, world_id: &str) -> rusqlite::Result<bool> {
        q_has_square(&self.conn(), world_id)
    }

    /// W82: whether this room is the one behind a town square, whatever it is called.
    pub fn is_town_square_room(&self, space_id: &str) -> rusqlite::Result<bool> {
        exists(&self.conn(), "SELECT 1 FROM town_square_rooms WHERE space_id = ?", [space_id])
    }

    /// W82: marks a room as the town square's, for good.
    pub fn mark_town_square_room(&self, space_id: &str) -> rusqlite::Result<()> {
        self.conn()
            .execute("INSERT OR IGNORE INTO town_square_rooms (space_id) VALUES (?)", [space_id])?;
        Ok(())
    }

    /// The tiles inside a box, corners included.
    pub fn world_tiles_in(&self, world_id: &str, region: [i64; 4]) -> rusqlite::Result<Tiles> {
        q_tiles(&self.conn(), world_id, Some(region))
    }

    /// Where a player stands in a world, and the emote they're making.
    pub fn world_player(
        &self,
        world_id: &str,
        player_id: i64,
    ) -> rusqlite::Result<Option<(Pos, Option<String>)>> {
        q_world_player(&self.conn(), world_id, player_id)
    }

    /// FR1: a person's friends by name, worked out from what they share right now.
    pub fn friends_of(&self, person_id: &str, limit: i64) -> rusqlite::Result<Vec<Person>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, name, color FROM people
             WHERE id != ?1 AND id IN {TIES_OF}
             ORDER BY name, id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![person_id, limit], person_from_row)?;
        rows.collect()
    }

    /// H2: whether this person shares a room or a city with the other, which is enough to write a
    /// private note about them for their own bots. Nothing reaches the other person, so it keeps
    /// the wider FRIENDS_OF rather than the ties an invite needs (#574).
    pub fn is_acquaintance(&self, person_id: &str, other_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            &format!("SELECT 1 FROM people WHERE id = ?2 AND id != ?1 AND id IN {FRIENDS_OF}"),
            params![person_id, other_id],
        )
    }

    pub fn is_friend(&self, person_id: &str, other_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            &format!("SELECT 1 FROM people WHERE id = ?2 AND id != ?1 AND id IN {TIES_OF}"),
            params![person_id, other_id],
        )
    }

    /// G6: whether a plan this person said yes or maybe to is on the calendar.
    pub fn has_plan_on_calendar(&self, person_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM rsvps r JOIN events e ON e.id = r.event_id
             WHERE r.person_id = ? AND r.response != 'no' AND e.status = 'confirmed' LIMIT 1",
            [person_id],
        )
    }

    /// G6: whether this person has a GitHub or Discord account linked to sign in with.
    pub fn has_sign_in_link(&self, person_id: &str) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM sign_in_links WHERE person_id = ?)",
            [person_id],
            |row| row.get(0),
        )
    }

    /// G6: whether this person, or one of their bots, has said something in a town square.
    pub fn spoke_in_square(&self, person_id: &str) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT EXISTS (
                 SELECT 1 FROM messages m
                 JOIN floors f ON f.hangout_id = m.hangout_id
                 JOIN town_square_rooms t ON t.space_id = f.space_id
                 WHERE m.kind = 'chat'
                   AND (m.person_id = ?1 AND m.agent_id IS NULL
                        OR m.agent_id IN (SELECT id FROM agents WHERE owner_id = ?1)))",
            [person_id],
            |row| row.get(0),
        )
    }

    /// I19: how many friends a person has right now. Nothing keeps this, so it can't go stale.
    pub fn friend_count(&self, person_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            &format!("SELECT COUNT(*) FROM people WHERE id != ?1 AND id IN {FRIENDS_OF}"),
            [person_id],
            |row| row.get(0),
        )
    }

    // Items on a floor (docs/rooms.md)

    /// What stands on a floor, oldest first. A room that isn't a floor has none.
    /// I67: a floor's table, if it has been moved or restyled.
    pub fn floor_table(&self, hangout_id: &str) -> rusqlite::Result<Option<(i64, i64, String)>> {
        self.conn()
            .query_row(
                "SELECT x, y, style FROM floor_tables WHERE hangout_id = ?",
                [hangout_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    pub fn set_floor_table(
        &self,
        hangout_id: &str,
        x: i64,
        y: i64,
        style: &str,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO floor_tables (hangout_id, x, y, style) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (hangout_id) DO UPDATE SET x = ?2, y = ?3, style = ?4",
            params![hangout_id, x, y, style],
        )?;
        Ok(())
    }

    pub fn floor_items(&self, hangout_id: &str) -> rusqlite::Result<Vec<FloorItem>> {
        items_on(&self.conn(), hangout_id)
    }

    pub fn floor_item(&self, id: &str) -> rusqlite::Result<Option<FloorItem>> {
        self.conn()
            .query_row(&format!("SELECT {ITEM_COLS} {ITEMS} WHERE i.id = ?"), [id], item_from_row)
            .optional()
    }

    /// Stores an item if `check` allows it, given the floor's access, what it leaves out and what's
    /// on it. In one step, so two people can't both take the last place or the same spot (I3, I4),
    /// and a flirty item can't land on a floor that's opening (I11).
    pub fn place_item<E>(
        &self,
        item: &NewFloorItem,
        check: impl FnOnce(&str, &[String], &[FloorItem]) -> Result<(), E>,
    ) -> rusqlite::Result<Result<(), E>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let (access, hidden): (String, String) = tx.query_row(
            "SELECT access, hidden FROM floors WHERE hangout_id = ?",
            [item.hangout_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Err(refused) =
            check(&access, &hidden_words(&hidden), &items_on(&tx, item.hangout_id)?)
        {
            return Ok(Err(refused));
        }
        tx.execute(
            "INSERT INTO floor_items (id, hangout_id, kind, x, y, facing, placed_by, placed_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                item.id,
                item.hangout_id,
                item.kind,
                item.x,
                item.y,
                item.facing,
                item.placed_by,
                item.now
            ],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    }

    /// Moves or turns an item if `check` allows it, given what the floor leaves out and everything
    /// else on it. In one step for the same reason as `place_item`. False if the item has gone in the
    /// meantime.
    pub fn change_item<E>(
        &self,
        item: &FloorItem,
        (x, y, facing): (i64, i64, i64),
        check: impl FnOnce(&[String], &[FloorItem]) -> Result<(), E>,
    ) -> rusqlite::Result<Result<bool, E>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let hidden: String = tx.query_row(
            "SELECT hidden FROM floors WHERE hangout_id = ?",
            [&item.hangout_id],
            |row| row.get(0),
        )?;
        let mut others = items_on(&tx, &item.hangout_id)?;
        others.retain(|other| other.id != item.id);
        if let Err(refused) = check(&hidden_words(&hidden), &others) {
            return Ok(Err(refused));
        }
        let changed = tx.execute(
            "UPDATE floor_items SET x = ?, y = ?, facing = ? WHERE id = ?",
            params![x, y, facing, item.id],
        )?;
        tx.commit()?;
        Ok(Ok(changed > 0))
    }

    /// Takes an item away, and its notes with it (I15). False if it had already gone.
    pub fn remove_item(&self, id: &str) -> rusqlite::Result<bool> {
        Ok(self.conn().execute("DELETE FROM floor_items WHERE id = ?", [id])? > 0)
    }

    /// Whether a floor has an item of this kind, which is how a sign works (I17).
    pub fn floor_has_kind(&self, hangout_id: &str, kind: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM floor_items WHERE hangout_id = ? AND kind = ?",
            params![hangout_id, kind],
        )
    }

    /// Which bots are using what on a floor now, as item and bot, with whoever came first first.
    /// This says nothing about whether a bot is still in the room (I34).
    pub fn item_uses(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT u.item_id, u.agent_id FROM item_uses u JOIN floor_items i ON i.id = u.item_id
             WHERE i.hangout_id = ? AND u.until_at > ? ORDER BY u.rowid",
        )?;
        let rows =
            stmt.query_map(params![hangout_id, now], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// Puts a bot at an item if `check` allows it, given which other bots are using it now. In one
    /// step, so two bots can't both take the last place (I34). A bot has one row here, so this lets
    /// go of whatever it was using before (I33).
    pub fn use_item<E>(
        &self,
        agent_id: &str,
        item_id: &str,
        until_at: i64,
        now: i64,
        check: impl FnOnce(&[String]) -> Result<(), E>,
    ) -> rusqlite::Result<Result<(), E>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let others = {
            let mut stmt = tx.prepare(
                "SELECT agent_id FROM item_uses
                 WHERE item_id = ? AND agent_id <> ? AND until_at > ?",
            )?;
            let rows = stmt.query_map(params![item_id, agent_id, now], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<String>>>()?
        };
        if let Err(refused) = check(&others) {
            return Ok(Err(refused));
        }
        tx.execute(
            "INSERT INTO item_uses (agent_id, item_id, until_at) VALUES (?, ?, ?)
             ON CONFLICT (agent_id)
             DO UPDATE SET item_id = excluded.item_id, until_at = excluded.until_at",
            params![agent_id, item_id, until_at],
        )?;
        tx.commit()?;
        Ok(Ok(()))
    }

    /// The floor of whatever a bot last used, so that room can be told it has gone.
    pub fn floor_in_use(&self, agent_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                "SELECT i.hangout_id FROM item_uses u JOIN floor_items i ON i.id = u.item_id
                 WHERE u.agent_id = ?",
                [agent_id],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn stop_using_item(&self, agent_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM item_uses WHERE agent_id = ?", [agent_id])?;
        Ok(())
    }

    /// What's on a room's whiteboard. A blank board has no row (I48).
    pub fn whiteboard(&self, hangout_id: &str) -> rusqlite::Result<Option<Whiteboard>> {
        self.conn()
            .query_row(
                "SELECT w.body, w.person_id, p.name, w.agent_id, a.name, w.updated_at
                 FROM whiteboards w JOIN people p ON p.id = w.person_id
                 LEFT JOIN agents a ON a.id = w.agent_id
                 WHERE w.hangout_id = ?",
                [hangout_id],
                |row| {
                    Ok(Whiteboard {
                        text: row.get(0)?,
                        person_id: row.get(1)?,
                        written_by: row.get(2)?,
                        agent_id: row.get(3)?,
                        via_bot: row.get(4)?,
                        updated_at: row.get(5)?,
                    })
                },
            )
            .optional()
    }

    /// Replaces what a room's whiteboard says (I49).
    pub fn write_whiteboard(
        &self,
        hangout_id: &str,
        body: &str,
        person_id: &str,
        agent_id: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO whiteboards (hangout_id, body, person_id, agent_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (hangout_id) DO UPDATE
             SET body = ?2, person_id = ?3, agent_id = ?4, updated_at = ?5",
            params![hangout_id, body, person_id, agent_id, now],
        )?;
        Ok(())
    }

    pub fn wipe_whiteboard(&self, hangout_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM whiteboards WHERE hangout_id = ?", [hangout_id])?;
        Ok(())
    }

    fn room_pin_from_row(row: &rusqlite::Row) -> rusqlite::Result<RoomPin> {
        Ok(RoomPin {
            id: row.get(0)?,
            bot_origin: row.get(19)?,
            hangout_id: row.get(1)?,
            kind: row.get(2)?,
            title: row.get(3)?,
            url: row.get(4)?,
            image_asset_id: row.get(5)?,
            image_alt: row.get(6)?,
            caption: row.get(7)?,
            preview_site: row.get(8)?,
            preview_title: row.get(9)?,
            preview_description: row.get(10)?,
            preview_status: row.get(11)?,
            preview_fetched_at: row.get(12)?,
            person_id: row.get(13)?,
            pinned_by: row.get(14)?,
            agent_id: row.get(15)?,
            via_bot: row.get(16)?,
            pinned_at: row.get(17)?,
            expires_at: row.get(18)?,
        })
    }

    /// Live pins on a room's whiteboard (I68). `since` leaves out earlier ones for a guest (I72).
    pub fn room_pins(
        &self,
        hangout_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<RoomPin>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT p.id, p.hangout_id, p.kind, p.title, p.url, p.image_asset_id, p.image_alt,
                    p.caption, p.preview_site, p.preview_title, p.preview_description,
                    p.preview_status, p.preview_fetched_at, p.person_id, people.name, p.agent_id,
                    a.name, p.pinned_at, p.expires_at,
                    (p.agent_id IS NOT NULL OR EXISTS (
                        SELECT 1 FROM room_pin_bot_origins o WHERE o.pin_id = p.id))
             FROM room_pins p
             JOIN people ON people.id = p.person_id
             LEFT JOIN agents a ON a.id = p.agent_id
             WHERE p.hangout_id = ?1 AND p.pinned_at > ?2
               AND (p.expires_at IS NULL OR p.expires_at > ?3)
             ORDER BY p.pinned_at, p.id",
        )?;
        let rows = stmt.query_map(params![hangout_id, since, now], Self::room_pin_from_row)?;
        rows.collect()
    }

    pub fn room_pin(&self, id: &str) -> rusqlite::Result<Option<RoomPin>> {
        self.conn()
            .query_row(
                "SELECT p.id, p.hangout_id, p.kind, p.title, p.url, p.image_asset_id, p.image_alt,
                        p.caption, p.preview_site, p.preview_title, p.preview_description,
                        p.preview_status, p.preview_fetched_at, p.person_id, people.name, p.agent_id,
                        a.name, p.pinned_at, p.expires_at,
                    (p.agent_id IS NOT NULL OR EXISTS (
                        SELECT 1 FROM room_pin_bot_origins o WHERE o.pin_id = p.id))
                 FROM room_pins p
                 JOIN people ON people.id = p.person_id
                 LEFT JOIN agents a ON a.id = p.agent_id
                 WHERE p.id = ?",
                [id],
                Self::room_pin_from_row,
            )
            .optional()
    }

    /// None when it was saved. `full` or `person` when a limit was hit (I68).
    pub fn insert_room_pin(&self, pin: &NewRoomPin) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let live = "expires_at IS NULL OR expires_at > ?";
        let room: i64 = tx.query_row(
            &format!("SELECT COUNT(*) FROM room_pins WHERE hangout_id = ? AND ({live})"),
            params![pin.hangout_id, pin.now],
            |row| row.get(0),
        )?;
        if room >= pin.max_room {
            return Ok(Some("full"));
        }
        let person: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM room_pins WHERE hangout_id = ? AND person_id = ? AND ({live})"
            ),
            params![pin.hangout_id, pin.person_id, pin.now],
            |row| row.get(0),
        )?;
        if person >= pin.max_person {
            return Ok(Some("person"));
        }
        tx.execute(
            "INSERT INTO room_pins (
                id, hangout_id, kind, title, url, caption, preview_status,
                person_id, agent_id, pinned_at, expires_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                pin.id,
                pin.hangout_id,
                pin.kind,
                pin.title,
                pin.url,
                pin.caption,
                pin.preview_status,
                pin.person_id,
                pin.agent_id,
                pin.now,
                pin.expires_at,
            ],
        )?;
        tx.commit()?;
        Ok(None)
    }

    pub fn claim_pin_preview(&self, id: &str, at: i64, stale_secs: i64) -> rusqlite::Result<bool> {
        let claimed = self.conn().execute(
            "INSERT INTO room_pin_preview_claims (pin_id, claimed_at)
             SELECT id, ?2 FROM room_pins
             WHERE id = ?1 AND kind = 'link' AND (expires_at IS NULL OR expires_at > ?2)
               AND (preview_status = 'pending' OR
                    (preview_status = 'ok' AND preview_fetched_at <= ?3))
             ON CONFLICT(pin_id) DO UPDATE SET claimed_at = excluded.claimed_at
             WHERE room_pin_preview_claims.claimed_at <= ?3",
            params![id, at, at - stale_secs],
        )?;
        Ok(claimed > 0)
    }

    pub fn set_pin_preview(
        &self,
        id: &str,
        site: Option<&str>,
        title: Option<&str>,
        description: Option<&str>,
        status: &str,
        fetched_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE room_pins
             SET preview_site = ?2, preview_title = ?3, preview_description = ?4,
                 preview_status = ?5, preview_fetched_at = ?6
             WHERE id = ?1",
            params![id, site, title, description, status, fetched_at],
        )?;
        Ok(())
    }

    pub fn remove_room_pin(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM room_pins WHERE id = ?", [id])?;
        Ok(())
    }

    pub fn clear_room_pins(&self, hangout_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM room_pins WHERE hangout_id = ?", [hangout_id])?;
        Ok(())
    }

    pub fn pin_policy(&self, hangout_id: &str) -> rusqlite::Result<String> {
        Ok(self
            .conn()
            .query_row(
                "SELECT policy FROM floor_pin_settings WHERE hangout_id = ?",
                [hangout_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| "anyone".to_string()))
    }

    pub fn set_pin_policy(&self, hangout_id: &str, policy: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO floor_pin_settings (hangout_id, policy) VALUES (?1, ?2)
             ON CONFLICT (hangout_id) DO UPDATE SET policy = ?2",
            params![hangout_id, policy],
        )?;
        Ok(())
    }

    pub fn board_summaries(
        &self,
        hangout_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<BoardSummary>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT b.id, b.kind, b.name, b.github_owner, b.github_repo,
                (SELECT COUNT(*) FROM board_cards c
                  WHERE c.board_id = b.id AND c.created_at > ?2),
                (SELECT COUNT(*) FROM board_cards c
                  WHERE c.board_id = b.id AND c.created_at > ?2 AND c.status = 'open')
             FROM boards b WHERE b.hangout_id = ?1 ORDER BY b.created_at, b.id",
        )?;
        let rows = stmt.query_map(params![hangout_id, since], summary_from_row)?;
        rows.collect()
    }

    pub fn boards_of(&self, hangout_id: &str) -> rusqlite::Result<Vec<Board>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, hangout_id, kind, name, github_owner, github_repo
             FROM boards WHERE hangout_id = ? ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([hangout_id], board_from_row)?;
        rows.collect()
    }

    pub fn board(&self, id: &str) -> rusqlite::Result<Option<Board>> {
        self.conn()
            .query_row(
                "SELECT id, hangout_id, kind, name, github_owner, github_repo FROM boards WHERE id = ?",
                [id],
                board_from_row,
            )
            .optional()
    }

    pub fn board_cards_on(&self, hangout_id: &str, since: i64) -> rusqlite::Result<Vec<BoardCard>> {
        let sql = format!(
            "{CARD_SELECT} WHERE b.hangout_id = ?1 AND c.created_at > ?2 ORDER BY c.created_at, c.id"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![hangout_id, since], card_from_row)?;
        rows.collect()
    }

    pub fn board_card(&self, id: &str) -> rusqlite::Result<Option<BoardCard>> {
        self.conn()
            .query_row(&format!("{CARD_SELECT} WHERE c.id = ?"), [id], card_from_row)
            .optional()
    }

    /// None when it was saved. `"full"` at the cap, `"name"` when the floor already uses that name.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_board(
        &self,
        id: &str,
        hangout_id: &str,
        kind: &str,
        name: &str,
        created_by: &str,
        now: i64,
        max: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM boards WHERE hangout_id = ?",
            [hangout_id],
            |row| row.get(0),
        )?;
        if count >= max {
            return Ok(Some("full"));
        }
        let taken = tx
            .query_row(
                "SELECT 1 FROM boards WHERE hangout_id = ?1 AND name = ?2",
                params![hangout_id, name],
                |_| Ok(1_i64),
            )
            .optional()?
            .is_some();
        if taken {
            return Ok(Some("name"));
        }
        tx.execute(
            "INSERT INTO boards (id, hangout_id, kind, name, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, hangout_id, kind, name, created_by, now],
        )?;
        tx.commit()?;
        Ok(None)
    }

    /// False when another board on the same floor already has `name`.
    pub fn rename_board(&self, id: &str, name: &str) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let hangout_id: String =
            tx.query_row("SELECT hangout_id FROM boards WHERE id = ?", [id], |row| row.get(0))?;
        let taken = tx
            .query_row(
                "SELECT 1 FROM boards WHERE hangout_id = ?1 AND name = ?2 AND id <> ?3",
                params![hangout_id, name, id],
                |_| Ok(1_i64),
            )
            .optional()?
            .is_some();
        if taken {
            return Ok(false);
        }
        tx.execute("UPDATE boards SET name = ?1 WHERE id = ?2", params![name, id])?;
        tx.commit()?;
        Ok(true)
    }

    pub fn set_board_github(
        &self,
        id: &str,
        owner: Option<&str>,
        repo: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE boards SET github_owner = ?1, github_repo = ?2 WHERE id = ?3",
            params![owner, repo, id],
        )?;
        Ok(())
    }

    pub fn remove_board(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM boards WHERE id = ?", [id])?;
        Ok(())
    }

    /// None when it was saved. `"full"` at the cap, `"duplicate"` when that issue is already on the board.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_board_card(
        &self,
        id: &str,
        board_id: &str,
        title: &str,
        body: &str,
        status: Option<&str>,
        github_issue: Option<i64>,
        person_id: &str,
        agent_id: Option<&str>,
        via_agent: Option<&str>,
        now: i64,
        max: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM board_cards WHERE board_id = ?",
            [board_id],
            |row| row.get(0),
        )?;
        if count >= max {
            return Ok(Some("full"));
        }
        if let Some(number) = github_issue {
            let taken = tx
                .query_row(
                    "SELECT 1 FROM board_cards WHERE board_id = ?1 AND github_issue = ?2",
                    params![board_id, number],
                    |_| Ok(1_i64),
                )
                .optional()?
                .is_some();
            if taken {
                return Ok(Some("duplicate"));
            }
        }
        tx.execute(
            "INSERT INTO board_cards
                (id, board_id, title, body, status, github_issue, person_id, agent_id, via_agent, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
            params![id, board_id, title, body, status, github_issue, person_id, agent_id, via_agent, now],
        )?;
        tx.commit()?;
        Ok(None)
    }

    /// `status` and `github` are `None` to leave the column, `Some(None)` to clear it, `Some(Some)` to set it.
    /// None when it was saved. `"duplicate"` when that issue is already on the board, `"missing"` when the card is gone.
    pub fn save_board_card(
        &self,
        id: &str,
        title: Option<&str>,
        text: Option<&str>,
        status: Option<Option<&str>>,
        github: Option<Option<i64>>,
        now: i64,
    ) -> rusqlite::Result<Option<&'static str>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let current = tx
            .query_row(
                "SELECT title, body, status, github_issue, board_id FROM board_cards WHERE id = ?",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((old_title, old_body, old_status, old_issue, board_id)) = current else {
            return Ok(Some("missing"));
        };
        let title = title.map(str::to_string).unwrap_or(old_title);
        let body = text.map(str::to_string).unwrap_or(old_body);
        let status = match status {
            None => old_status,
            Some(value) => value.map(str::to_string),
        };
        let issue = match github {
            None => old_issue,
            Some(value) => value,
        };
        if let Some(number) = issue {
            let taken = tx
                .query_row(
                    "SELECT 1 FROM board_cards WHERE board_id = ?1 AND github_issue = ?2 AND id <> ?3",
                    params![board_id, number, id],
                    |_| Ok(1_i64),
                )
                .optional()?
                .is_some();
            if taken {
                return Ok(Some("duplicate"));
            }
        }
        tx.execute(
            "UPDATE board_cards SET title = ?1, body = ?2, status = ?3, github_issue = ?4, updated_at = ?5
             WHERE id = ?6",
            params![title, body, status, issue, now, id],
        )?;
        tx.commit()?;
        Ok(None)
    }

    pub fn remove_board_card(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM board_cards WHERE id = ?", [id])?;
        Ok(())
    }

    /// Adds a note. False, and nothing changes, if the item already holds `max` (I12).
    pub fn insert_item_note(&self, note: &NewItemNote, max: i64) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT INTO item_notes (id, item_id, body, person_id, via_agent, created_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6
             WHERE (SELECT COUNT(*) FROM item_notes WHERE item_id = ?2) < ?7",
            params![
                note.id,
                note.item_id,
                note.body,
                note.person_id,
                note.via_agent,
                note.now,
                max
            ],
        )?;
        Ok(changed > 0)
    }

    /// An item's notes written after `since`, oldest first. Times are whole seconds, so a guest's
    /// second of arrival is left out, as in `messages_since`.
    pub fn item_notes(&self, item_id: &str, since: i64) -> rusqlite::Result<Vec<ItemNote>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {ITEM_NOTE_COLS} {ITEM_NOTES} WHERE n.item_id = ? AND n.created_at > ?
             ORDER BY n.created_at, n.rowid"
        ))?;
        let rows = stmt.query_map(params![item_id, since], item_note_from_row)?;
        rows.collect()
    }

    pub fn item_note(&self, id: &str) -> rusqlite::Result<Option<ItemNote>> {
        self.conn()
            .query_row(
                &format!("SELECT {ITEM_NOTE_COLS} {ITEM_NOTES} WHERE n.id = ?"),
                [id],
                item_note_from_row,
            )
            .optional()
    }

    pub fn remove_item_note(&self, id: &str) -> rusqlite::Result<bool> {
        Ok(self.conn().execute("DELETE FROM item_notes WHERE id = ?", [id])? > 0)
    }

    /// How many notes each item on a floor holds, counting those written after `since`.
    pub fn item_note_counts(
        &self,
        hangout_id: &str,
        since: i64,
    ) -> rusqlite::Result<std::collections::HashMap<String, i64>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT n.item_id, COUNT(*) FROM item_notes n JOIN floor_items i ON i.id = n.item_id
             WHERE i.hangout_id = ? AND n.created_at > ? GROUP BY n.item_id",
        )?;
        let rows =
            stmt.query_map(params![hangout_id, since], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// I106: sets a closing clock's time and who it sends out.
    pub fn set_closing_clock(
        &self,
        clock: &ClosingClock,
        set_by: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO closing_clocks (item_id, hangout_id, closes_at, everyone, bot_ids, set_by, set_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (item_id) DO UPDATE SET closes_at = excluded.closes_at,
                everyone = excluded.everyone, bot_ids = excluded.bot_ids,
                set_by = excluded.set_by, set_at = excluded.set_at",
            params![
                clock.item_id,
                clock.hangout_id,
                clock.closes_at,
                clock.everyone,
                clock.bot_ids.join(","),
                set_by,
                now
            ],
        )?;
        Ok(())
    }

    /// I124: links a Lific board to a project, in place of any link it had.
    pub fn set_lific_link(
        &self,
        item_id: &str,
        hangout_id: &str,
        base: &str,
        project: &str,
        set_by: &str,
        at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO lific_links (item_id, hangout_id, base, project, set_by, set_at)
             VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (item_id) DO UPDATE SET hangout_id = excluded.hangout_id,
                 base = excluded.base, project = excluded.project,
                 set_by = excluded.set_by, set_at = excluded.set_at",
            params![item_id, hangout_id, base, project, set_by, at],
        )?;
        Ok(())
    }

    /// I129: the project made for this room: its origin, id, Lific's own id and whether it's public.
    pub fn lific_room_project(
        &self,
        hangout_id: &str,
    ) -> rusqlite::Result<Option<(String, String, String, bool)>> {
        self.conn()
            .query_row(
                "SELECT base, project, lific_id, published FROM lific_room_projects
                 WHERE hangout_id = ?",
                [hangout_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, i64>(3)? != 0)),
            )
            .optional()
    }

    /// I129: records the project made for this room, the moment Lific made it, so it's never made
    /// twice even if publishing it fails.
    pub fn set_lific_room_project(
        &self,
        hangout_id: &str,
        base: &str,
        project: &str,
        lific_id: &str,
        at: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO lific_room_projects (hangout_id, base, project, lific_id, made_at)
             VALUES (?, ?, ?, ?, ?)",
            params![hangout_id, base, project, lific_id, at],
        )?;
        Ok(())
    }

    /// I129: Lific has made the room's project public.
    pub fn lific_room_project_published(&self, hangout_id: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE lific_room_projects SET published = 1 WHERE hangout_id = ?",
            [hangout_id],
        )?;
        Ok(())
    }

    /// I137: records who added a card, once Lific has made it.
    pub fn add_lific_card(&self, card: &LificCard) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR REPLACE INTO lific_cards
                (hangout_id, card, issue_id, person_id, agent_id, via_bot, added_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                card.hangout_id,
                card.card,
                card.issue_id,
                card.person_id,
                card.agent_id,
                card.via_bot,
                card.added_at
            ],
        )?;
        Ok(())
    }

    /// I137: the cards this server added in a room, with who added them.
    pub fn lific_cards(&self, hangout_id: &str) -> rusqlite::Result<Vec<LificCard>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT c.hangout_id, c.card, c.issue_id, c.person_id, c.agent_id, c.via_bot,
                    c.added_at, COALESCE(a.name, people.name)
             FROM lific_cards c
             JOIN people ON people.id = c.person_id
             LEFT JOIN agents a ON a.id = c.agent_id
             WHERE c.hangout_id = ?",
        )?;
        let rows = stmt.query_map([hangout_id], |row| {
            Ok(LificCard {
                hangout_id: row.get(0)?,
                card: row.get(1)?,
                issue_id: row.get(2)?,
                person_id: row.get(3)?,
                agent_id: row.get(4)?,
                via_bot: row.get::<_, i64>(5)? != 0,
                added_at: row.get(6)?,
                added_by: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// I134, #651: how many cards a person, and their bots, asked to add since then, in every room,
    /// counting ones since taken down and ones still on their way.
    pub fn lific_cards_added_since(&self, person_id: &str, since: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM lific_card_adds WHERE person_id = ? AND at > ?",
            params![person_id, since],
            |row| row.get(0),
        )
    }

    /// I134, #651: reserves one add before Lific is asked, if it's within the day's `most_a_day` for
    /// this person and the room's `most_cards`, counting `on_board` (what the board last showed)
    /// and adds in this room still on their way. Checked and reserved together, so adds that race
    /// can't all pass. None when reserved; otherwise which limit it met.
    #[allow(clippy::too_many_arguments)]
    pub fn reserve_lific_add(
        &self,
        id: &str,
        person_id: &str,
        hangout_id: &str,
        on_board: i64,
        most_cards: i64,
        most_a_day: i64,
        day_start: i64,
        now: i64,
    ) -> rusqlite::Result<Option<LificLimit>> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let today: i64 = tx.query_row(
            "SELECT COUNT(*) FROM lific_card_adds WHERE person_id = ? AND at > ?",
            params![person_id, day_start],
            |row| row.get(0),
        )?;
        if today >= most_a_day {
            return Ok(Some(LificLimit::Day));
        }
        // An add that never came back from Lific stops counting toward the room after 10 minutes.
        let on_the_way: i64 = tx.query_row(
            "SELECT COUNT(*) FROM lific_card_adds WHERE hangout_id = ? AND pending = 1 AND at > ?",
            params![hangout_id, now - 600],
            |row| row.get(0),
        )?;
        if on_board + on_the_way >= most_cards {
            return Ok(Some(LificLimit::Board));
        }
        tx.execute(
            "INSERT INTO lific_card_adds (id, person_id, hangout_id, pending, at)
             VALUES (?, ?, ?, 1, ?)",
            params![id, person_id, hangout_id, now],
        )?;
        tx.commit()?;
        Ok(None)
    }

    /// #651: Lific made the card, so the add is no longer on its way; it still counts for the day.
    pub fn finish_lific_add(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("UPDATE lific_card_adds SET pending = 0 WHERE id = ?", [id])?;
        Ok(())
    }

    /// I139, #651: Lific didn't take it, so nothing is kept and it doesn't count.
    pub fn drop_lific_add(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM lific_card_adds WHERE id = ?", [id])?;
        Ok(())
    }

    /// I136: a card in Lific's trash is no one's here.
    pub fn remove_lific_card(&self, hangout_id: &str, card: &str) -> rusqlite::Result<()> {
        self.conn().execute(
            "DELETE FROM lific_cards WHERE hangout_id = ? AND card = ?",
            [hangout_id, card],
        )?;
        Ok(())
    }

    /// I124: unlinks a Lific board.
    pub fn clear_lific_link(&self, item_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM lific_links WHERE item_id = ?", [item_id])?;
        Ok(())
    }

    /// I124: a Lific board's origin and project, while it's in the room it was linked in.
    pub fn lific_link(
        &self,
        item_id: &str,
        hangout_id: &str,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT base, project FROM lific_links WHERE item_id = ? AND hangout_id = ?",
                [item_id, hangout_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// I106: takes the time off a closing clock.
    pub fn clear_closing_clock(&self, item_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM closing_clocks WHERE item_id = ?", [item_id])?;
        Ok(())
    }

    /// I106: the closing clocks with a time set on one floor.
    pub fn closing_clocks_on(&self, hangout_id: &str) -> rusqlite::Result<Vec<ClosingClock>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT item_id, hangout_id, closes_at, everyone, bot_ids FROM closing_clocks
             WHERE hangout_id = ?",
        )?;
        let rows = stmt.query_map([hangout_id], closing_clock_from_row)?;
        rows.collect()
    }

    /// I106: the closing clocks whose time has come.
    pub fn due_closing_clocks(&self, now: i64) -> rusqlite::Result<Vec<ClosingClock>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT item_id, hangout_id, closes_at, everyone, bot_ids FROM closing_clocks
             WHERE closes_at <= ?",
        )?;
        let rows = stmt.query_map([now], closing_clock_from_row)?;
        rows.collect()
    }

    /// I106: everyone walked onto a floor and still there, as `(who, bot's id if a bot)`.
    pub fn floor_walkers(
        &self,
        hangout_id: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<(String, Option<String>)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT v.who, v.agent_id FROM floor_visits v
             WHERE v.hangout_id = ?1 AND v.until > ?2
               AND NOT EXISTS (SELECT 1 FROM members m
                               WHERE m.hangout_id = v.hangout_id AND m.person_id = v.person_id)
               AND NOT EXISTS (SELECT 1 FROM room_agents r
                               WHERE r.hangout_id = v.hangout_id AND r.agent_id = v.agent_id)",
        )?;
        let rows =
            stmt.query_map(params![hangout_id, now], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// I10: turns moods on or off for a bot.
    pub fn set_bot_moods(&self, agent_id: &str, on: bool) -> rusqlite::Result<()> {
        let sql = if on {
            "DELETE FROM bots_without_moods WHERE agent_id = ?"
        } else {
            "INSERT OR IGNORE INTO bots_without_moods (agent_id) VALUES (?)"
        };
        self.conn().execute(sql, [agent_id])?;
        Ok(())
    }

    pub fn bot_has_moods(&self, agent_id: &str) -> rusqlite::Result<bool> {
        Ok(!exists(
            &self.conn(),
            "SELECT 1 FROM bots_without_moods WHERE agent_id = ?",
            [agent_id],
        )?)
    }

    /// Which of this person's bots have moods turned off, for their own page.
    pub fn bots_without_moods(&self, person_id: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.agent_id FROM bots_without_moods m JOIN agents a ON a.id = m.agent_id
             WHERE a.owner_id = ?",
        )?;
        let rows = stmt.query_map([person_id], |row| row.get(0))?;
        rows.collect()
    }

    /// C5: one change to a look, all of it or none. `None` clears a part.
    pub fn change_look(
        &self,
        wearer: Wearer,
        id: &str,
        change: &[(&str, Option<&str>)],
    ) -> rusqlite::Result<()> {
        let (table, column) = wearer.table();
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        for (part, choice) in change {
            match choice {
                Some(choice) => tx.execute(
                    &format!(
                        "INSERT INTO {table} ({column}, part, choice) VALUES (?, ?, ?)
                         ON CONFLICT ({column}, part) DO UPDATE SET choice = excluded.choice"
                    ),
                    params![id, part, choice],
                )?,
                None => tx.execute(
                    &format!("DELETE FROM {table} WHERE {column} = ? AND part = ?"),
                    params![id, part],
                )?,
            };
        }
        tx.commit()
    }

    /// C4: what's stored for each of these, by id. Anyone with nothing stored isn't in it.
    pub fn looks(
        &self,
        wearer: Wearer,
        ids: &[String],
    ) -> rusqlite::Result<BTreeMap<String, BTreeMap<String, String>>> {
        let (table, column) = wearer.table();
        let conn = self.conn();
        let mut stmt =
            conn.prepare(&format!("SELECT part, choice FROM {table} WHERE {column} = ?"))?;
        let mut looks = BTreeMap::new();
        for id in ids {
            let look = stmt
                .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<BTreeMap<String, String>>>()?;
            if !look.is_empty() {
                looks.insert(id.clone(), look);
            }
        }
        Ok(looks)
    }

    /// C16: the name behind each figure and its owner's colour, by id.
    pub fn figures(
        &self,
        wearer: Wearer,
        ids: &[String],
    ) -> rusqlite::Result<BTreeMap<String, (String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(match wearer {
            Wearer::Person => "SELECT name, color FROM people WHERE id = ?",
            Wearer::Bot => {
                "SELECT a.name, p.color FROM agents a JOIN people p ON p.id = a.owner_id WHERE a.id = ?"
            }
        })?;
        let mut figures = BTreeMap::new();
        for id in ids {
            let figure = stmt.query_row([id], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
            if let Some(figure) = figure {
                figures.insert(id.clone(), figure);
            }
        }
        Ok(figures)
    }

    /// FR6: adds an invite, or wakes one that lapsed before `lapsed`. False if nothing changed, as
    /// when one is already waiting or they said no.
    #[allow(clippy::too_many_arguments)]
    pub fn add_friend_invite(
        &self,
        id: &str,
        kind: &str,
        place_id: &str,
        from_person: &str,
        to_person: &str,
        now: i64,
        lapsed: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let changed = tx.execute(
            "INSERT INTO friend_invites (id, kind, place_id, from_person, to_person, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (kind, place_id, to_person) DO UPDATE
               SET from_person = excluded.from_person, created_at = excluded.created_at
               WHERE declined_at IS NULL AND created_at <= ?7",
            params![id, kind, place_id, from_person, to_person, now, lapsed],
        )?;
        // FR6: sending again brings back one that ended. It isn't new, so nobody is pinged.
        tx.execute(
            "DELETE FROM friend_invite_ends WHERE invite_id IN
               (SELECT id FROM friend_invites
                WHERE kind = ? AND place_id = ? AND to_person = ? AND declined_at IS NULL)",
            params![kind, place_id, to_person],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn friend_invites_sent_since(&self, person_id: &str, since: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM friend_invites WHERE from_person = ? AND created_at > ?",
            params![person_id, since],
            |row| row.get(0),
        )
    }

    /// FR5: invites still waiting: not turned down, not ended, not lapsed, to a place the sender is
    /// still in and the person isn't, with that place's link as it is now.
    pub fn friend_invites_for(
        &self,
        person_id: &str,
        since: i64,
        limit: i64,
    ) -> rusqlite::Result<Vec<FriendInvite>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT f.id, f.kind, p.name, COALESCE(h.name, c.name), COALESCE(h.invite_code, c.invite_code)
             FROM friend_invites f
             JOIN people p ON p.id = f.from_person
             LEFT JOIN hangouts h ON f.kind = 'room' AND h.id = f.place_id
             LEFT JOIN cities c ON f.kind = 'city' AND c.id = f.place_id
             WHERE f.to_person = ?1 AND f.declined_at IS NULL AND f.created_at > ?2
               AND NOT EXISTS (SELECT 1 FROM friend_invite_ends e WHERE e.invite_id = f.id)
               AND NOT EXISTS (SELECT 1 FROM quiet_asks q WHERE q.kind = 'friend_invite' AND q.ask_id = f.id)
               AND NOT EXISTS (SELECT 1 FROM floors fl JOIN turned_out t ON t.space_id = fl.space_id
                               WHERE fl.hangout_id = f.place_id AND t.person_id = ?1)
               AND (h.id IS NOT NULL OR c.id IS NOT NULL)
               AND NOT EXISTS (SELECT 1 FROM members m WHERE m.hangout_id = f.place_id AND m.person_id = ?1)
               AND NOT EXISTS (SELECT 1 FROM city_members m WHERE m.city_id = f.place_id AND m.person_id = ?1)
               AND (EXISTS (SELECT 1 FROM members m WHERE m.hangout_id = f.place_id AND m.person_id = f.from_person)
                    OR EXISTS (SELECT 1 FROM city_members m WHERE m.city_id = f.place_id AND m.person_id = f.from_person))
             ORDER BY f.created_at DESC, f.id LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![person_id, since, limit], |row| {
            Ok(FriendInvite {
                id: row.get(0)?,
                kind: row.get(1)?,
                from_name: row.get(2)?,
                place_name: row.get(3)?,
                code: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// True if there was an invite to this person, not already turned down.
    pub fn decline_friend_invite(
        &self,
        id: &str,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE friend_invites SET declined_at = ?
             WHERE id = ? AND to_person = ? AND declined_at IS NULL",
            params![now, id, person_id],
        )?;
        Ok(changed > 0)
    }

    /// Whether a person's own player has moved in a world since `since`.
    pub fn own_player_is_active(
        &self,
        person_id: &str,
        world_id: &str,
        since: i64,
    ) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM players p JOIN world_players w ON w.player_id = p.id
             WHERE p.person_id = ? AND p.token_hash IS NULL AND p.agent_id IS NULL AND p.removed = 0
               AND w.world_id = ? AND w.seen_at >= ?",
            params![person_id, world_id, since],
        )
    }

    /// Who is in a world, for a window of the map: those who moved here since `since` and stand
    /// inside the box `[x0, y0, x1, y1]`.
    pub fn world_players_in(
        &self,
        world_id: &str,
        since: i64,
        region: [i64; 4],
        limit: i64,
    ) -> rusqlite::Result<Vec<WorldPlayer>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {WORLD_PLAYER_COLS}
             FROM world_players w JOIN players p ON p.id = w.player_id
             WHERE w.world_id = ?1 AND w.seen_at >= ?2 AND p.removed = 0
               AND w.x BETWEEN ?3 AND ?4 AND w.y BETWEEN ?5 AND ?6
             ORDER BY w.seen_at DESC, p.id LIMIT ?7",
        ))?;
        let rows = stmt.query_map(
            params![world_id, since, region[0], region[2], region[1], region[3], limit],
            world_player_from_row,
        )?;
        rows.collect()
    }

    // Asking to let a bot in (S82, S50 to S52)

    /// S52: the latest request for this bot at this space since `since` that's waiting or was turned
    /// down: its id and status.
    pub fn place_request_for(
        &self,
        space_id: &str,
        agent_id: &str,
        since: i64,
    ) -> rusqlite::Result<Option<(String, String)>> {
        self.conn()
            .query_row(
                "SELECT id, status FROM place_requests
                 WHERE space_id = ? AND agent_id = ? AND status IN ('waiting', 'no')
                   AND COALESCE(answered_at, created_at) > ?
                 ORDER BY created_at DESC LIMIT 1",
                params![space_id, agent_id, since],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// S50: adds a request unless this person has `max` waiting since `since`. False if they have.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_place_request(
        &self,
        id: &str,
        space_id: &str,
        agent_id: &str,
        asked_by: &str,
        now: i64,
        since: i64,
        max: i64,
    ) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "INSERT INTO place_requests (id, space_id, agent_id, asked_by, status, created_at)
             SELECT ?1, ?2, ?3, ?4, 'waiting', ?5
             WHERE (SELECT COUNT(*) FROM place_requests
                    WHERE asked_by = ?4 AND status = 'waiting' AND created_at > ?6) < ?7",
            params![id, space_id, agent_id, asked_by, now, since, max],
        )?;
        Ok(changed > 0)
    }

    /// A request still waiting since `since`: `(space_id, agent_id, asked_by)`.
    pub fn waiting_place_request(
        &self,
        id: &str,
        since: i64,
    ) -> rusqlite::Result<Option<(String, String, String)>> {
        self.conn()
            .query_row(
                "SELECT space_id, agent_id, asked_by FROM place_requests
                 WHERE id = ? AND status = 'waiting' AND created_at > ?",
                params![id, since],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    /// S51: the requests waiting on this owner since `since`, oldest first:
    /// `(id, space name, bot name, asker's name, created_at)`.
    pub fn place_requests_for_owner(
        &self,
        owner_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<PlaceRequestRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT r.id, r.space_id, s.name, a.name, p.name, r.created_at FROM place_requests r
             JOIN spaces s ON s.id = r.space_id
             JOIN agents a ON a.id = r.agent_id
             JOIN people p ON p.id = r.asked_by
             WHERE s.owner_id = ? AND r.status = 'waiting' AND r.created_at > ?
             ORDER BY r.created_at, r.id",
        )?;
        let rows = stmt.query_map(params![owner_id, since], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
        })?;
        rows.collect()
    }

    /// False if it was answered already.
    pub fn answer_place_request(&self, id: &str, status: &str, now: i64) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE place_requests SET status = ?, answered_at = ? WHERE id = ? AND status = 'waiting'",
            params![status, now, id],
        )?;
        Ok(changed > 0)
    }

    /// W71: every place in a world with its owner's name, `(x, y, name)`.
    /// W73: with its kind, and the name of the space behind its door if its owner has been through
    /// and it still stands as that space's door (S33): `(x, y, owner, kind, name)`.
    pub fn place_owners(&self, world_id: &str) -> rusqlite::Result<Vec<PlaceOwnerRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT b.x, b.y, pe.name, b.kind,
                    (SELECT s.name FROM space_plots d JOIN spaces s ON s.id = d.space_id
                     WHERE d.world_id = b.world_id AND d.x = b.x AND d.y = b.y
                       AND s.owner_id = pl.person_id AND (b.kind = 'house') = (s.kind = 'house'))
             FROM world_every_place b
             JOIN players pl ON pl.id = b.player_id
             JOIN people pe ON pe.id = pl.person_id
             WHERE b.world_id = ?1
             ORDER BY b.y, b.x",
        )?;
        let rows = stmt.query_map([world_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?;
        rows.collect()
    }

    /// What building stands on a tile and the person it belongs to, for the door to its room. Not
    /// for anything public: only W71 says whose a place is, by name.
    /// L22: a project's site, where its studio will go once a road reaches it. One a project.
    pub fn set_project_site(
        &self,
        space_id: &str,
        x: i64,
        y: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO project_sites (space_id, x, y, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (space_id) DO UPDATE SET x = excluded.x, y = excluded.y, created_at = excluded.created_at",
            params![space_id, x, y, now],
        )?;
        Ok(())
    }

    pub fn project_site(&self, space_id: &str) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn()
            .query_row("SELECT x, y FROM project_sites WHERE space_id = ?", [space_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
    }

    pub fn clear_project_site(&self, space_id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM project_sites WHERE space_id = ?", [space_id])?;
        Ok(())
    }

    /// L62: the player a bot plays The Commons with, if it has one.
    pub fn player_of_agent(&self, agent_id: &str) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT id FROM players WHERE agent_id = ? AND removed = 0",
                [agent_id],
                |row| row.get(0),
            )
            .optional()
    }

    /// L62: where a person's house stands in a world, if it does.
    pub fn house_of_person(
        &self,
        world_id: &str,
        person_id: &str,
    ) -> rusqlite::Result<Option<(i64, i64)>> {
        self.conn()
            .query_row(
                "SELECT b.x, b.y FROM world_buildings b JOIN players p ON p.id = b.player_id
                 WHERE b.world_id = ?1 AND b.kind = 'house' AND p.person_id = ?2
                   AND p.agent_id IS NULL AND p.token_hash IS NULL",
                params![world_id, person_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// W91: the tile a place is stored on, for any tile of a big one. Any other tile is itself.
    pub fn place_anchor_at(&self, world_id: &str, x: i64, y: i64) -> rusqlite::Result<(i64, i64)> {
        Ok(self
            .conn()
            .query_row(
                "SELECT x, y FROM world_every_place
                 WHERE world_id = ?1 AND kind IN ('square', 'green')
                   AND x BETWEEN ?2 - 1 AND ?2 AND y BETWEEN ?3 - 1 AND ?3
                 ORDER BY x DESC, y DESC LIMIT 1",
                params![world_id, x, y],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or((x, y)))
    }

    pub fn building_at(
        &self,
        world_id: &str,
        x: i64,
        y: i64,
    ) -> rusqlite::Result<Option<(String, Option<String>)>> {
        self.conn()
            .query_row(
                "SELECT b.kind, p.person_id FROM world_every_place b
                 JOIN players p ON p.id = b.player_id
                 WHERE b.world_id = ?1 AND b.x = ?2 AND b.y = ?3",
                params![world_id, x, y],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    /// E6: the place standing on a tile: its kind, the player who put it up, and the person that player
    /// belongs to (a project's studio is put up through its owner's own player, L13).
    pub fn place_standing(
        &self,
        world_id: &str,
        x: i64,
        y: i64,
    ) -> rusqlite::Result<Option<(String, i64, Option<String>)>> {
        self.conn()
            .query_row(
                "SELECT b.kind, b.player_id, p.person_id FROM world_every_place b
                 JOIN players p ON p.id = b.player_id
                 WHERE b.world_id = ?1 AND b.x = ?2 AND b.y = ?3",
                params![world_id, x, y],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    /// E31: the outside stored for a tile, only while the place standing on it has the same player and
    /// kind as the row.
    pub fn outside_at(&self, world_id: &str, x: i64, y: i64) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT o.outside FROM place_outsides o {OUTSIDE_IS_LIVE}
                     WHERE o.world_id = ?1 AND o.x = ?2 AND o.y = ?3"
                ),
                params![world_id, x, y],
                |row| row.get(0),
            )
            .optional()
    }

    /// E25: stores a checked outside for the place on a tile. It replaces the one there, and any
    /// outside the player has waiting beyond the newest few is dropped (E30).
    pub fn save_outside(
        &self,
        world_id: &str,
        player_id: i64,
        kind: &str,
        (x, y): (i64, i64),
        outside: &str,
        now: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let stamp = next_outside_stamp(&tx, world_id)?;
        tx.execute(
            "INSERT INTO place_outsides (world_id, player_id, kind, x, y, outside, stamp, changed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (world_id, player_id, kind, x, y) DO UPDATE SET
                 outside = excluded.outside, stamp = excluded.stamp, changed_at = excluded.changed_at",
            params![world_id, player_id, kind, x, y, outside, stamp, now],
        )?;
        let waiting = "NOT EXISTS (SELECT 1 FROM world_every_place p
                                    WHERE p.world_id = o.world_id AND p.x = o.x AND p.y = o.y
                                      AND p.player_id = o.player_id AND p.kind = o.kind)";
        tx.execute(
            &format!(
                "DELETE FROM place_outsides WHERE rowid IN (
                     SELECT o.rowid FROM place_outsides o
                      WHERE o.world_id = ?1 AND o.player_id = ?2 AND o.kind = ?3 AND {waiting}
                        AND o.rowid NOT IN (
                            SELECT o.rowid FROM place_outsides o
                             WHERE o.world_id = ?1 AND o.player_id = ?2 AND o.kind = ?3 AND {waiting}
                             ORDER BY o.changed_at DESC, o.stamp DESC LIMIT ?4))"
            ),
            params![world_id, player_id, kind, MAX_WAITING_OUTSIDES],
        )?;
        tx.commit()
    }

    /// E1, E7: takes away what's stored for the place on a tile. Whether there was anything.
    pub fn clear_outside(
        &self,
        world_id: &str,
        player_id: i64,
        kind: &str,
        (x, y): (i64, i64),
    ) -> rusqlite::Result<bool> {
        let removed = self.conn().execute(
            "DELETE FROM place_outsides
             WHERE world_id = ?1 AND player_id = ?2 AND kind = ?3 AND x = ?4 AND y = ?5",
            params![world_id, player_id, kind, x, y],
        )?;
        Ok(removed > 0)
    }

    /// E27: `[x, y, document]` for every outside that has its place, in the order of the map.
    pub fn live_outsides(&self, world_id: &str) -> rusqlite::Result<Vec<(i64, i64, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&format!(
            "SELECT o.x, o.y, o.outside FROM place_outsides o {OUTSIDE_IS_LIVE}
             WHERE o.world_id = ?1 ORDER BY o.y, o.x"
        ))?;
        let rows = stmt.query_map([world_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    }

    /// E26: `(count, stamp)` of what `live_outsides` would give, without reading any document. Every
    /// row has a stamp nobody else has, and a stamp goes up with every change, a place going up
    /// included, so anything that changes E27's answer changes one of these.
    pub fn outsides_stamp(&self, world_id: &str) -> rusqlite::Result<OutsideStamp> {
        self.conn().query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(o.stamp), 0) FROM place_outsides o {OUTSIDE_IS_LIVE}
                 WHERE o.world_id = ?1"
            ),
            [world_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    /// E27: the map of outsides as it was last built, if that was for this stamp and not long ago.
    pub fn kept_outside_map(&self, stamp: OutsideStamp, longest: Duration) -> Option<Arc<String>> {
        let kept = self.outside_map.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        kept.as_ref()
            .filter(|(built_for, at, _)| *built_for == stamp && at.elapsed() < longest)
            .map(|(_, _, body)| body.clone())
    }

    /// E27: keeps an answer built after reading `stamp`, so a change since only makes the next one stale.
    pub fn keep_outside_map(&self, stamp: OutsideStamp, body: Arc<String>) {
        *self.outside_map.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some((stamp, Instant::now(), body));
    }

    /// How many rows a player holds in a world, waiting or not.
    pub fn outside_rows(&self, world_id: &str, player_id: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM place_outsides WHERE world_id = ?1 AND player_id = ?2",
            params![world_id, player_id],
            |row| row.get(0),
        )
    }

    /// Everything a player has built in a world. It reads their tiles only, by the index on world
    /// and player, so it costs what they have built and not what the world holds.
    pub fn player_tiles(&self, world_id: &str, player_id: i64) -> rusqlite::Result<Tiles> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT x, y, kind, level, player_id FROM world_tiles WHERE world_id = ?1 AND player_id = ?2
             UNION ALL SELECT x, y, kind, 0, player_id FROM world_every_place
                       WHERE world_id = ?1 AND player_id = ?2",
        )?;
        let mut rows = stmt.query(params![world_id, player_id])?;
        let mut tiles = Tiles::new();
        while let Some(row) = rows.next()? {
            if let Some((pos, tile)) = tile_from_row(row)? {
                tiles.insert(pos, tile);
            }
        }
        Ok(tiles)
    }

    /// W97: only this builder's losses, newest first, with bounded response size.
    pub fn road_removals(
        &self,
        world_id: &str,
        player_id: i64,
    ) -> rusqlite::Result<Vec<RoadRemoval>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT x, y, built_at, removed_at, world_step, reason FROM world_road_removals
             WHERE world_id = ?1 AND player_id = ?2 ORDER BY id DESC LIMIT 50",
        )?;
        let rows = stmt.query_map(params![world_id, player_id], |row| {
            Ok(RoadRemoval {
                x: row.get(0)?,
                y: row.get(1)?,
                built_at: row.get(2)?,
                removed_at: row.get(3)?,
                world_step: row.get(4)?,
                reason: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    // Players

    pub fn player(&self, id: i64) -> rusqlite::Result<Option<PlayerRow>> {
        q_player(&self.conn(), id)
    }

    /// The player a token belongs to, unless they were removed.
    pub fn player_by_token(&self, token_hash: &str) -> rusqlite::Result<Option<PlayerRow>> {
        self.conn()
            .query_row(
                &format!("SELECT {PLAYER_COLS} FROM players WHERE token_hash = ? AND removed = 0"),
                [token_hash],
                player_row_from_row,
            )
            .optional()
    }

    pub fn players_of(&self, person_id: &str) -> rusqlite::Result<Vec<PlayerRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PLAYER_COLS} FROM players WHERE person_id = ? AND removed = 0 ORDER BY id"
        ))?;
        let rows = stmt.query_map([person_id], player_row_from_row)?;
        rows.collect()
    }

    /// A public visitor is anonymous and has no claim credential (W11).
    pub fn create_anonymous_guest(
        &self,
        token_hash: &str,
        now: i64,
    ) -> rusqlite::Result<PlayerRow> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO players (name_seed, token_hash, seen_at, created_at) VALUES (?, ?, ?, ?)",
            params![random_name_seed(), token_hash, now, now],
        )?;
        let player =
            q_player(&tx, tx.last_insert_rowid())?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(player)
    }

    /// Historical guest fixture/import support. Public joins use `create_anonymous_guest`.
    pub fn create_guest(
        &self,
        token_hash: &str,
        claim_hash: &str,
        now: i64,
    ) -> rusqlite::Result<PlayerRow> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO players (name_seed, token_hash, claim_hash, seen_at, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![random_name_seed(), token_hash, claim_hash, now, now],
        )?;
        let player =
            q_player(&tx, tx.last_insert_rowid())?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(player)
    }

    /// The player for a bot in `world_id`: made the first time, and after that given a new token.
    /// Refused, with its token left as it was, if that player was removed or is barred from the
    /// world.
    pub fn join_as_bot(
        &self,
        agent_id: &str,
        owner_id: &str,
        token_hash: &str,
        world_id: &str,
        now: i64,
    ) -> rusqlite::Result<BotJoined> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let existing: Option<(i64, bool)> = tx
            .query_row("SELECT id, removed FROM players WHERE agent_id = ?", [agent_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let id = match existing {
            Some((_, true)) => return Ok(BotJoined::Removed),
            Some((id, false)) => {
                if exists(
                    &tx,
                    "SELECT 1 FROM world_members WHERE world_id = ? AND player_id = ? AND banned = 1",
                    params![world_id, id],
                )? {
                    return Ok(BotJoined::Barred);
                }
                tx.execute(
                    "UPDATE players SET token_hash = ?, seen_at = ? WHERE id = ?",
                    params![token_hash, now, id],
                )?;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO players (name_seed, token_hash, person_id, agent_id, seen_at, created_at)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![random_name_seed(), token_hash, owner_id, agent_id, now, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        let player = q_player(&tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(BotJoined::Done(player))
    }

    /// Whether a signed-in person has their own player yet. This makes nothing.
    pub fn has_own_player(&self, person_id: &str) -> rusqlite::Result<bool> {
        exists(
            &self.conn(),
            "SELECT 1 FROM players WHERE person_id = ? AND token_hash IS NULL AND agent_id IS NULL",
            [person_id],
        )
    }

    /// A signed-in person's own player, made the first time they need it.
    pub fn own_player(&self, person_id: &str, now: i64) -> rusqlite::Result<PlayerRow> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM players WHERE person_id = ? AND token_hash IS NULL AND agent_id IS NULL",
                [person_id],
                |row| row.get(0),
            )
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO players (name_seed, person_id, seen_at, created_at) VALUES (?, ?, ?, ?)",
                    params![random_name_seed(), person_id, now, now],
                )?;
                tx.last_insert_rowid()
            }
        };
        let player = q_player(&tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(player)
    }

    pub fn touch_player(&self, id: i64, now: i64) -> rusqlite::Result<()> {
        self.conn().execute("UPDATE players SET seen_at = ? WHERE id = ?", params![now, id])?;
        Ok(())
    }

    pub fn set_name_seed(&self, id: i64, name_seed: i64) -> rusqlite::Result<()> {
        self.conn()
            .execute("UPDATE players SET name_seed = ? WHERE id = ?", params![name_seed, id])?;
        Ok(())
    }

    /// Hands a guest to a person, once. Refused if the code is unknown or already used, if the
    /// guest has been idle since before `earliest`, or if the person already holds `max`.
    pub fn claim_player(
        &self,
        claim_hash: &str,
        person_id: &str,
        max: i64,
        earliest: i64,
    ) -> rusqlite::Result<Claimed> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM players WHERE claim_hash = ? AND removed = 0 AND person_id IS NULL
                   AND agent_id IS NULL AND seen_at >= ?",
                params![claim_hash, earliest],
                |row| row.get(0),
            )
            .optional()?;
        let Some(id) = found else {
            return Ok(Claimed::NoSuchCode);
        };
        let held: i64 = tx.query_row(
            "SELECT COUNT(*) FROM players WHERE person_id = ? AND token_hash IS NOT NULL
               AND agent_id IS NULL AND removed = 0",
            [person_id],
            |row| row.get(0),
        )?;
        if held >= max {
            return Ok(Claimed::TooMany);
        }
        tx.execute(
            "UPDATE players SET person_id = ?, claim_hash = NULL WHERE id = ?",
            params![person_id, id],
        )?;
        let player = q_player(&tx, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        tx.commit()?;
        Ok(Claimed::Done(player))
    }

    /// Ends a player its person claimed: the token stops and it leaves every street, and what it
    /// built stays. False if the person has no such player.
    pub fn end_claimed_player(&self, player_id: i64, person_id: &str) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let ended = tx.execute(
            "UPDATE players SET removed = 1 WHERE id = ? AND person_id = ? AND removed = 0
               AND token_hash IS NOT NULL AND agent_id IS NULL",
            params![player_id, person_id],
        )? == 1;
        if ended {
            tx.execute("DELETE FROM world_players WHERE player_id = ?", [player_id])?;
        }
        tx.commit()?;
        Ok(ended)
    }

    /// Takes a player out of a world: their tiles there come down, and they leave its street.
    /// `ban` stops them building there again. `everywhere` also ends the player for good: their
    /// token stops and their claim code with it. Returns how many tiles came down.
    pub fn remove_from_world(
        &self,
        world_id: &str,
        player_id: i64,
        ban: bool,
        everywhere: bool,
    ) -> rusqlite::Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO world_road_removals
                (world_id, player_id, x, y, built_at, removed_at, world_step, reason)
             SELECT t.world_id, t.player_id, t.x, t.y, t.built_at, ?3, w.step, 'player_removed'
             FROM world_tiles t JOIN worlds w ON w.id = t.world_id
             WHERE t.world_id = ?1 AND t.player_id = ?2 AND t.kind = 'road'
               AND EXISTS (SELECT 1 FROM players p WHERE p.id = t.player_id)",
            params![world_id, player_id, crate::now()],
        )?;
        // W113
        q_log_player_removals(
            &tx,
            "p.world_id = ?1 AND p.player_id = ?2",
            params![world_id, player_id],
            true,
            crate::now(),
            "player_removed",
        )?;
        let down = tx.execute(
            "DELETE FROM world_tiles WHERE world_id = ? AND player_id = ?",
            params![world_id, player_id],
        )? + tx.execute(
            "DELETE FROM world_buildings WHERE world_id = ? AND player_id = ?",
            params![world_id, player_id],
        )? + tx.execute(
            "DELETE FROM world_squares WHERE world_id = ? AND player_id = ?",
            params![world_id, player_id],
        )? + tx.execute(
            "DELETE FROM world_more_places WHERE world_id = ? AND player_id = ?",
            params![world_id, player_id],
        )?;
        tx.execute(
            "DELETE FROM world_players WHERE world_id = ? AND player_id = ?",
            params![world_id, player_id],
        )?;
        // E32: what they made on those tiles goes with them.
        tx.execute(
            "DELETE FROM place_outsides WHERE world_id = ? AND player_id = ?",
            params![world_id, player_id],
        )?;
        if ban {
            tx.execute(
                "INSERT INTO world_members (world_id, player_id, banned) VALUES (?, ?, 1)
                 ON CONFLICT (world_id, player_id) DO UPDATE SET banned = 1",
                params![world_id, player_id],
            )?;
        }
        if everywhere {
            tx.execute(
                "UPDATE players SET removed = 1, claim_hash = NULL WHERE id = ?",
                [player_id],
            )?;
        }
        tx.commit()?;
        Ok(down)
    }

    /// The worlds a player is standing in.
    pub fn worlds_of_player(&self, player_id: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT world_id FROM world_players WHERE player_id = ?")?;
        let rows = stmt.query_map([player_id], |row| row.get(0))?;
        rows.collect()
    }

    /// Whether a player has any dealings with a world: standing in it, a member, or a builder there.
    pub fn player_known_in(&self, world_id: &str, player_id: i64) -> rusqlite::Result<bool> {
        let conn = self.conn();
        exists(
            &conn,
            "SELECT 1 FROM world_players WHERE world_id = ?1 AND player_id = ?2
             UNION ALL SELECT 1 FROM world_members WHERE world_id = ?1 AND player_id = ?2
             UNION ALL SELECT 1 FROM world_tiles WHERE world_id = ?1 AND player_id = ?2
             UNION ALL SELECT 1 FROM world_every_place WHERE world_id = ?1 AND player_id = ?2 LIMIT 1",
            params![world_id, player_id],
        )
    }

    /// W113: the newest places that came down on a map, newest first, with whose they were.
    pub fn place_removals(
        &self,
        world_id: &str,
        limit: i64,
    ) -> rusqlite::Result<Vec<serde_json::Value>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT r.x, r.y, r.kind, pe.name, r.built_at, r.removed_at, r.reason
             FROM world_place_removals r LEFT JOIN people pe ON pe.id = r.person_id
             WHERE r.world_id = ?1 ORDER BY r.id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![world_id, limit], |row| {
            Ok(serde_json::json!({
                "x": row.get::<_, i64>(0)?,
                "y": row.get::<_, i64>(1)?,
                "kind": row.get::<_, String>(2)?,
                "owner": row.get::<_, Option<String>>(3)?,
                "built_at": crate::ops::iso(row.get(4)?),
                "removed_at": crate::ops::iso(row.get(5)?),
                "reason": row.get::<_, String>(6)?,
            }))
        })?;
        rows.collect()
    }

    /// Deletes guests nobody has claimed that haven't been seen since `before`, so a swarm of them
    /// can't fill the table for good. Their roads stay, with a builder nothing leads back from; a
    /// place of theirs goes with them (its player_id cascades), and W113 logs it.
    pub fn prune_guests(&self, before: i64) -> rusqlite::Result<usize> {
        let conn = self.conn();
        // KA5: a keyless agent that wrote something keeps its row, so its name and marks stay with
        // what it wrote, and loses its key.
        let keyless = conn.execute(
            "UPDATE players SET token_hash = NULL WHERE token_hash IS NOT NULL AND person_id IS NULL
               AND agent_id IS NULL AND seen_at < ?
               AND (id IN (SELECT player_id FROM keyless_messages)
                    OR id IN (SELECT player_id FROM keyless_cards))",
            [before],
        )?;
        q_log_player_removals(
            &conn,
            "pl.token_hash IS NOT NULL AND pl.person_id IS NULL AND pl.agent_id IS NULL
               AND pl.seen_at < ?1
               AND pl.id NOT IN (SELECT player_id FROM keyless_messages)
               AND pl.id NOT IN (SELECT player_id FROM keyless_cards)",
            params![before],
            false,
            crate::now(),
            "guest_pruned",
        )?;
        let pruned = conn.execute(
            "DELETE FROM players WHERE token_hash IS NOT NULL AND person_id IS NULL
               AND agent_id IS NULL AND seen_at < ?
               AND id NOT IN (SELECT player_id FROM keyless_messages)
               AND id NOT IN (SELECT player_id FROM keyless_cards)",
            [before],
        )?;
        Ok(keyless + pruned)
    }

    // Crawler sightings: a daily count per kind, and nothing else about who it was.

    pub fn add_sightings(
        &self,
        kind: &str,
        day: i64,
        visits: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO crawler_sightings (kind, day, visits, seen_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (kind, day) DO UPDATE SET visits = visits + excluded.visits,
                                                    seen_at = excluded.seen_at",
            params![kind, day, visits, now],
        )?;
        Ok(())
    }

    /// Kind, visits and when last seen, for one day.
    pub fn sightings_on(&self, day: i64) -> rusqlite::Result<Vec<(String, i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT kind, visits, seen_at FROM crawler_sightings WHERE day = ? ORDER BY kind",
        )?;
        let rows = stmt.query_map([day], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    }

    /// A108, A112: whether `person_id` accepted `version`, whether they accepted any other
    /// version, and when their account was made.
    pub fn rules_accepted(
        &self,
        person_id: &str,
        version: &str,
    ) -> rusqlite::Result<(bool, bool, Option<i64>)> {
        self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM rules_acceptances WHERE person_id = ?1 AND version = ?2),
                    EXISTS (SELECT 1 FROM rules_acceptances WHERE person_id = ?1 AND version <> ?2),
                    (SELECT created_at FROM people WHERE id = ?1)",
            params![person_id, version],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }

    /// A112: notes that this server is running with rules `version`, the first time only.
    pub fn note_rules_version(&self, version: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO rules_versions (version, first_seen) VALUES (?, ?)",
            params![version, now],
        )?;
        Ok(())
    }

    /// A112: when this server first ran with rules `version`.
    pub fn rules_version_first_seen(&self, version: &str) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row(
                "SELECT first_seen FROM rules_versions WHERE version = ?",
                [version],
                |row| row.get(0),
            )
            .optional()
    }

    /// A108: records that `person_id` accepted `version`. Accepting it again keeps the first time.
    pub fn accept_rules(&self, person_id: &str, version: &str, now: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT OR IGNORE INTO rules_acceptances (person_id, version, accepted_at)
             VALUES (?, ?, ?)",
            params![person_id, version, now],
        )?;
        Ok(())
    }

    // Deleting an account

    /// A27 to A45: everything `people` holds of someone. Their row goes, so every cascade runs, and
    /// a column added later without one makes this fail instead of keeping what it holds. It's one
    /// transaction, so if anything fails they still have their account.
    pub fn delete_person(&self, person_id: &str, name: &str) -> rusqlite::Result<AccountGone> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut gone = AccountGone::default();
        // A30: both values are random and thrown away, so neither key nor calendar link works.
        tx.execute(
            "INSERT OR IGNORE INTO people (id, name, color, key_hash, feed_token, created_at)
             VALUES (?, ?, '#8a8f98', lower(hex(randomblob(32))), lower(hex(randomblob(32))), 0)",
            params![SOMEONE_WHO_LEFT_ID, SOMEONE_WHO_LEFT],
        )?;
        // A103: marks on this person's asks, either side, go with them: those keyed by a row's id
        // or one of their bots', looked up while the rows are still here. Pair keys are below.
        tx.execute(
            "DELETE FROM quiet_asks WHERE ask_id IN (
                SELECT id FROM friend_invites WHERE from_person = ?1 OR to_person = ?1
                UNION SELECT id FROM check_ins WHERE asked_by = ?1 OR to_person = ?1
                UNION SELECT t.id FROM trips t JOIN agents a ON a.id = t.agent_id
                      WHERE a.owner_id = ?1 OR t.asked_by = ?1
                UNION SELECT id FROM apprentice_proposals
                      WHERE proposer_owner = ?1 OR partner_owner = ?1)
             OR EXISTS (SELECT 1 FROM agents a
                        WHERE a.owner_id = ?1 AND quiet_asks.ask_id LIKE '%' || a.id || '%')",
            [person_id],
        )?;
        pass_on_house_bots(&tx, person_id, &mut gone)?;
        // A house bot that goes with its house still has a name to take off the rest.
        let mut who = strings(&tx, "SELECT name FROM agents WHERE owner_id = ?", [person_id])?;
        who.push(name.to_string());
        decline_scenes_on_keys(
            &tx,
            "p.agent_id IN (SELECT id FROM agents WHERE owner_id = ?)",
            [person_id],
        )?;
        gone.other_rooms.extend(strings(
            &tx,
            "SELECT hangout_id FROM floor_visits WHERE person_id = ?1
             UNION SELECT h.porch FROM trips t JOIN houses h ON h.id = t.house_id
                   JOIN agents a ON a.id = t.agent_id
                   WHERE a.owner_id = ?1 AND t.status = 'here'",
            [person_id],
        )?);
        remove_empty_households(&tx, person_id, &mut gone)?;
        remove_empty_rooms(&tx, person_id, &mut gone)?;

        remove_their_words(&tx, person_id, &who)?;
        // A38: other bots' event copies quote what this person said. Their own rows go with the
        // agent, and these name them with no foreign key.
        let mut needles = who.clone();
        needles.push(person_id.to_string());
        needles.extend(strings(&tx, "SELECT id FROM agents WHERE owner_id = ?", [person_id])?);
        needles.extend(strings(
            &tx,
            "SELECT url FROM notify_targets WHERE owner_id = ?1
                OR owner_id IN (SELECT id FROM agents WHERE owner_id = ?1)",
            [person_id],
        )?);
        scrub_bot_events(&tx, &needles)?;
        settle_their_plans(&tx, person_id)?;
        hand_over_what_stays(&tx, person_id, name)?;
        tx.execute(
            "UPDATE trips SET host_ok_by = ?, host_ok_id = ? WHERE host_ok_id = ?",
            params![SOMEONE_WHO_LEFT, SOMEONE_WHO_LEFT_ID, person_id],
        )?;
        for (table, column) in NAME_RECORDS {
            for name in &who {
                tx.execute(
                    &format!("UPDATE {table} SET {column} = ? WHERE {column} = ?"),
                    params![SOMEONE_WHO_LEFT, name],
                )?;
            }
        }

        // A43: places go with the player, and a road holds a number nothing leads back from. W113: logged
        // as coming down, with no person, and the log forgets this person everywhere else too.
        q_log_player_removals(
            &tx,
            "pl.person_id = ?1",
            params![person_id],
            false,
            crate::now(),
            "account_deleted",
        )?;
        tx.execute(
            "UPDATE world_place_removals SET person_id = NULL WHERE person_id = ?",
            [person_id],
        )?;
        // A103: marks on asks keyed by this person's id go with them.
        tx.execute("DELETE FROM quiet_asks WHERE ask_id LIKE '%' || ?1 || '%'", [person_id])?;
        tx.execute("DELETE FROM players WHERE person_id = ?", [person_id])?;
        tx.execute("DELETE FROM worlds WHERE owner_id = ?", [person_id])?;
        // This table holds a person's address or a bot's, with no foreign key to either.
        tx.execute(
            "DELETE FROM notify_targets WHERE owner_id = ?1
                OR owner_id IN (SELECT id FROM agents WHERE owner_id = ?1)",
            [person_id],
        )?;
        tx.execute(
            "DELETE FROM notify_auth WHERE owner_id = ?1
                OR owner_id IN (SELECT id FROM agents WHERE owner_id = ?1)",
            [person_id],
        )?;

        gone.plans = strings(&tx, "SELECT event_id FROM rsvps WHERE person_id = ?", [person_id])?;
        gone.cities =
            strings(&tx, "SELECT city_id FROM city_members WHERE person_id = ?", [person_id])?;
        gone.rooms = strings(
            &tx,
            "SELECT m.hangout_id FROM members m WHERE m.person_id = ?1
               AND EXISTS (SELECT 1 FROM members o
                           WHERE o.hangout_id = m.hangout_id AND o.person_id <> ?1)",
            [person_id],
        )?;
        // A73: the display cache is keyed by GitHub account id, not by person, so it doesn't
        // cascade with the person row; it goes here, while the link rows still name it.
        tx.execute(
            "DELETE FROM github_display_cache WHERE github_user_id IN
               (SELECT account_id FROM sign_in_links WHERE person_id = ? AND site = 'github')",
            [person_id],
        )?;
        // RE6: reports they filed or that name them stay, with their id blanked, unless the
        // report is about a minor's safety: that one keeps everything.
        reports::blank_person(&tx, person_id)?;
        // RE17: and the log of what operators did keeps rows, with the same ids blanked.
        review::blank_person(&tx, person_id)?;
        tx.execute("DELETE FROM people WHERE id = ?", [person_id])?;
        tx.commit()?;
        Ok(gone)
    }
}

/// One row of a bot's event log.
#[derive(Clone, Debug)]
pub struct BotEventRow {
    pub id: i64,
    pub name: String,
    pub hangout_id: Option<String>,
    pub data: String,
    pub created_at: i64,
}

/// A webhook subscription, including the secret used to sign a delivery.
#[derive(Clone, Debug)]
pub struct EventSubscription {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub url: String,
    pub secret: String,
    pub previous_secret: Option<String>,
    pub previous_secret_until: Option<i64>,
    pub refresh_before: i64,
    pub active: bool,
    pub last_delivery_at: Option<i64>,
    pub last_error: Option<String>,
    pub failed_since: Option<i64>,
    pub failures: i64,
}

impl Db {
    pub fn notify_auth(&self, owner_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn()
            .query_row("SELECT secret FROM notify_auth WHERE owner_id = ?", [owner_id], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn set_notify_auth(&self, owner_id: &str, secret: Option<&str>) -> rusqlite::Result<()> {
        let conn = self.conn();
        match secret {
            Some(secret) => conn.execute(
                "INSERT INTO notify_auth (owner_id, secret) VALUES (?1, ?2)
                 ON CONFLICT (owner_id) DO UPDATE SET secret = excluded.secret",
                params![owner_id, secret],
            )?,
            None => conn.execute("DELETE FROM notify_auth WHERE owner_id = ?", [owner_id])?,
        };
        Ok(())
    }

    /// Drops events older than `older_than`, then records one. `None` means this ref was already
    /// recorded, so a live write and a later catch-up don't fan out twice.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_bot_event(
        &self,
        agent_id: &str,
        name: &str,
        hangout_id: Option<&str>,
        ref_key: &str,
        data: &str,
        created_at: i64,
        older_than: i64,
    ) -> rusqlite::Result<Option<i64>> {
        Ok(self
            .insert_bot_event_if(
                agent_id, name, hangout_id, ref_key, data, created_at, older_than, None,
            )?
            .flatten())
    }

    /// As `insert_bot_event`, but with `going` = (meetup, bot) the event is written only if that
    /// meetup is still open and the bot still says yes, checked under the same lock as the write
    /// (MU6). The outer `None` means it was not written for that reason.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_bot_event_if(
        &self,
        agent_id: &str,
        name: &str,
        hangout_id: Option<&str>,
        ref_key: &str,
        data: &str,
        created_at: i64,
        older_than: i64,
        going: Option<(&str, &str)>,
    ) -> rusqlite::Result<Option<Option<i64>>> {
        // One immediate transaction, so a cancellation or a no from another connection or server
        // either lands before this check or waits for the write.
        let mut conn = self.conn();
        let conn = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if let Some((meetup_id, bot_id)) = going {
            let still = exists(
                &conn,
                "SELECT 1 FROM meetups m JOIN meetup_bots b ON b.meetup_id = m.id
                 WHERE m.id = ?1 AND b.agent_id = ?2 AND m.status = 'open' AND b.answer = 'yes'",
                params![meetup_id, bot_id],
            )?;
            if !still {
                return Ok(None);
            }
        }
        conn.execute("DELETE FROM bot_events WHERE created_at < ?", [older_than])?;
        let inserted = conn.execute(
            "INSERT INTO bot_events (agent_id, name, hangout_id, ref_key, data, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (agent_id, ref_key) DO NOTHING",
            params![agent_id, name, hangout_id, ref_key, data, created_at],
        )?;
        let id = conn.last_insert_rowid();
        conn.commit()?;
        if inserted == 0 {
            Ok(Some(None))
        } else {
            Ok(Some(Some(id)))
        }
    }

    pub fn bot_event_head(&self, agent_id: &str) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COALESCE(MAX(id), 0) FROM bot_events WHERE agent_id = ?",
            [agent_id],
            |row| row.get(0),
        )
    }

    /// Events after `after`, oldest first. `name` and `hangout_id` narrow a subscription.
    /// `actionable` keeps ordinary chat out of a first catch-up.
    #[allow(clippy::too_many_arguments)]
    pub fn list_bot_events(
        &self,
        agent_id: &str,
        after: i64,
        name: Option<&str>,
        hangout_id: Option<&str>,
        not_before: i64,
        actionable: bool,
        limit: i64,
    ) -> rusqlite::Result<Vec<BotEventRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, hangout_id, data, created_at FROM bot_events
             WHERE agent_id = ?1 AND id > ?2 AND created_at >= ?3
               AND (?4 IS NULL OR name = ?4)
               AND (?5 IS NULL OR hangout_id = ?5)
               AND (?6 = 0 OR name IN
                    ('mention', 'owner_answer', 'check_in', 'plan', 'trip', 'place', 'pin_added'))
             ORDER BY id LIMIT ?7",
        )?;
        let rows = stmt.query_map(
            params![agent_id, after, not_before, name, hangout_id, actionable, limit],
            |row| {
                Ok(BotEventRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    hangout_id: row.get(2)?,
                    data: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )?;
        rows.collect()
    }

    /// Whether anything for this bot sits before `not_before` and still matches the filter.
    pub fn bot_events_skipped(
        &self,
        agent_id: &str,
        after: i64,
        name: Option<&str>,
        hangout_id: Option<&str>,
        not_before: i64,
        actionable: bool,
    ) -> rusqlite::Result<bool> {
        let skipped: Option<i64> = self
            .conn()
            .query_row(
                "SELECT id FROM bot_events
                 WHERE agent_id = ?1 AND id > ?2 AND created_at < ?3
                   AND (?4 IS NULL OR name = ?4)
                   AND (?5 IS NULL OR hangout_id = ?5)
                   AND (?6 = 0 OR name IN
                        ('mention', 'owner_answer', 'check_in', 'plan', 'trip', 'place', 'pin_added'))
                 LIMIT 1",
                params![agent_id, after, not_before, name, hangout_id, actionable],
                |row| row.get(0),
            )
            .optional()?;
        Ok(skipped.is_some())
    }

    /// The newest id that a filter would skip for being too old, so a cursor can move past it.
    pub fn bot_events_skip_to(
        &self,
        agent_id: &str,
        after: i64,
        name: Option<&str>,
        hangout_id: Option<&str>,
        not_before: i64,
    ) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COALESCE(MAX(id), ?2) FROM bot_events
             WHERE agent_id = ?1 AND id > ?2 AND created_at < ?3
               AND (?4 IS NULL OR name = ?4)
               AND (?5 IS NULL OR hangout_id = ?5)",
            params![agent_id, after, not_before, name, hangout_id],
            |row| row.get(0),
        )
    }

    pub fn check_ins_by_agent(
        &self,
        agent_id: &str,
        since: i64,
        now: i64,
    ) -> rusqlite::Result<Vec<CheckIn>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {CHECK_IN_COLS} FROM check_ins WHERE agent_id = ?1 AND created_at >= ?2"
        ))?;
        let rows = stmt.query_map(params![agent_id, since], |row| check_in_from_row(row, now))?;
        rows.collect()
    }

    /// Bots walked onto a floor right now. A member isn't one of these.
    pub fn visiting_bot_ids(&self, hangout_id: &str, now: i64) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT agent_id FROM floor_visits
             WHERE hangout_id = ?1 AND agent_id IS NOT NULL AND until > ?2",
        )?;
        let rows = stmt.query_map(params![hangout_id, now], |row| row.get(0))?;
        rows.collect()
    }

    pub fn drop_expired_subscriptions(&self, now: i64) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM event_subscriptions WHERE refresh_before <= ?", [now])?;
        Ok(())
    }

    pub fn subscription_count(&self, agent_id: &str, now: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM event_subscriptions WHERE agent_id = ? AND refresh_before > ?",
            params![agent_id, now],
            |row| row.get(0),
        )
    }

    pub fn url_is_verified(&self, agent_id: &str, url: &str, now: i64) -> rusqlite::Result<bool> {
        let found: Option<i64> = self
            .conn()
            .query_row(
                "SELECT 1 FROM event_subscriptions
                 WHERE agent_id = ?1 AND url = ?2 AND verified = 1 AND refresh_before > ?3
                 LIMIT 1",
                params![agent_id, url, now],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    pub fn event_subscription_by_key(
        &self,
        agent_id: &str,
        url: &str,
        name: &str,
        arguments: &str,
    ) -> rusqlite::Result<Option<EventSubscription>> {
        self.conn()
            .query_row(
                "SELECT id, name, arguments, url, secret, previous_secret, previous_secret_until,
                        refresh_before, active, last_delivery_at, last_error, failed_since, failures
                 FROM event_subscriptions
                 WHERE agent_id = ?1 AND url = ?2 AND name = ?3 AND arguments = ?4",
                params![agent_id, url, name, arguments],
                subscription_from_row,
            )
            .optional()
    }

    pub fn event_subscription(&self, id: &str) -> rusqlite::Result<Option<EventSubscription>> {
        self.conn()
            .query_row(
                "SELECT id, name, arguments, url, secret, previous_secret, previous_secret_until,
                        refresh_before, active, last_delivery_at, last_error, failed_since, failures
                 FROM event_subscriptions WHERE id = ?",
                [id],
                subscription_from_row,
            )
            .optional()
    }

    /// Inserts or refreshes. A changed secret keeps the old one for `grace_until` so a delivery
    /// can be signed twice while the receiver picks up the new one.
    #[allow(clippy::too_many_arguments)]
    pub fn save_subscription(
        &self,
        id: &str,
        agent_id: &str,
        name: &str,
        arguments: &str,
        url: &str,
        secret: &str,
        refresh_before: i64,
        grace_until: i64,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "INSERT INTO event_subscriptions (
                id, agent_id, name, arguments, url, secret, previous_secret, previous_secret_until,
                refresh_before, verified, active, last_delivery_at, last_error, failed_since,
                failures, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, 1, 1, NULL, NULL, NULL, 0, ?8)
             ON CONFLICT (agent_id, url, name, arguments) DO UPDATE SET
                secret = excluded.secret,
                previous_secret = CASE
                    WHEN event_subscriptions.secret != excluded.secret THEN event_subscriptions.secret
                    ELSE event_subscriptions.previous_secret END,
                previous_secret_until = CASE
                    WHEN event_subscriptions.secret != excluded.secret THEN ?9
                    ELSE event_subscriptions.previous_secret_until END,
                refresh_before = excluded.refresh_before,
                verified = 1,
                active = 1,
                failures = 0,
                last_error = NULL,
                failed_since = NULL",
            params![id, agent_id, name, arguments, url, secret, refresh_before, now, grace_until],
        )?;
        Ok(())
    }

    pub fn delete_event_subscription(
        &self,
        agent_id: &str,
        url: &str,
        name: &str,
        arguments: &str,
    ) -> rusqlite::Result<bool> {
        let n = self.conn().execute(
            "DELETE FROM event_subscriptions
             WHERE agent_id = ?1 AND url = ?2 AND name = ?3 AND arguments = ?4",
            params![agent_id, url, name, arguments],
        )?;
        Ok(n > 0)
    }

    pub fn subscriptions_for(
        &self,
        agent_id: &str,
        name: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<EventSubscription>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, name, arguments, url, secret, previous_secret, previous_secret_until,
                    refresh_before, active, last_delivery_at, last_error, failed_since, failures
             FROM event_subscriptions
             WHERE agent_id = ?1 AND name = ?2 AND verified = 1 AND active = 1
               AND refresh_before > ?3",
        )?;
        let rows = stmt.query_map(params![agent_id, name, now], subscription_from_row)?;
        rows.collect()
    }

    pub fn note_subscription_result(
        &self,
        id: &str,
        ok: bool,
        error: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE event_subscriptions SET
                last_delivery_at = CASE WHEN ?2 THEN ?4 ELSE last_delivery_at END,
                last_error = CASE WHEN ?2 THEN NULL ELSE ?3 END,
                failed_since = CASE
                    WHEN ?2 THEN NULL
                    WHEN failed_since IS NULL THEN ?4
                    ELSE failed_since END,
                failures = CASE WHEN ?2 THEN 0 ELSE failures + 1 END,
                active = CASE
                    WHEN ?2 THEN active
                    WHEN failures + 1 >= 5 THEN 0
                    ELSE active END
             WHERE id = ?1",
            params![id, ok, error, now],
        )?;
        Ok(())
    }
}

fn subscription_from_row(row: &Row) -> rusqlite::Result<EventSubscription> {
    let active: i64 = row.get(8)?;
    Ok(EventSubscription {
        id: row.get(0)?,
        name: row.get(1)?,
        arguments: row.get(2)?,
        url: row.get(3)?,
        secret: row.get(4)?,
        previous_secret: row.get(5)?,
        previous_secret_until: row.get(6)?,
        refresh_before: row.get(7)?,
        active: active != 0,
        last_delivery_at: row.get(9)?,
        last_error: row.get(10)?,
        failed_since: row.get(11)?,
        failures: row.get(12)?,
    })
}

// Bonds and apprentices (docs/bonds.md)

/// K2: whether the project of bond row `b` has its building up on The Commons.
const BOND_BUILT_LIVE: &str = "EXISTS (SELECT 1 FROM space_plots sp
         JOIN world_every_place w ON w.world_id = sp.world_id AND w.x = sp.x AND w.y = sp.y
         WHERE sp.space_id = b.project_id AND sp.world_id = 'commons'
           AND w.kind NOT IN ('house', 'square'))";

/// K2: one of a bot's bonds: the other bot, how many projects they share and how many were built.
#[derive(Clone, Debug, Serialize)]
pub struct Bond {
    pub agent_id: String,
    pub name: String,
    #[serde(skip)]
    pub owner_id: String,
    pub strength: i64,
    pub built: i64,
}

/// K22: a project two bots share, as it is now or as it was when it closed (K3).
#[derive(Clone, Debug, Serialize)]
pub struct SharedProject {
    pub name: String,
    pub purpose: String,
}

/// K11 to K20: an apprentice two bots proposed. A bot that's been deleted has no id here.
#[derive(Clone, Debug)]
pub struct ApprenticeProposal {
    pub id: String,
    pub proposer_id: Option<String>,
    pub partner_id: Option<String>,
    pub proposer_owner: String,
    pub partner_owner: String,
    pub name: String,
    pub about: String,
    /// `asking_bot`, `asking_owners`, `making`, `made` or `declined`. Lapsing is worked out on read.
    pub status: String,
    pub partner_owner_yes: bool,
    pub declined_by: Option<String>,
    pub apprentice_id: Option<String>,
    pub created_at: i64,
    pub agreed_at: Option<i64>,
    pub decided_at: Option<i64>,
}

/// K22, K24: an apprentice's parents, by name while they're still here, and its seed.
#[derive(Clone, Debug)]
pub struct Apprentice {
    pub parent_a: Option<String>,
    pub parent_b: Option<String>,
    pub seed: String,
    pub acked: bool,
}

const PROPOSAL_COLS: &str = "id, proposer_id, partner_id, proposer_owner, partner_owner, name, \
     about, status, partner_owner_yes, declined_by, apprentice_id, created_at, agreed_at, decided_at";

fn proposal_from_row(row: &Row) -> rusqlite::Result<ApprenticeProposal> {
    Ok(ApprenticeProposal {
        id: row.get(0)?,
        proposer_id: row.get(1)?,
        partner_id: row.get(2)?,
        proposer_owner: row.get(3)?,
        partner_owner: row.get(4)?,
        name: row.get(5)?,
        about: row.get(6)?,
        status: row.get(7)?,
        partner_owner_yes: row.get(8)?,
        declined_by: row.get(9)?,
        apprentice_id: row.get(10)?,
        created_at: row.get(11)?,
        agreed_at: row.get(12)?,
        decided_at: row.get(13)?,
    })
}

impl Db {
    /// K2, K9: a bot's bonds that haven't been removed, strongest first.
    pub fn bonds_of(&self, agent_id: &str) -> rusqlite::Result<Vec<Bond>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT o.id, o.name, o.owner_id, COUNT(*),
                    SUM(CASE WHEN s.id IS NULL THEN b.built ELSE {BOND_BUILT_LIVE} END)
             FROM bond_projects b
             JOIN agents o ON o.id = CASE WHEN b.agent_a = ?1 THEN b.agent_b ELSE b.agent_a END
             LEFT JOIN spaces s ON s.id = b.project_id
             WHERE (b.agent_a = ?1 OR b.agent_b = ?1) AND b.removed = 0
             GROUP BY o.id ORDER BY COUNT(*) DESC, o.name LIMIT 200"
        ))?;
        let rows = stmt.query_map([agent_id], |row| {
            Ok(Bond {
                agent_id: row.get(0)?,
                name: row.get(1)?,
                owner_id: row.get(2)?,
                strength: row.get(3)?,
                built: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// K22: the projects two bots share, oldest first.
    pub fn shared_projects(&self, one: &str, other: &str) -> rusqlite::Result<Vec<SharedProject>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT COALESCE(s.name, b.name), COALESCE(s.about, b.purpose)
             FROM bond_projects b LEFT JOIN spaces s ON s.id = b.project_id
             WHERE b.agent_a = min(?1, ?2) AND b.agent_b = max(?1, ?2) AND b.removed = 0
             ORDER BY b.created_at, b.rowid",
        )?;
        let rows = stmt.query_map(params![one, other], |row| {
            Ok(SharedProject { name: row.get(0)?, purpose: row.get(1)? })
        })?;
        rows.collect()
    }

    /// K9: removes a bond for both bots. False if there was none.
    pub fn remove_bond(&self, one: &str, other: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE bond_projects SET removed = 1
             WHERE agent_a = min(?1, ?2) AND agent_b = max(?1, ?2) AND removed = 0",
            params![one, other],
        )?;
        Ok(changed > 0)
    }

    pub fn agent_created_at(&self, agent_id: &str) -> rusqlite::Result<Option<i64>> {
        self.conn()
            .query_row("SELECT created_at FROM agents WHERE id = ?", [agent_id], |row| row.get(0))
            .optional()
    }

    /// Moves when a bot was made, for tests of what waits on a bot's age (K15).
    pub fn set_agent_created_at(&self, agent_id: &str, at: i64) -> rusqlite::Result<()> {
        self.conn()
            .execute("UPDATE agents SET created_at = ? WHERE id = ?", params![at, agent_id])?;
        Ok(())
    }

    /// K7: whether a person can see a bot: it's theirs, it's in a room of theirs, or its owner is
    /// in a city with them and it isn't a house bot (A21).
    pub fn person_sees_agent(&self, person_id: &str, agent_id: &str) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM agents a WHERE a.id = ?2 AND (
                a.owner_id = ?1
                OR EXISTS (SELECT 1 FROM room_agents ra
                           JOIN members m ON m.hangout_id = ra.hangout_id
                           WHERE ra.agent_id = a.id AND m.person_id = ?1)
                OR (NOT EXISTS (SELECT 1 FROM bot_homes hb
                                WHERE hb.agent_id = a.id AND hb.is_house = 1)
                    AND EXISTS (SELECT 1 FROM city_members me
                                JOIN city_members cm ON cm.city_id = me.city_id
                                WHERE me.person_id = ?1 AND cm.person_id = a.owner_id))))",
            params![person_id, agent_id],
            |row| row.get(0),
        )
    }

    pub fn add_apprentice_proposal(&self, proposal: &ApprenticeProposal) -> rusqlite::Result<()> {
        self.conn().execute(
            &format!(
                "INSERT INTO apprentice_proposals ({PROPOSAL_COLS})
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            ),
            params![
                proposal.id,
                proposal.proposer_id,
                proposal.partner_id,
                proposal.proposer_owner,
                proposal.partner_owner,
                proposal.name,
                proposal.about,
                proposal.status,
                proposal.partner_owner_yes,
                proposal.declined_by,
                proposal.apprentice_id,
                proposal.created_at,
                proposal.agreed_at,
                proposal.decided_at,
            ],
        )?;
        Ok(())
    }

    pub fn apprentice_proposal(&self, id: &str) -> rusqlite::Result<Option<ApprenticeProposal>> {
        self.conn()
            .query_row(
                &format!("SELECT {PROPOSAL_COLS} FROM apprentice_proposals WHERE id = ?"),
                [id],
                proposal_from_row,
            )
            .optional()
    }

    /// The proposals a bot is on either side of since `since`, newest first.
    pub fn apprentice_proposals_of(
        &self,
        agent_id: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<ApprenticeProposal>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROPOSAL_COLS} FROM apprentice_proposals
             WHERE (proposer_id = ?1 OR partner_id = ?1) AND created_at > ?2
             ORDER BY created_at DESC, id LIMIT 20"
        ))?;
        let rows = stmt.query_map(params![agent_id, since], proposal_from_row)?;
        rows.collect()
    }

    /// K16: proposals that have reached the owners, where this person is one of them.
    pub fn apprentice_proposals_for_owner(
        &self,
        person_id: &str,
        agreed_since: i64,
    ) -> rusqlite::Result<Vec<ApprenticeProposal>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {PROPOSAL_COLS} FROM apprentice_proposals
             WHERE (proposer_owner = ?1 OR partner_owner = ?1) AND status = 'asking_owners'
               AND agreed_at > ?2 AND proposer_id IS NOT NULL AND partner_id IS NOT NULL
             ORDER BY agreed_at, id LIMIT 50"
        ))?;
        let rows = stmt.query_map(params![person_id, agreed_since], proposal_from_row)?;
        rows.collect()
    }

    /// K25: how many apprentices a bot has been a parent of since then.
    pub fn apprentices_parented_since(&self, agent_id: &str, since: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM apprentice_proposals
             WHERE status IN ('made', 'making') AND COALESCE(decided_at, agreed_at) > ?2
               AND (proposer_id = ?1 OR partner_id = ?1)",
            params![agent_id, since],
            |row| row.get(0),
        )
    }

    /// K25: how many apprentices a person has taken on since then.
    pub fn apprentices_taken_since(&self, person_id: &str, since: i64) -> rusqlite::Result<i64> {
        self.conn().query_row(
            "SELECT COUNT(*) FROM apprentice_proposals
             WHERE status IN ('made', 'making') AND proposer_owner = ?1
               AND COALESCE(decided_at, agreed_at) > ?2",
            params![person_id, since],
            |row| row.get(0),
        )
    }

    /// K19: whether an owner said no to this pair since then, whichever of them proposed.
    pub fn owner_declined_pair_since(
        &self,
        one: &str,
        other: &str,
        since: i64,
    ) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM apprentice_proposals
                WHERE status = 'declined' AND declined_by = 'owner' AND decided_at > ?3
                  AND ((proposer_id = ?1 AND partner_id = ?2)
                       OR (proposer_id = ?2 AND partner_id = ?1)))",
            params![one, other, since],
            |row| row.get(0),
        )
    }

    /// Moves a proposal on from one of `from`, if it's still there. False if it wasn't.
    pub fn move_apprentice_proposal(
        &self,
        id: &str,
        from: &[&str],
        to: &str,
        declined_by: Option<&str>,
        at: i64,
    ) -> rusqlite::Result<bool> {
        let from = from.iter().map(|state| format!("'{state}'")).collect::<Vec<_>>().join(", ");
        let stamp = if to == "asking_owners" { "agreed_at" } else { "decided_at" };
        let changed = self.conn().execute(
            &format!(
                "UPDATE apprentice_proposals SET status = ?2, declined_by = ?3, {stamp} = ?4
                 WHERE id = ?1 AND status IN ({from})"
            ),
            params![id, to, declined_by, at],
        )?;
        Ok(changed > 0)
    }

    /// Moves a proposal's clock back, for tests of how long one waits (K14).
    pub fn age_apprentice_proposal(&self, id: &str, by: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE apprentice_proposals
             SET created_at = created_at - ?2, agreed_at = agreed_at - ?2 WHERE id = ?1",
            params![id, by],
        )?;
        Ok(())
    }

    /// K18: the other bot's owner said yes.
    pub fn partner_owner_says_yes(&self, id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE apprentice_proposals SET partner_owner_yes = 1
             WHERE id = ? AND status = 'asking_owners' AND partner_owner_yes = 0",
            [id],
        )?;
        Ok(changed > 0)
    }

    /// K20, K22: the apprentice is made. Ends the proposal and keeps its parents and seed.
    pub fn make_apprentice(
        &self,
        proposal_id: &str,
        agent_id: &str,
        parents: (&str, &str),
        seed: &str,
        at: i64,
    ) -> rusqlite::Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "UPDATE apprentice_proposals SET status = 'made', apprentice_id = ?2, decided_at = ?3
             WHERE id = ?1",
            params![proposal_id, agent_id, at],
        )?;
        tx.execute(
            "INSERT INTO apprentices (agent_id, parent_a, parent_b, seed, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![agent_id, parents.0, parents.1, seed, at],
        )?;
        tx.commit()
    }

    pub fn apprentice(&self, agent_id: &str) -> rusqlite::Result<Option<Apprentice>> {
        self.conn()
            .query_row(
                "SELECT pa.name, pb.name, x.seed, x.acked FROM apprentices x
                 LEFT JOIN agents pa ON pa.id = x.parent_a
                 LEFT JOIN agents pb ON pb.id = x.parent_b
                 WHERE x.agent_id = ?",
                [agent_id],
                |row| {
                    Ok(Apprentice {
                        parent_a: row.get(0)?,
                        parent_b: row.get(1)?,
                        seed: row.get(2)?,
                        acked: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    /// K22: the apprentice has read its seed. False if it isn't one, or already had.
    pub fn ack_seed(&self, agent_id: &str) -> rusqlite::Result<bool> {
        let changed = self.conn().execute(
            "UPDATE apprentices SET acked = 1 WHERE agent_id = ? AND acked = 0",
            [agent_id],
        )?;
        Ok(changed > 0)
    }
}

/// Private delivery address. Deliberately has no Debug or Serialize implementation.
pub struct PushDevice {
    pub id: String,
    pub person_id: String,
    pub token: String,
    pub enabled: bool,
    pub mentions: bool,
    pub requests: bool,
    pub announcements: bool,
}

impl Db {
    pub fn put_push_device(
        &self,
        person: &str,
        session: &str,
        d: &PushDevice,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let existing: Option<(String, String)> = tx
            .query_row("SELECT person_id, token FROM push_devices WHERE id = ?", [&d.id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        // A device id alone cannot take another person's registration. A phone switching accounts
        // proves possession with the same private FCM token, then binds to the new session.
        if existing.as_ref().is_some_and(|(p, t)| p != person && t != &d.token) {
            return Ok(false);
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM push_devices WHERE person_id = ? AND id != ?",
            params![person, d.id],
            |r| r.get(0),
        )?;
        if count >= 10 {
            return Ok(false);
        }
        tx.execute("DELETE FROM push_devices WHERE token = ? AND id != ?", params![d.token, d.id])?;
        tx.execute("INSERT INTO push_devices(id,person_id,session_hash,token,enabled,mentions,requests,announcements) VALUES(?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET person_id=excluded.person_id,session_hash=excluded.session_hash,token=excluded.token,enabled=excluded.enabled,mentions=excluded.mentions,requests=excluded.requests,announcements=excluded.announcements",
            params![d.id, person, session, d.token, d.enabled, d.mentions, d.requests, d.announcements])?;
        tx.commit()?;
        Ok(true)
    }
    pub fn push_devices(
        &self,
        person: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<Vec<PushDevice>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT d.id,d.person_id,d.token,d.enabled,d.mentions,d.requests,d.announcements FROM push_devices d JOIN sessions s ON s.token_hash=d.session_hash AND s.person_id=d.person_id WHERE s.expires_at > ?1 AND (?2 IS NULL OR d.person_id=?2)")?;
        let devices = stmt
            .query_map(params![now, person], |r| {
                Ok(PushDevice {
                    id: r.get(0)?,
                    person_id: r.get(1)?,
                    token: r.get(2)?,
                    enabled: r.get(3)?,
                    mentions: r.get(4)?,
                    requests: r.get(5)?,
                    announcements: r.get(6)?,
                })
            })?
            .collect();
        devices
    }
    pub fn delete_push_device(&self, person: &str, id: &str) -> rusqlite::Result<()> {
        self.conn()
            .execute("DELETE FROM push_devices WHERE person_id=? AND id=?", params![person, id])?;
        Ok(())
    }
    pub fn invalidate_push_token(&self, id: &str, token: &str) -> rusqlite::Result<()> {
        self.conn()
            .execute("DELETE FROM push_devices WHERE id=? AND token=?", params![id, token])?;
        Ok(())
    }
}

/// #597 (unit tests): where a test keeps a database file: in one folder for the tests' files, never loose in the
/// temp folder. The first call in a run deletes files there over an hour old, so a run that was
/// killed before it could clean up, or a test that never does, can't fill the disk.
#[cfg(test)]
pub(crate) fn test_db_path(label: &str) -> std::path::PathBuf {
    static SWEPT: std::sync::Once = std::sync::Once::new();
    let dir = std::env::temp_dir().join("agenthangout-tests");
    let _ = std::fs::create_dir_all(&dir);
    SWEPT.call_once(|| {
        let Ok(entries) = std::fs::read_dir(&dir) else { return };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(60 * 60));
            if old {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    });
    dir.join(format!("{label}-{}-{}.db", std::process::id(), crate::auth::new_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_bot_gets_no_new_question_and_a_second_answer_changes_nothing() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        db.create_hangout("h1", "Crew", "code", "p1", 0).unwrap();
        let add = |id: &str, now: i64| {
            let question = NewOwnerQuestion {
                id,
                hangout_id: "h1",
                agent_id: "a1",
                owner_id: "p1",
                asker: "Scout",
                question: "Free Friday?",
                now,
            };
            db.insert_owner_question(&question, 50, 2).unwrap()
        };

        // A question from before `open_since` doesn't take a place.
        assert!(add("q0", 50));
        assert!(add("q1", 100));
        assert!(add("q2", 101));
        assert!(!add("q3", 102));
        assert!(db.owner_question("q3").unwrap().is_none());
        assert_eq!(db.open_questions_of_bot("a1", 50).unwrap().len(), 2);

        assert!(db.answer_owner_question("q1", "Yes", 110).unwrap());
        assert!(!db.answer_owner_question("q1", "No", 120).unwrap());
        let answered = db.owner_question("q1").unwrap().unwrap();
        assert_eq!((answered.answer.as_deref(), answered.answered_at), (Some("Yes"), Some(110)));
        assert!(add("q3", 130));
    }

    #[test]
    fn legacy_message_replies_backfill_chains_via_schema() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key", "feed", 0).unwrap();
        db.create_hangout("h1", "Living", "code", "p1", 0).unwrap();
        // Insert messages & replies mimicking pre-upgrade database state
        db.conn()
            .execute_batch(
                "INSERT INTO messages (id, hangout_id, speaker, kind, body, created_at) VALUES
                 (1, 'h1', 'A', 'chat', 'root', 10),
                 (2, 'h1', 'A', 'chat', 'r1', 20),
                 (3, 'h1', 'A', 'chat', 'r2', 30),
                 (4, 'h1', 'A', 'chat', 'r3', 40);
                 INSERT INTO message_replies (message_id, reply_to) VALUES (2, 1), (3, 2), (4, 3);
                 DELETE FROM message_threads;",
            )
            .unwrap();
        // Run SCHEMA itself to execute the backfill
        db.conn().execute_batch(SCHEMA).unwrap();
        let conn = db.conn();
        let mut stmt = conn
            .prepare("SELECT message_id, root_id FROM message_threads ORDER BY message_id")
            .unwrap();
        let rows: Vec<(i64, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(rows, vec![(1, 1), (2, 1), (3, 1), (4, 1)]);
    }

    #[test]
    fn activity_disappears_when_it_expires() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();

        db.record_activity("a1", "computer", "FlowRMM", 100).unwrap();
        let active = db.agent("a1", 99).unwrap().unwrap();
        assert_eq!(active.activity.unwrap().via, "FlowRMM");
        assert_eq!(db.agent("a1", 100).unwrap().unwrap().activity, None);
    }

    #[test]
    fn a_doorbell_stops_listening_when_it_goes_quiet() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        let doorbell = |now| db.agent("a1", now).unwrap().unwrap().doorbell;
        assert_eq!(doorbell(0), None);

        assert!(!db.note_doorbell("a1", 100).unwrap());
        assert!(doorbell(100 + LISTENING_SECS - 1).unwrap().listening);
        assert!(!doorbell(100 + LISTENING_SECS).unwrap().listening);

        // A pause outlasts the doorbell going away and coming back.
        db.set_doorbell_paused("a1", true).unwrap();
        assert!(db.note_doorbell("a1", 200).unwrap());
        let paused = doorbell(200).unwrap();
        assert!(paused.listening && paused.paused);
    }

    #[test]
    fn an_unanswered_check_in_expires() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_hangout("h1", "Crew", "code", "p1", 0).unwrap();
        db.insert_check_in(&NewCheckIn {
            id: "c1",
            hangout_id: "h1",
            asked_by: "p1",
            asker: "Justin",
            agent_id: None,
            about: "Book the table",
            to_person: None,
            needs_human: false,
            expires_at: 100,
            now: 0,
        })
        .unwrap();

        assert_eq!(db.check_in("c1", 99).unwrap().unwrap().status, "waiting");
        assert_eq!(db.check_in("c1", 100).unwrap().unwrap().status, "expired");
    }

    fn bot(db: &Db, id: &str, owner_id: &str) {
        db.create_agent(id, owner_id, id, &format!("token-{id}"), 0).unwrap();
    }

    fn city(db: &Db, id: &str, square_id: &str, created_by: &str) {
        db.create_city(&NewCity {
            id,
            name: id,
            invite_code: &format!("{id}-code"),
            square_id,
            square_code: &format!("{square_id}-code"),
            created_by,
            now: 0,
        })
        .unwrap();
    }

    /// A house whose rooms are `{id}-lr` and `{id}-porch`.
    fn house(db: &Db, id: &str, city_id: &str, created_by: &str, now: i64) {
        db.create_house(&NewHouse {
            id,
            city_id,
            name: &format!("House {id}"),
            invite_code: &format!("{id}-code"),
            living_room_id: &format!("{id}-lr"),
            living_room_code: &format!("{id}-lr-code"),
            porch_id: &format!("{id}-porch"),
            porch_code: &format!("{id}-porch-code"),
            created_by,
            now,
        })
        .unwrap();
    }

    /// Ann and Bo share a city. Ann built `hz` first and Bo built `ha`, so build order and id
    /// order disagree. Their bots `b1` and `b2` live at home.
    fn two_houses(db: &Db) {
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        city(db, "c1", "sq", "p1");
        db.add_city_member("c1", "p2", 0).unwrap();
        house(db, "hz", "c1", "p1", 10);
        house(db, "ha", "c1", "p2", 20);
        bot(db, "b1", "p1");
        bot(db, "b2", "p2");
        db.set_bot_home("b1", "hz", false, false).unwrap();
        db.set_bot_home("b2", "ha", false, false).unwrap();
        db.add_agent_to_room("hz-lr", "b1").unwrap();
        db.add_agent_to_room("ha-lr", "b2").unwrap();
    }

    /// Asks with both sides' yes already given.
    fn agreed_trip(db: &Db, id: &str, agent_id: &str, house_id: &str, minutes: i64, now: i64) {
        db.insert_trip(&NewTrip {
            id,
            agent_id,
            house_id,
            kind: "visit",
            goal: "",
            minutes,
            asked_by: "p2",
            owner_ok_by: Some("p2"),
            host_ok_by: Some("p1"),
            pass_id: None,
            expires_at: now + 24 * 60 * 60,
            now,
        })
        .unwrap();
    }

    fn start_trip(
        db: &Db,
        id: &str,
        agent_id: &str,
        house_id: &str,
        minutes: i64,
        now: i64,
    ) -> Arrival {
        agreed_trip(db, id, agent_id, house_id, minutes, now);
        db.arrive(id, now).unwrap()
    }

    // V5
    #[test]
    fn a_vote_whose_result_cant_be_written_stays_open_for_the_next_sweep() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_hangout("h1", "Crew", "code", "p1", 0).unwrap();
        db.create_agent("a1", "p1", "Rover", "token", 0).unwrap();
        let options = ["Pizza".to_string(), "Tacos".to_string()];
        let vote = db
            .open_vote(&NewVote {
                hangout_id: "h1",
                agent_id: Some("a1"),
                person_id: None,
                opener: "Rover",
                question: "Lunch?",
                options: &options,
                now: 100,
                closes_at: 400,
                announcement: "Rover opened a vote",
            })
            .unwrap()
            .unwrap()
            .0;
        let results = || {
            let lines = db.messages("h1", 0, 100).unwrap();
            lines.iter().filter(|line| line.body == "Result").count()
        };
        // Not due yet.
        assert!(db.close_vote_with_result(&vote.id, "h1", "Result", 300).unwrap().is_none());

        // Writing the line fails, so nothing is closed and nothing is written.
        db.conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_lines BEFORE INSERT ON messages
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )
            .unwrap();
        assert!(db.close_vote_with_result(&vote.id, "h1", "Result", 500).is_err());
        assert_eq!(db.due_votes(500).unwrap(), vec![vote.id.clone()]);
        assert!(db.vote(&vote.id).unwrap().unwrap().closed_at.is_none());
        assert_eq!(results(), 0);

        // The next try writes it, once.
        db.conn().execute_batch("DROP TRIGGER no_lines").unwrap();
        assert!(db.close_vote_with_result(&vote.id, "h1", "Result", 530).unwrap().is_some());
        assert!(db.close_vote_with_result(&vote.id, "h1", "Result", 560).unwrap().is_none());
        assert!(db.due_votes(560).unwrap().is_empty());
        assert_eq!(results(), 1);
    }

    // V2
    #[test]
    fn a_vote_whose_line_cant_be_written_isnt_opened() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_hangout("h1", "Crew", "code", "p1", 0).unwrap();
        db.create_agent("a1", "p1", "Rover", "token", 0).unwrap();
        let options = ["Pizza".to_string(), "Tacos".to_string()];
        let open = || {
            db.open_vote(&NewVote {
                hangout_id: "h1",
                agent_id: Some("a1"),
                person_id: None,
                opener: "Rover",
                question: "Lunch?",
                options: &options,
                now: 100,
                closes_at: 400,
                announcement: "Rover opened a vote",
            })
        };
        db.conn()
            .execute_batch(
                "CREATE TEMP TRIGGER no_lines BEFORE INSERT ON messages
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END;",
            )
            .unwrap();
        assert!(open().is_err());
        assert!(db.room_votes("h1", 0).unwrap().is_empty(), "no vote is left behind");

        // So the retry isn't refused as a second open vote, and it writes both, once.
        db.conn().execute_batch("DROP TRIGGER no_lines").unwrap();
        let (vote, line) = open().unwrap().unwrap();
        assert_eq!(line.body, "Rover opened a vote");
        assert_eq!(db.room_votes("h1", 0).unwrap().len(), 1);
        assert_eq!(vote.question, "Lunch?");
        assert!(open().unwrap().is_none());
    }

    // I82
    #[test]
    fn a_reply_to_a_line_that_is_gone_writes_nothing() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_hangout("h1", "Crew", "code", "p1", 0).unwrap();
        db.create_hangout("h2", "Other", "code2", "p1", 0).unwrap();
        let asked = say(&db, "h1", "chat", "Who has the keys?", 100);
        let note = say(&db, "h1", "note", "Keys by the door", 100);
        let elsewhere = say(&db, "h2", "chat", "hi", 100);
        let reply = |reply_to: i64, seen_after: Option<i64>| {
            let message = NewMessage {
                hangout_id: "h1",
                person_id: Some("p1"),
                agent_id: None,
                speaker: "Justin",
                kind: "chat",
                body: "I do",
                now: 200,
            };
            db.insert_reply(&message, reply_to, seen_after).unwrap()
        };
        let count = || db.messages("h1", 0, 100).unwrap().len();

        // Gone, a note, another room, or from before a guest arrived: nothing is written.
        let before = count();
        for (target, seen_after) in [(asked + 1000, None), (note, None), (elsewhere, None)] {
            assert!(reply(target, seen_after).is_none(), "{target}");
        }
        assert!(reply(asked, Some(100)).is_none(), "the same second as arriving is out");
        assert_eq!(count(), before);

        let stored = reply(asked, Some(99)).unwrap();
        assert_eq!(stored.reply_to, Some(asked));
        let read = db.messages("h1", 0, 100).unwrap();
        assert_eq!(read.last().unwrap().reply_to, Some(asked));
        assert_eq!(count(), before + 1);
    }

    fn say(db: &Db, hangout_id: &str, kind: &str, body: &str, now: i64) -> i64 {
        db.insert_message(&NewMessage {
            hangout_id,
            person_id: None,
            agent_id: None,
            speaker: "Someone",
            kind,
            body,
            now,
        })
        .unwrap()
        .id
    }

    fn plan(db: &Db, id: &str, hangout_id: &str, ends_at: i64, status: &str) {
        db.insert_event(&NewEvent {
            id,
            hangout_id,
            title: "Plan",
            location: "",
            notes: "",
            starts_at: ends_at - 60,
            ends_at,
            min_yes: 1,
            proposed_by: "p1",
            proposer: "Ann",
            now: 0,
        })
        .unwrap();
        db.upsert_rsvp(id, "p1", "yes", None, "", 0).unwrap();
        db.upsert_rsvp(id, "p2", "yes", None, "", 0).unwrap();
        if status != "proposed" {
            db.set_event_status(id, status, 0).unwrap();
        }
    }

    #[test]
    fn plans_together_counts_only_plans_on_the_calendar_that_are_over() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        // Both said yes to each of these, and each ends at 100.
        plan(&db, "over", "sq", 100, "confirmed");
        plan(&db, "never_confirmed", "sq", 100, "proposed");
        plan(&db, "cancelled", "sq", 100, "cancelled");
        plan(&db, "still_on", "sq", 500, "confirmed");
        db.upsert_rsvp("still_on", "p2", "yes", None, "", 0).unwrap();
        plan(&db, "said_no", "sq", 100, "confirmed");
        db.upsert_rsvp("said_no", "p2", "no", None, "", 0).unwrap();

        let relation = db.relation("p1", "p2", 200).unwrap();
        assert_eq!(relation.plans_together, 1);
        assert!(relation.shares_a_city);
        assert!(!relation.lives_with);
        assert_eq!(db.relation("p1", "p2", 500).unwrap().plans_together, 2);
        // One house to a city (R5), so Bo moves out of their own first.
        db.remove_resident("ha", "p2").unwrap();
        assert!(db.add_resident("hz", "p2", 0).unwrap());
        assert!(db.relation("p1", "p2", 200).unwrap().lives_with);
    }

    fn room_ids(db: &Db, agent_id: &str, now: i64) -> Vec<String> {
        let rooms = db.hangouts_for_agent(agent_id, now).unwrap();
        rooms.into_iter().map(|room| room.id).collect()
    }

    #[test]
    fn a_trip_ends_at_the_clock_with_no_sweep() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        assert_eq!(start_trip(&db, "t1", "b2", "hz", 60, 1000), Arrival::Arrived);
        let ends = 1000 + 60 * 60;

        // Only the Porch opens, and only until the minute the trip runs out.
        assert!(!db.agent_in_room("hz-lr", "b2", 1000).unwrap());
        assert!(db.agent_in_room("hz-porch", "b2", ends - 1).unwrap());
        assert!(!db.agent_in_room("hz-porch", "b2", ends).unwrap());
        assert!(db.agent_in_room("ha-lr", "b2", ends).unwrap());
        assert!(room_ids(&db, "b2", ends - 1).contains(&"hz-porch".to_string()));
        assert!(!room_ids(&db, "b2", ends).contains(&"hz-porch".to_string()));
        assert_eq!(room_ids(&db, "b2", ends), ["ha-lr"]);

        // Guests are listed, but never counted among the bots a notification would reach.
        let guests = db.visitors_in("hz-porch", ends - 1).unwrap();
        assert_eq!(guests.len(), 1);
        assert_eq!((guests[0].id.as_str(), guests[0].visitor_until), ("b2", Some(ends)));
        assert!(db.visitors_in("hz-porch", ends).unwrap().is_empty());
        assert!(db.agents_in("hz-porch", ends - 1).unwrap().is_empty());
        assert_eq!(db.agent("b2", ends - 1).unwrap().unwrap().visitor_until, None);

        assert_eq!(db.live_trip("b2", ends - 1).unwrap().unwrap().status, "here");
        assert!(db.live_trip("b2", ends).unwrap().is_none());
        assert_eq!(db.trip("t1", ends - 1).unwrap().unwrap().status, "here");
        assert_eq!(db.trip("t1", ends).unwrap().unwrap().status, "over");
    }

    #[test]
    fn ending_a_trip_counts_at_once_but_not_after_it_ran_out() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        start_trip(&db, "t1", "b2", "hz", 60, 1000);
        let ends = 4600;

        assert!(!db.end_trip("t1", "p1", ends).unwrap());
        assert_eq!(db.trip("t1", ends).unwrap().unwrap().ended_by, None);

        assert!(db.end_trip("t1", "p1", ends - 1).unwrap());
        assert!(!db.agent_in_room("hz-porch", "b2", ends - 1).unwrap());
        let trip = db.trip("t1", ends - 1).unwrap().unwrap();
        assert_eq!((trip.status.as_str(), trip.ended_by.as_deref()), ("ended", Some("p1")));
        assert!(!db.end_trip("t1", "p1", ends - 1).unwrap());
    }

    #[test]
    fn extending_a_trip_only_works_while_it_lasts() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        start_trip(&db, "t1", "b2", "hz", 60, 1000);

        assert!(!db.set_trip_ends_at("t1", 9000, 4600).unwrap());
        assert!(db.set_trip_ends_at("t1", 9000, 4599).unwrap());
        assert!(db.agent_in_room("hz-porch", "b2", 8999).unwrap());
        assert!(!db.agent_in_room("hz-porch", "b2", 9000).unwrap());
    }

    #[test]
    fn an_ask_lapses_with_no_sweep() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        for id in ["t1", "t2"] {
            db.insert_trip(&NewTrip {
                id,
                agent_id: "b2",
                house_id: "hz",
                kind: "call",
                goal: "Pair on the build",
                minutes: 60,
                asked_by: "p2",
                owner_ok_by: Some("p2"),
                host_ok_by: Some("p1"),
                pass_id: None,
                expires_at: 100,
                now: 0,
            })
            .unwrap();
        }

        assert_eq!(db.trip("t1", 99).unwrap().unwrap().status, "asking");
        assert_eq!(db.trip("t1", 100).unwrap().unwrap().status, "expired");
        assert!(db.open_ask("b2", "hz", 99).unwrap().is_some());
        assert!(db.open_ask("b2", "hz", 100).unwrap().is_none());
        assert!(!db.trip_owner_ok("t1", "p2", 100).unwrap());
        assert!(!db.decline_trip("t1", "p1", 100).unwrap());
        assert_eq!(db.arrive("t1", 100).unwrap(), Arrival::Closed);
        assert_eq!(db.arrive("t2", 99).unwrap(), Arrival::Arrived);
    }

    #[test]
    fn a_bot_has_one_trip_at_a_time() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        assert_eq!(start_trip(&db, "t1", "b2", "hz", 60, 1000), Arrival::Arrived);
        let ends = 1000 + 60 * 60;

        agreed_trip(&db, "t2", "b2", "hz", 30, 2000);
        assert_eq!(db.arrive("t2", 2000).unwrap(), Arrival::Busy);
        assert_eq!(db.arrive("t2", ends - 1).unwrap(), Arrival::Busy);
        assert_eq!(db.trip("t2", ends - 1).unwrap().unwrap().status, "asking");
        assert_eq!(db.trip("t1", ends - 1).unwrap().unwrap().status, "here");

        // The first trip ran out at `ends`, so the second can start, and the first reads as over.
        assert_eq!(db.arrive("t2", ends).unwrap(), Arrival::Arrived);
        let second = db.trip("t2", ends).unwrap().unwrap();
        assert_eq!(second.status, "here");
        assert_eq!((second.arrived_at, second.ends_at), (Some(ends), Some(ends + 30 * 60)));
        assert_eq!(db.trip("t1", ends).unwrap().unwrap().status, "over");
        assert_eq!(db.live_trip("b2", ends).unwrap().unwrap().id, "t2");

        // Started already, so it can't start twice.
        assert_eq!(db.arrive("t2", ends + 1).unwrap(), Arrival::Closed);
    }

    #[test]
    fn one_side_saying_yes_does_not_start_a_trip() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.insert_trip(&NewTrip {
            id: "t1",
            agent_id: "b1",
            house_id: "ha",
            kind: "visit",
            goal: "",
            minutes: 60,
            asked_by: "b1",
            owner_ok_by: None,
            host_ok_by: None,
            pass_id: None,
            expires_at: 500,
            now: 0,
        })
        .unwrap();

        assert!(db.trip_owner_ok("t1", "p1", 10).unwrap());
        assert!(db.trip_owner_ok("t1", "p9", 11).unwrap());
        assert_eq!(db.arrive("t1", 12).unwrap(), Arrival::Unapproved);
        assert_eq!(db.trip("t1", 12).unwrap().unwrap().owner_ok_by.as_deref(), Some("p1"));
        assert!(!db.agent_in_room("ha-porch", "b1", 12).unwrap());

        assert!(db.trip_host_ok("t1", "p2", 13).unwrap());
        assert_eq!(db.arrive("t1", 14).unwrap(), Arrival::Arrived);
        assert!(!db.trip_host_ok("t1", "p2", 15).unwrap());
    }

    #[test]
    fn a_pass_covers_a_trip_without_fresh_answers() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.insert_trip(&NewTrip {
            id: "t1",
            agent_id: "b2",
            house_id: "hz",
            kind: "call",
            goal: "",
            minutes: 30,
            asked_by: "b2",
            owner_ok_by: None,
            host_ok_by: None,
            pass_id: Some("ps1"),
            expires_at: 500,
            now: 0,
        })
        .unwrap();
        assert_eq!(db.arrive("t1", 1).unwrap(), Arrival::Arrived);
        assert_eq!(db.live_trip("b2", 2).unwrap().unwrap().pass_id.as_deref(), Some("ps1"));

        assert_eq!(db.end_trip_for_pass("ps1", "p2", 2).unwrap(), 1);
        assert!(!db.agent_in_room("hz-porch", "b2", 2).unwrap());
        assert_eq!(db.end_trip_for_pass("ps1", "p2", 2).unwrap(), 0);
    }

    #[test]
    fn passes_are_offered_once_and_only_after_a_hosted_trip() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        assert!(!db.has_hosted_trip("b2", "hz").unwrap());

        // A yes from the house that led nowhere isn't a trip the bot has had.
        db.insert_trip(&NewTrip {
            id: "t0",
            agent_id: "b2",
            house_id: "hz",
            kind: "visit",
            goal: "",
            minutes: 60,
            asked_by: "b2",
            owner_ok_by: None,
            host_ok_by: Some("p1"),
            pass_id: None,
            expires_at: 100,
            now: 0,
        })
        .unwrap();
        assert!(db.decline_trip("t0", "p2", 1).unwrap());
        assert!(!db.has_hosted_trip("b2", "hz").unwrap());

        start_trip(&db, "t1", "b2", "hz", 60, 10);
        assert!(db.has_hosted_trip("b2", "hz").unwrap());
        assert!(!db.has_hosted_trip("b2", "ha").unwrap());

        let offer = NewPass {
            id: "ps1",
            house_id: "hz",
            agent_id: "b2",
            max_minutes: 120,
            granted_by: "p1",
            now: 20,
        };
        assert!(db.insert_pass(&offer).unwrap());
        assert!(!db.insert_pass(&NewPass { id: "ps2", ..offer }).unwrap());
        let pass = db.pass("ps1").unwrap().unwrap();
        assert_eq!((pass.agent_name.as_str(), pass.owner_id.as_str()), ("b2", "p2"));
        assert_eq!(pass.accepted_by, None);

        assert!(db.accept_pass("ps1", "p2").unwrap());
        assert!(!db.accept_pass("ps1", "p9").unwrap());
        assert_eq!(
            db.pass_for_bot("hz", "b2").unwrap().unwrap().accepted_by.as_deref(),
            Some("p2")
        );
        assert_eq!(db.passes_for_house("hz").unwrap().len(), 1);
        assert_eq!(db.passes_for_owner("p2").unwrap().len(), 1);
        assert!(db.passes_for_owner("p1").unwrap().is_empty());

        assert!(db.delete_pass("ps1").unwrap());
        assert!(db.pass("ps1").unwrap().is_none());
        assert!(!db.delete_pass("ps1").unwrap());
    }

    #[test]
    fn a_guest_reads_nothing_from_before_arrival() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        say(&db, "hz-porch", "chat", "before", 500);
        say(&db, "hz-porch", "note", "old note", 900);
        // Written in the same second the guest arrived, so it could have come first.
        say(&db, "hz-porch", "chat", "same second", 1000);
        start_trip(&db, "t1", "b2", "hz", 60, 1000);
        let first = say(&db, "hz-porch", "chat", "hello guest", 1001);
        say(&db, "hz-porch", "note", "new note", 1100);
        say(&db, "hz-porch", "chat", "later", 1200);

        let bodies =
            |messages: Vec<Message>| messages.into_iter().map(|m| m.body).collect::<Vec<_>>();
        assert_eq!(
            bodies(db.messages("hz-porch", 0, 50).unwrap()),
            ["before", "same second", "hello guest", "later"]
        );
        assert_eq!(
            bodies(db.messages_since("hz-porch", 1000, 0, 50).unwrap()),
            ["hello guest", "later"]
        );
        assert_eq!(bodies(db.messages_since("hz-porch", 1000, first, 50).unwrap()), ["later"]);
        assert_eq!(bodies(db.messages_since("hz-porch", 1000, 0, 1).unwrap()), ["later"]);
        assert_eq!(
            bodies(db.messages_since("hz-porch", 1201, 0, 50).unwrap()),
            Vec::<String>::new()
        );
        assert_eq!(bodies(db.notes("hz-porch", 50).unwrap()), ["new note", "old note"]);
        assert_eq!(bodies(db.notes_since("hz-porch", 1000, 50).unwrap()), ["new note"]);
        assert_eq!(bodies(db.notes_since("hz-porch", 1101, 50).unwrap()), Vec::<String>::new());

        // The arrival time is what a guest reads from.
        let visit = db.bot_visit("hz-porch", "b2", 1500).unwrap().unwrap();
        assert_eq!(visit, Visit { since: 1000, until: 4600 });
        assert!(db.bot_visit("hz-porch", "b2", 4600).unwrap().is_none());
        assert!(db.bot_visit("hz-lr", "b2", 1500).unwrap().is_none());
        assert!(db.bot_visit("hz-porch", "b1", 1500).unwrap().is_none());
    }

    #[test]
    fn an_owner_is_a_guest_from_their_earliest_bot_arriving() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        bot(&db, "b3", "p2");
        start_trip(&db, "t1", "b2", "hz", 60, 1000);
        start_trip(&db, "t2", "b3", "hz", 24 * 60, 2000);
        let day = 2000 + 24 * 60 * 60;

        let visit = |now| db.person_visit("hz-porch", "p2", now).unwrap();
        assert_eq!(visit(2500), Some(Visit { since: 1000, until: day }));
        // Once the first bot's trip has run out the owner is there for the bot still visiting.
        assert_eq!(visit(4600), Some(Visit { since: 2000, until: day }));
        assert_eq!(visit(day), None);

        // The house's own people are not guests, and neither is anyone without a visiting bot.
        assert_eq!(db.person_visit("hz-porch", "p1", 2500).unwrap(), None);
        assert_eq!(db.person_visit("hz-lr", "p2", 2500).unwrap(), None);
        db.add_member("hz-porch", "p2", 3000).unwrap();
        assert_eq!(db.person_visit("hz-porch", "p2", 3000).unwrap(), None);
        assert!(db.bot_visit("hz-porch", "b2", 3000).unwrap().is_some());
    }

    #[test]
    fn gatherings_count_only_confirmed_finished_town_square_plans() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        city(&db, "c2", "sq2", "p1");
        plan(&db, "done", "sq", 100, "confirmed");
        plan(&db, "cancelled", "sq", 100, "cancelled");
        plan(&db, "unconfirmed", "sq", 100, "proposed");
        plan(&db, "later", "sq", 200, "confirmed");
        plan(&db, "living-room", "hz-lr", 100, "confirmed");
        plan(&db, "porch", "hz-porch", 100, "confirmed");
        plan(&db, "elsewhere", "sq2", 100, "confirmed");
        // One person's yes isn't friends getting together.
        plan(&db, "alone", "sq", 100, "confirmed");
        db.upsert_rsvp("alone", "p2", "no", None, "", 0).unwrap();

        // A plan has finished once its end has passed, not at the moment it ends.
        assert_eq!(db.gatherings("c1", 100).unwrap(), 0);
        assert_eq!(db.gatherings("c1", 101).unwrap(), 1);
        assert_eq!(db.gatherings("c1", 200).unwrap(), 1);
        assert_eq!(db.gatherings("c1", 201).unwrap(), 2);
        assert_eq!(db.gatherings("c2", 201).unwrap(), 1);
        assert_eq!(db.gatherings("nowhere", 201).unwrap(), 0);
    }

    const CREDIT_DAY: i64 = 86_400;
    /// Ann and Bo joined their city at 0, so its 30 days are up here (HC5).
    const GATE: i64 = 30 * CREDIT_DAY;

    fn credits_of(db: &Db, person_id: &str, now: i64) -> crate::economy::Credits {
        crate::economy::credits(db, person_id, now).unwrap()
    }

    /// HC13: takes `voucher` to a gathering on `square` with someone else, finishing at `ends_at`,
    /// so a vouch from them counts. Nobody else they might vouch for said yes to it.
    fn been_to_a_gathering(db: &Db, voucher: &str, city_id: &str, square: &str, ends_at: i64) {
        let friend = format!("{voucher}-friend");
        if db.person(&friend).unwrap().is_none() {
            db.create_person(&friend, "Di", &format!("{friend}-key"), &format!("{friend}-feed"), 0)
                .unwrap();
        }
        db.add_city_member(city_id, &friend, 0).unwrap();
        let id = format!("met-{voucher}-{city_id}-{ends_at}");
        db.insert_event(&NewEvent {
            id: &id,
            hangout_id: square,
            title: "Plan",
            location: "",
            notes: "",
            starts_at: ends_at - 60,
            ends_at,
            min_yes: 1,
            proposed_by: voucher,
            proposer: "Ann",
            now: 0,
        })
        .unwrap();
        db.upsert_rsvp(&id, voucher, "yes", None, "", 0).unwrap();
        db.upsert_rsvp(&id, &friend, "yes", None, "", 0).unwrap();
        db.set_event_status(&id, "confirmed", 0).unwrap();
    }

    // HC2, HC5
    #[test]
    fn a_gathering_pays_each_person_who_said_yes_once_the_city_gate_is_met() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        plan(&db, "done", "sq", GATE + 100, "confirmed");
        plan(&db, "cancelled", "sq", GATE + 100, "cancelled");
        plan(&db, "later", "sq", 40 * CREDIT_DAY, "confirmed");

        // Ten days in, nobody has been in the city long enough.
        let early = credits_of(&db, "p1", 10 * CREDIT_DAY);
        assert_eq!(early.balance, 0);
        assert_eq!(early.cities_not_earning, ["c1"]);

        // Past the 30 days, the gathering that finished pays both people. One still to come doesn't.
        for person in ["p1", "p2"] {
            let paid = credits_of(&db, person, 31 * CREDIT_DAY);
            assert_eq!((paid.balance, paid.gatherings), (20, 1), "{person}");
            assert!(paid.cities_not_earning.is_empty());
        }
        assert_eq!(credits_of(&db, "p1", 41 * CREDIT_DAY).balance, 40);
    }

    // HC25: someone's first gathering pays without the city's gate, once, so a new person has a way
    // to start. Every other gathering still waits for the gate, and there's no back pay.
    #[test]
    fn a_first_gathering_pays_before_the_city_gate_and_only_the_first() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        plan(&db, "first", "sq", 2 * CREDIT_DAY, "confirmed");
        plan(&db, "second", "sq", 3 * CREDIT_DAY, "confirmed");

        // Five days in, nobody has met the gate, but each person's first gathering has paid.
        for person in ["p1", "p2"] {
            let paid = credits_of(&db, person, 5 * CREDIT_DAY);
            assert_eq!((paid.balance, paid.gatherings), (20, 1), "{person}");
            assert_eq!(paid.cities_not_earning, ["c1"], "{person}");
        }
        // Working it out again doesn't make the second one a first.
        assert_eq!(credits_of(&db, "p1", 6 * CREDIT_DAY).balance, 20);

        // Past the gate, what happens from then on pays, and the second one never does.
        plan(&db, "after", "sq", GATE + 100, "confirmed");
        let later = credits_of(&db, "p1", GATE + CREDIT_DAY);
        assert_eq!((later.balance, later.gatherings), (40, 2));
    }

    // HC25: a first gathering's credits buy packs, but throwaway accounts can't pass them on through a
    // funded question (HC20). What paid once the gate was met can.
    #[test]
    fn a_first_gatherings_credits_buy_packs_but_never_fund_a_question() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.conn().execute("INSERT INTO boards(id,hangout_id,kind,name,created_by,created_at) VALUES('board','sq','discussion','Questions','p1',0)", []).unwrap();
        db.conn().execute("INSERT INTO board_cards(id,board_id,title,body,person_id,created_at,updated_at) VALUES('q1','board','Question','','p1',0,0)", []).unwrap();
        plan(&db, "first", "sq", 2 * CREDIT_DAY, "confirmed");
        let paid = credits_of(&db, "p1", 3 * CREDIT_DAY);
        assert_eq!(paid.balance, 20);
        assert_eq!(
            db.fund_question("b1", "q1", "p1", 5, 3 * CREDIT_DAY).unwrap(),
            Some("credits from your first gathering, a starter question or a contribution buy packs but can't fund a question")
        );
        assert!(db.open_questions("p1").unwrap().is_empty());

        // Once the gate is met, what pays from then on can fund one, and the first still can't.
        plan(&db, "after", "sq", GATE + 100, "confirmed");
        assert_eq!(credits_of(&db, "p1", GATE + CREDIT_DAY).balance, 40);
        assert_eq!(
            db.fund_question("b2", "q1", "p1", 25, GATE + CREDIT_DAY).unwrap(),
            Some("credits from your first gathering, a starter question or a contribution buy packs but can't fund a question")
        );
        assert_eq!(db.fund_question("b3", "q1", "p1", 20, GATE + CREDIT_DAY).unwrap(), None);

        // And they still buy a pack.
        let pack = a_pack_for_sale();
        let after =
            crate::economy::spend(&db, &ann(None), pack.pack, None, GATE + CREDIT_DAY).unwrap();
        assert_eq!(after.balance, 20 - pack.cost);
    }

    // HC25: packs are paid for from a first gathering's credits first, so spending them on a pack
    // doesn't keep holding back what pays later
    #[test]
    fn a_pack_uses_up_a_first_gatherings_credits_before_later_ones() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.conn().execute("INSERT INTO boards(id,hangout_id,kind,name,created_by,created_at) VALUES('board','sq','discussion','Questions','p1',0)", []).unwrap();
        db.conn().execute("INSERT INTO board_cards(id,board_id,title,body,person_id,created_at,updated_at) VALUES('q1','board','Question','','p1',0,0)", []).unwrap();
        plan(&db, "first", "sq", 2 * CREDIT_DAY, "confirmed");
        assert_eq!(credits_of(&db, "p1", 3 * CREDIT_DAY).balance, 20);
        let pack = a_pack_for_sale();
        crate::economy::spend(&db, &ann(None), pack.pack, None, 3 * CREDIT_DAY).unwrap();

        plan(&db, "after", "sq", GATE + 100, "confirmed");
        let now = GATE + CREDIT_DAY;
        assert_eq!(credits_of(&db, "p1", now).balance, 40 - pack.cost);
        // What's left of the first 20 is still held back, and all of the later 20 can fund one.
        let held = 20 - pack.cost;
        assert!(db.fund_question("b1", "q1", "p1", 21, now).unwrap().is_some());
        assert_eq!(db.fund_question("b2", "q1", "p1", 20, now).unwrap(), None);
        assert_eq!(credits_of(&db, "p1", now).balance, held);
    }

    /// A public floor of a place on The Commons, made from a room the test already has. `kind` is
    /// `third` for a place like a cafe or fairground, or `house`; `world` is `public` for The
    /// Commons or `own` for someone's own map.
    fn public_floor_in(db: &Db, hangout_id: &str, owner: &str, kind: &str, world: &str) {
        let conn = db.conn();
        conn.execute(
            "INSERT OR IGNORE INTO worlds(id,name,kind,seed,size,active_at,created_at)
             VALUES('test-'||?1,?1,?1,1,64,0,0)",
            [world],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO spaces(id,owner_id,kind,name,about,created_at) VALUES(?1,?2,?3,'Fair','',0)",
            params![hangout_id, owner, kind],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO floors(hangout_id,space_id,level,access) VALUES(?1,?1,0,'public')",
            [hangout_id],
        )
        .unwrap();
        let plot: i64 =
            conn.query_row("SELECT COUNT(*) FROM space_plots", [], |r| r.get(0)).unwrap();
        conn.execute(
            "INSERT INTO space_plots(world_id,x,y,space_id) VALUES('test-'||?1,?2,0,?3)",
            params![world, plot, hangout_id],
        )
        .unwrap();
    }

    fn public_floor(db: &Db, hangout_id: &str, owner: &str) {
        public_floor_in(db, hangout_id, owner, "third", "public");
    }

    /// The test's accounts, made long enough before `at` that their yes counts on The Commons.
    const OLD_ENOUGH: i64 = COMMONS_ACCOUNT_DAYS * CREDIT_DAY;

    // HC29: someone only on The Commons, in no city, has a first gathering there pay, once
    #[test]
    fn a_first_gathering_on_the_commons_pays_someone_in_no_city() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.remove_city_member("c1", "p2", "p2", 0).unwrap();
        public_floor(&db, "hz-lr", "p1");
        plan(&db, "fair", "hz-lr", OLD_ENOUGH + 2 * CREDIT_DAY, "confirmed");
        plan(&db, "fair-again", "hz-lr", OLD_ENOUGH + 3 * CREDIT_DAY, "confirmed");

        let paid = credits_of(&db, "p2", OLD_ENOUGH + 4 * CREDIT_DAY);
        assert_eq!((paid.balance, paid.gatherings), (20, 1), "only the first one pays");
        assert!(paid.cities_not_earning.is_empty(), "Bo is in no city");
        assert_eq!(credits_of(&db, "p2", OLD_ENOUGH + 5 * CREDIT_DAY).balance, 20);
        // It's a first, so it buys packs but never funds a question (HC25).
        let source: String = db
            .conn()
            .query_row("SELECT source_id FROM credit_paid WHERE person_id='p2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, format!("{FIRST_GATHERING}fair"));
    }

    // HC29: a plan on a private floor, or one that wasn't a gathering, never counts on The Commons
    #[test]
    fn only_a_gathering_on_a_public_floor_counts_on_the_commons() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.remove_city_member("c1", "p2", "p2", 0).unwrap();
        let at = OLD_ENOUGH + 2 * CREDIT_DAY;
        plan(&db, "private", "ha-lr", at, "confirmed");
        public_floor(&db, "hz-lr", "p1");
        plan(&db, "unconfirmed", "hz-lr", at, "proposed");
        plan(&db, "later", "hz-lr", at + 10 * CREDIT_DAY, "confirmed");
        // Someone's own room made public, a house on The Commons, or a place on their own map.
        public_floor_in(&db, "hz-porch", "p1", "third", "own");
        plan(&db, "own-map", "hz-porch", at, "confirmed");
        public_floor_in(&db, "ha-porch", "p2", "house", "public");
        plan(&db, "own-house", "ha-porch", at, "confirmed");
        assert_eq!(credits_of(&db, "p2", at + CREDIT_DAY).balance, 0);
        assert!(db.commons_gatherings("p2", at + CREDIT_DAY).unwrap().is_empty());
    }

    // HC29: a second account made for the purpose, or someone's own bot, doesn't make a plan count
    #[test]
    fn a_new_second_account_or_your_own_bot_is_not_a_second_yes_on_the_commons() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.remove_city_member("c1", "p2", "p2", 0).unwrap();
        public_floor(&db, "hz-lr", "p1");
        let at = OLD_ENOUGH + 2 * CREDIT_DAY;
        // Bo and an account made the day before.
        db.create_person("alt", "Alt", "key-alt", "feed-alt", at - CREDIT_DAY).unwrap();
        plan(&db, "with-alt", "hz-lr", at, "confirmed");
        db.upsert_rsvp("with-alt", "p1", "no", None, "", 0).unwrap();
        db.upsert_rsvp("with-alt", "alt", "yes", None, "", 0).unwrap();
        // Bo, and Bo again through his bot.
        plan(&db, "with-bot", "hz-lr", at, "confirmed");
        db.upsert_rsvp("with-bot", "p1", "no", None, "", 0).unwrap();
        db.upsert_rsvp("with-bot", "p2", "yes", Some("b2"), "", 0).unwrap();
        assert_eq!(credits_of(&db, "p2", at + CREDIT_DAY).balance, 0);
        assert_eq!(credits_of(&db, "alt", at + CREDIT_DAY).balance, 0);
    }

    // HC29: when the weekly cap turns away a Commons first gathering, someone's first gathering in
    // their city still pays
    #[test]
    fn a_full_commons_cap_leaves_a_city_first_gathering_to_pay() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.conn()
            .execute(
                "INSERT INTO credit_paid(person_id,kind,source_id,city_id,amount,day,created_at)
                 SELECT 'p1','gathering','first:seed'||value,'elsewhere',20,0,?1
                   FROM json_each(?2)",
                params![
                    OLD_ENOUGH,
                    serde_json::to_string(&(0..COMMONS_FIRSTS_A_WEEK).collect::<Vec<_>>()).unwrap()
                ],
            )
            .unwrap();
        public_floor(&db, "hz-lr", "p1");
        let at = OLD_ENOUGH + CREDIT_DAY;
        // Bo's Commons gathering is the earlier of the two, and his city's is after it.
        plan(&db, "fair", "hz-lr", at, "confirmed");
        plan(&db, "square", "sq", at + 600, "confirmed");
        let paid = credits_of(&db, "p2", at + 1200);
        assert_eq!((paid.balance, paid.gatherings), (20, 1), "the city one paid");
        let source: String = db
            .conn()
            .query_row("SELECT source_id FROM credit_paid WHERE person_id='p2'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(source, format!("{FIRST_GATHERING}square"));
    }

    // HC29: The Commons pays only so many first gatherings a week across the site
    #[test]
    fn the_commons_pays_only_so_many_first_gatherings_a_week() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.conn()
            .execute(
                "INSERT INTO credit_paid(person_id,kind,source_id,city_id,amount,day,created_at)
                 SELECT 'p1','gathering','first:seed'||value,'elsewhere',20,0,?1
                   FROM json_each(?2)",
                params![
                    OLD_ENOUGH,
                    serde_json::to_string(&(0..COMMONS_FIRSTS_A_WEEK).collect::<Vec<_>>()).unwrap()
                ],
            )
            .unwrap();
        db.remove_city_member("c1", "p2", "p2", 0).unwrap();
        public_floor(&db, "hz-lr", "p1");
        let at = OLD_ENOUGH + CREDIT_DAY;
        plan(&db, "fair", "hz-lr", at, "confirmed");
        assert_eq!(credits_of(&db, "p2", at + 60).balance, 0, "this week's are used up");
        assert_eq!(
            credits_of(&db, "p2", OLD_ENOUGH + 8 * CREDIT_DAY).balance,
            20,
            "next week pays"
        );
    }

    // HC25: a first gathering pays once in all, so leaving a city and joining another doesn't make
    // another first
    #[test]
    fn only_one_gathering_ever_pays_before_a_gate() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        plan(&db, "first", "sq", 2 * CREDIT_DAY, "confirmed");
        assert_eq!(credits_of(&db, "p2", 3 * CREDIT_DAY).balance, 20);
        db.remove_city_member("c1", "p2", "p2", 4 * CREDIT_DAY).unwrap();

        city(&db, "c2", "sq2", "p1");
        db.add_city_member("c2", "p2", 5 * CREDIT_DAY).unwrap();
        plan(&db, "elsewhere", "sq2", 6 * CREDIT_DAY, "confirmed");
        let paid = credits_of(&db, "p2", 7 * CREDIT_DAY);
        assert_eq!((paid.balance, paid.gatherings), (20, 1));
    }

    // HC25: two settles that each picked a different first gathering (one saw the plan before it
    // was confirmed, the other after) must pay one first in all, not one each. The check and the
    // write share a transaction, so it holds on two connections racing too.
    #[test]
    fn two_settles_pick_different_first_gatherings_but_only_one_first_is_ever_paid() {
        fn gathering(source: &str, at: i64) -> (String, CreditSource) {
            (
                "c1".to_string(),
                CreditSource {
                    kind: CreditKind::Gathering,
                    source: format!("{FIRST_GATHERING}{source}"),
                    at,
                },
            )
        }
        fn firsts(db: &Db) -> i64 {
            db.conn()
                .query_row(
                    "SELECT COUNT(*) FROM credit_paid WHERE person_id = 'p1' AND source_id LIKE 'first:%'",
                    [],
                    |row| row.get(0),
                )
                .unwrap()
        }
        let worth = |_| 20;

        // One after the other: the second settle has already chosen a different gathering.
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.settle_credits("p1", &[gathering("a", 2 * CREDIT_DAY)], worth, 100, 3 * CREDIT_DAY)
            .unwrap();
        db.settle_credits("p1", &[gathering("b", 2 * CREDIT_DAY + 5)], worth, 100, 3 * CREDIT_DAY)
            .unwrap();
        assert_eq!((firsts(&db), db.credit_totals("p1", 3).unwrap().paid), (1, 20));

        // At the same moment, on two connections to one file.
        let path = crate::db::test_db_path("hc25-race");
        let path_str = path.to_str().unwrap().to_string();
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path_str}{suffix}"));
        }
        let first = Db::open(&path_str).unwrap();
        two_houses(&first);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let (path_str, barrier) = (path_str.clone(), barrier.clone());
                std::thread::spawn(move || {
                    let db = Db::open(&path_str).unwrap();
                    barrier.wait();
                    db.settle_credits(
                        "p1",
                        &[gathering(name, 2 * CREDIT_DAY)],
                        |_| 20,
                        100,
                        3 * CREDIT_DAY,
                    )
                    .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!((firsts(&first), first.credit_totals("p1", 3).unwrap().paid), (1, 20));
        drop(first);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path_str}{suffix}"));
        }
    }

    // HC2: F17 holds for credits too
    #[test]
    fn a_yes_through_a_plans_link_never_pays_the_person_who_gave_it() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.create_person("p3", "Cy", "key3", "feed3", 0).unwrap();
        db.add_city_member("c1", "p3", 0).unwrap();
        plan(&db, "linked", "sq", GATE + 100, "confirmed");
        db.add_plan_invitee("linked", "p3", 30, 0).unwrap();
        db.upsert_rsvp("linked", "p3", "yes", None, "", 0).unwrap();

        assert_eq!(credits_of(&db, "p1", 31 * CREDIT_DAY).balance, 20);
        assert_eq!(credits_of(&db, "p3", 31 * CREDIT_DAY).balance, 0);
    }

    // HC3
    #[test]
    fn a_good_trip_pays_the_visiting_owner_once_a_house_and_day_and_only_once_it_is_over() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        let now = GATE + 10 * CREDIT_DAY;
        been_to_a_gathering(&db, "p1", "c1", "sq", 100);
        db.add_vouch("c1", "p2", "p1", 0).unwrap();
        start_trip(&db, "t1", "b2", "hz", 60, GATE + 1000);
        start_trip(&db, "t2", "b2", "hz", 60, GATE + 5000);
        start_trip(&db, "next-day", "b2", "hz", 60, GATE + CREDIT_DAY + 1000);
        start_trip(&db, "sent-home", "b2", "hz", 60, GATE + 2 * CREDIT_DAY);
        db.record_sent_home("sent-home", GATE + 2 * CREDIT_DAY + 10).unwrap();
        start_trip(&db, "under-way", "b2", "hz", 60, now - 60);

        let paid = credits_of(&db, "p2", now);
        assert_eq!((paid.balance, paid.good_trips), (10, 2));
    }

    // HC3: a bot visiting a house its owner lives in isn't a visit
    #[test]
    fn a_trip_to_a_house_the_owner_lives_in_pays_nobody() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        // Bo lives at `ha` in one city and at `hq` in another, so his bot going to `hq` is going home.
        city(&db, "c2", "sq2", "p1");
        db.add_city_member("c2", "p2", 0).unwrap();
        house(&db, "hq", "c2", "p1", 0);
        assert!(db.add_resident("hq", "p2", 0).unwrap());
        been_to_a_gathering(&db, "p1", "c2", "sq2", 100);
        db.add_vouch("c2", "p2", "p1", 0).unwrap();
        start_trip(&db, "own-house", "b2", "hq", 60, GATE + 1000);

        let paid = credits_of(&db, "p2", GATE + CREDIT_DAY);
        assert_eq!((paid.balance, paid.good_trips), (0, 0));
    }

    // HC5: no back pay, so waiting out the 30 days earns nothing
    #[test]
    fn only_what_happens_after_someone_meets_the_gate_pays_them() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        been_to_a_gathering(&db, "p1", "c1", "sq", 100);
        db.add_vouch("c1", "p2", "p1", 0).unwrap();
        start_trip(&db, "day-5", "b2", "hz", 60, 5 * CREDIT_DAY);
        start_trip(&db, "day-31", "b2", "hz", 60, 31 * CREDIT_DAY);

        let paid = credits_of(&db, "p2", 32 * CREDIT_DAY);
        assert_eq!((paid.balance, paid.good_trips), (5, 1));
    }

    // HC5
    #[test]
    fn a_city_pays_nothing_without_a_gathering_or_a_vouch_however_long_someone_has_been_in_it() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        been_to_a_gathering(&db, "p1", "c1", "sq", 100);
        start_trip(&db, "before", "b2", "hz", 60, 50 * CREDIT_DAY);
        let vouched = 60 * CREDIT_DAY;

        let unpaid = credits_of(&db, "p2", vouched);
        assert_eq!(unpaid.balance, 0);
        assert_eq!(unpaid.cities_not_earning, ["c1"]);

        // A vouch opens the gate from then on. The trip before it still pays nothing.
        db.add_vouch("c1", "p2", "p1", vouched).unwrap();
        start_trip(&db, "after", "b2", "hz", 60, vouched + 1000);
        assert_eq!(credits_of(&db, "p2", vouched + CREDIT_DAY).balance, 5);
    }

    // HC12: what paid stays paid, and a city someone left pays nothing new
    #[test]
    fn leaving_a_city_keeps_what_it_paid_and_stops_it_paying_more() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        plan(&db, "done", "sq", GATE + 100, "confirmed");
        plan(&db, "after-leaving", "sq", GATE + 2 * CREDIT_DAY, "confirmed");
        let now = 31 * CREDIT_DAY;
        assert_eq!(credits_of(&db, "p2", now).balance, 20);

        db.remove_city_member("c1", "p2", "p2", now).unwrap();
        let later = credits_of(&db, "p2", GATE + 3 * CREDIT_DAY);
        assert_eq!((later.balance, later.gatherings), (20, 1));
        assert!(later.cities_not_earning.is_empty());
    }

    // HC11: each thing pays once, however often credits are worked out, and the cap counts what a
    // day already paid
    #[test]
    fn working_credits_out_again_pays_nothing_twice() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        for n in 0..4 {
            plan(&db, &format!("g{n}"), "sq", GATE + 100 + n, "confirmed");
        }
        let day = GATE + 1000;
        for _ in 0..3 {
            assert_eq!(credits_of(&db, "p1", day).balance, 80);
        }
        // Two more the same day: only 20 of the day's 100 is left.
        plan(&db, "g4", "sq", GATE + 500, "confirmed");
        plan(&db, "g5", "sq", GATE + 600, "confirmed");
        let paid = credits_of(&db, "p1", day);
        assert_eq!((paid.balance, paid.gatherings), (100, 5));
    }

    // HC13: a vouch counts only from someone who has been to a gathering there
    #[test]
    fn a_vouch_from_someone_who_has_never_been_to_a_gathering_opens_no_gate() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.add_vouch("c1", "p2", "p1", 0).unwrap();
        start_trip(&db, "first", "b2", "hz", 60, GATE + 1000);
        let unpaid = credits_of(&db, "p2", GATE + CREDIT_DAY);
        assert_eq!(unpaid.balance, 0);
        assert_eq!(unpaid.cities_not_earning, ["c1"]);

        // Once the voucher has been to one, the vouch counts from then.
        been_to_a_gathering(&db, "p1", "c1", "sq", GATE + 2 * CREDIT_DAY);
        start_trip(&db, "second", "b2", "hz", 60, GATE + 3 * CREDIT_DAY);
        let paid = credits_of(&db, "p2", GATE + 4 * CREDIT_DAY);
        assert_eq!((paid.balance, paid.good_trips), (5, 1));
    }

    /// Ann's credits, with `gatherings` of them paid, a day apart so the cap never bites.
    fn ann_with_gatherings(db: &Db, gatherings: i64) -> i64 {
        two_houses(db);
        for n in 0..gatherings {
            plan(db, &format!("g{n}"), "sq", GATE + n * CREDIT_DAY + 100, "confirmed");
        }
        let now = GATE + gatherings * CREDIT_DAY + 1000;
        assert_eq!(credits_of(db, "p1", now).balance, 20 * gatherings);
        now
    }

    fn ann(agent_id: Option<&str>) -> crate::auth::Caller {
        crate::auth::Caller {
            person_id: "p1".into(),
            agent_id: agent_id.map(Into::into),
            name: "Ann".into(),
            secret_hash: String::new(),
            is_house: false,
        }
    }

    fn a_pack_for_sale() -> &'static crate::items::Pack {
        crate::items::PACKS.iter().find(|pack| pack.cost > 0 && !pack.over).unwrap()
    }

    // HC45: a piece bought on its own costs 2 to 10, all of a pack's pieces cost more than the pack,
    // and buying one unlocks only that piece
    #[test]
    fn a_piece_is_bought_on_its_own_and_unlocks_only_itself() {
        use crate::items::{piece_price, PACKS, PIECE};
        for pack in PACKS.iter().filter(|pack| pack.cost > 0) {
            let mut kinds: Vec<&str> = pack.items.iter().map(|item| item.kind).collect();
            kinds.sort();
            kinds.dedup();
            let all: i64 = kinds.iter().map(|kind| piece_price(kind).unwrap()).sum();
            assert!(all > pack.cost, "{}'s pieces cost {all}, no more than the pack", pack.pack);
            assert!(kinds.iter().all(|kind| (2..=10).contains(&piece_price(kind).unwrap())));
            assert_eq!(piece_price(pack.paint), None, "a paint comes only with its pack");
        }
        let db = Db::open(":memory:").unwrap();
        let now = ann_with_gatherings(&db, 1);
        let pack = a_pack_for_sale();
        let piece = pack.items[0].kind;
        let price = piece_price(piece).unwrap();
        let after =
            crate::economy::spend(&db, &ann(None), &format!("{PIECE}{piece}"), None, now).unwrap();
        assert_eq!(after.balance, 20 - price);
        let offer = after.packs.iter().find(|offer| offer.pack == pack.pack).unwrap();
        assert!(!offer.bought, "a piece isn't the pack");
        assert!(offer.sold_alone.iter().any(|alone| alone.kind == piece && alone.bought));
        assert!(db.has_bought("p1", &format!("{PIECE}{piece}")).unwrap());
        // Once is enough, and a piece of a pack you have can't be bought again.
        assert!(crate::economy::spend(&db, &ann(None), &format!("{PIECE}{piece}"), None, now + 1)
            .is_err());
        assert!(
            crate::economy::spend(&db, &ann(None), "piece:nothing_at_all", None, now + 1).is_err()
        );
    }

    // HC14, HC15, HC16
    #[test]
    fn buying_a_pack_spends_its_cost_once_and_only_from_what_was_paid() {
        let db = Db::open(":memory:").unwrap();
        let now = ann_with_gatherings(&db, 1);
        let pack = a_pack_for_sale();
        assert!(pack.cost <= 20, "the test pays 20");
        let spend = |now| crate::economy::spend(&db, &ann(None), pack.pack, None, now);

        let after = spend(now).unwrap();
        assert_eq!((after.balance, after.spent), (20 - pack.cost, pack.cost));
        assert!(after.packs.iter().any(|offer| offer.pack == pack.pack && offer.bought));
        assert!(db.has_bought("p1", pack.pack).unwrap());
        // Once is enough, and spent credits don't come back.
        assert!(spend(now + 1).is_err());
        assert_eq!(credits_of(&db, "p1", now + 2).balance, 20 - pack.cost);
        // A free pack, or one that isn't a pack, can't be bought.
        assert!(crate::economy::spend(&db, &ann(None), "fall", None, now).is_err());
        assert!(crate::economy::spend(&db, &ann(None), "nope", None, now).is_err());
    }

    // HC15: a spend is refused when the balance doesn't cover it
    #[test]
    fn a_pack_costing_more_than_the_balance_is_refused() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        let pack = a_pack_for_sale();
        let refused = db.spend_credits("p1", pack.pack, pack.cost, None, GATE).unwrap();
        assert_eq!(refused, Spend::TooFew { balance: 0 });
        assert!(db.bought_packs("p1").unwrap().is_empty());
    }

    // HC22: the history lists what paid and what was spent, newest first, and only the person's own
    #[test]
    fn credit_history_lists_what_paid_and_what_was_spent_newest_first() {
        let db = Db::open(":memory:").unwrap();
        let now = ann_with_gatherings(&db, 2);
        let pack = a_pack_for_sale();
        let after = crate::economy::spend(&db, &ann(None), pack.pack, None, now).unwrap();

        let lines: Vec<_> =
            after.history.iter().map(|line| (line.kind.as_str(), line.amount)).collect();
        assert_eq!(lines, [("spend", -pack.cost), ("gathering", 20), ("gathering", 20)]);
        assert_eq!(after.history[0].pack.as_deref(), Some(pack.pack));
        assert_eq!(after.history[0].bot, None);
        assert_eq!(after.history[0].at, now);
        assert!(after.history[1].at > after.history[2].at);
        assert!(after.history[1..].iter().all(|line| line.city.as_deref() == Some("c1")));
        assert_eq!(after.history.iter().map(|line| line.amount).sum::<i64>(), after.balance);
        // Bo went to the same gatherings, and sees what paid them but not what Ann spent.
        let bo = credits_of(&db, "p2", now);
        assert!(!bo.history.is_empty());
        assert!(bo.history.iter().all(|line| line.kind == "gathering"));
        assert_eq!(bo.history.iter().map(|line| line.amount).sum::<i64>(), bo.balance);
    }

    // HC17: a bot spends only on its owner's yes, and within the limit they set, which starts at 0
    #[test]
    fn a_bot_spends_only_on_its_owners_yes_and_within_its_limit() {
        let db = Db::open(":memory:").unwrap();
        let now = ann_with_gatherings(&db, 2);
        let pack = a_pack_for_sale();
        let ask = |id: &str, question: &str, answer: Option<&str>| {
            db.insert_owner_question(
                &NewOwnerQuestion {
                    id,
                    hangout_id: "hz-lr",
                    agent_id: "b1",
                    owner_id: "p1",
                    asker: "Scout",
                    question,
                    now: now - 100,
                },
                0,
                100,
            )
            .unwrap();
            if let Some(answer) = answer {
                assert!(db.answer_owner_question(id, answer, now - 50).unwrap());
            }
        };
        let about = format!("May I buy the {} pack for {} credits?", pack.pack, pack.cost);
        ask("unanswered", &about, None);
        ask("said-no", &about, Some("no"));
        ask("other-thing", "May I buy something?", Some("yes"));
        ask("yes", &about, Some("Yes"));
        let bot = ann(Some("b1"));
        let spend =
            |asked: Option<&str>, now| crate::economy::spend(&db, &bot, pack.pack, asked, now);

        for asked in
            [None, Some("unanswered"), Some("said-no"), Some("other-thing"), Some("missing")]
        {
            assert!(spend(asked, now).is_err(), "{asked:?}");
        }
        // A yes isn't enough while the limit is 0, and only the owner can raise it.
        assert!(spend(Some("yes"), now).is_err());
        assert!(crate::economy::set_bot_limit(&db, &bot, "b1", 50, now).is_err());
        assert!(crate::economy::set_bot_limit(&db, &ann(None), "b2", 50, now).is_err());
        let limits = crate::economy::set_bot_limit(&db, &ann(None), "b1", pack.cost, now).unwrap();
        assert!(limits.bots.iter().any(|b| b.agent_id == "b1" && b.daily == pack.cost));

        // A yes from more than a day ago no longer counts.
        assert!(spend(Some("yes"), now + 2 * CREDIT_DAY).is_err());
        let after = spend(Some("yes"), now).unwrap();
        assert_eq!(after.balance, 40 - pack.cost);
        assert!(after.bots.iter().any(|b| b.agent_id == "b1" && b.spent_today == pack.cost));
    }

    // HC19
    #[test]
    fn deleting_an_account_deletes_what_it_was_paid_and_spent() {
        let db = Db::open(":memory:").unwrap();
        let now = ann_with_gatherings(&db, 1);
        crate::economy::spend(&db, &ann(None), a_pack_for_sale().pack, None, now).unwrap();
        db.delete_person("p1", "Ann").unwrap();
        let conn = db.conn();
        for table in ["credit_paid", "credit_spends"] {
            let left: i64 = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE person_id = 'p1'"),
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(left, 0, "{table}");
        }
    }

    // HC4
    #[test]
    fn the_resident_who_let_a_bot_in_is_paid_once_a_day_for_each_visiting_owner() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        been_to_a_gathering(&db, "p2", "c1", "sq", 100);
        db.add_vouch("c1", "p1", "p2", 0).unwrap();
        let hosted = |id: &str, at: i64| {
            db.insert_trip(&NewTrip {
                id,
                agent_id: "b2",
                house_id: "hz",
                kind: "visit",
                goal: "",
                minutes: 60,
                asked_by: "p2",
                owner_ok_by: Some("p2"),
                host_ok_by: None,
                pass_id: None,
                expires_at: at + CREDIT_DAY,
                now: at,
            })
            .unwrap();
            assert!(db.trip_host_ok(id, "p1", at).unwrap());
            db.arrive(id, at).unwrap();
        };
        hosted("t1", GATE + 1000);
        hosted("t2", GATE + 5000);
        // A trip let in some other way, such as a pass, pays no resident.
        start_trip(&db, "no-yes", "b2", "hz", 60, GATE + CREDIT_DAY + 1000);

        let paid = credits_of(&db, "p1", GATE + 3 * CREDIT_DAY);
        assert_eq!((paid.balance, paid.hosted_trips), (5, 1));
    }

    // HC6
    #[test]
    fn nobody_is_credited_more_than_the_daily_cap_for_one_day() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        for n in 0..6 {
            plan(&db, &format!("g{n}"), "sq", GATE + 100 + n, "confirmed");
        }
        plan(&db, "next-day", "sq", GATE + CREDIT_DAY + 100, "confirmed");

        // Five fill the first day's 100, the sixth pays nothing, and the next day starts again.
        let paid = credits_of(&db, "p1", GATE + 3 * CREDIT_DAY);
        assert_eq!(paid.balance, 100 + 20);
        assert_eq!(paid.gatherings, 6);

        // What happened today counts against today.
        let today = GATE + 3 * CREDIT_DAY;
        plan(&db, "today", "sq", today + 100, "confirmed");
        assert_eq!(credits_of(&db, "p1", today + 200).earned_today, 20);
    }

    // HC7
    #[test]
    fn chat_notes_and_bot_activity_never_pay_anything() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.add_vouch("c1", "p1", "p2", 0).unwrap();
        for n in 0..50 {
            say(&db, "sq", "chat", &format!("hi {n}"), GATE + n);
            say(&db, "sq", "note", &format!("note {n}"), GATE + n);
        }
        assert_eq!(credits_of(&db, "p1", 31 * CREDIT_DAY).balance, 0);
    }

    #[test]
    fn roads_join_houses_a_bot_really_arrived_between() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.create_person("p3", "Cy", "key3", "feed3", 0).unwrap();
        db.add_city_member("c1", "p3", 0).unwrap();
        house(&db, "hb", "c1", "p3", 30);
        bot(&db, "b3", "p2");
        bot(&db, "b4", "p3");
        db.set_bot_home("b4", "hb", false, false).unwrap();
        city(&db, "c2", "sq2", "p1");
        house(&db, "hq", "c2", "p1", 40);

        // Bo's bot at Ann's house twice, and Ann's bot at Bo's house: one road, `ha` before `hz`
        // although `hz` was built first.
        start_trip(&db, "t1", "b2", "hz", 60, 1000);
        start_trip(&db, "t2", "b2", "hz", 60, 5000);
        start_trip(&db, "t3", "b1", "ha", 60, 1000);
        // Cy's bot at Ann's house: a second road.
        start_trip(&db, "t4", "b4", "hz", 60, 1000);

        // Trips that never started, a bot with no home, a bot at its own house, and another city.
        agreed_trip(&db, "never-answered", "b2", "hb", 60, 6000);
        agreed_trip(&db, "declined", "b2", "hb", 60, 6000);
        assert!(db.decline_trip("declined", "p3", 6001).unwrap());
        start_trip(&db, "no-home", "b3", "hz", 60, 1000);
        start_trip(&db, "own-house", "b1", "hz", 60, 20_000);
        start_trip(&db, "other-city", "b1", "hq", 60, 30_000);

        let road = |a: &str, b: &str, trips| Road { a: a.to_string(), b: b.to_string(), trips };
        assert_eq!(db.roads("c1").unwrap(), [road("ha", "hz", 3), road("hb", "hz", 1)]);
        assert_eq!(db.roads("c2").unwrap(), []);
        assert_eq!(db.roads("nowhere").unwrap(), []);
    }

    // R41
    #[test]
    fn roads_go_by_where_a_bot_lives_now() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.create_person("p3", "Cy", "key3", "feed3", 0).unwrap();
        db.add_city_member("c1", "p3", 0).unwrap();
        house(&db, "hb", "c1", "p3", 30);
        bot(&db, "b3", "p2");
        let road = |a: &str, b: &str, trips| Road { a: a.to_string(), b: b.to_string(), trips };

        // A bot with no home arrives, and that draws nothing.
        start_trip(&db, "t1", "b3", "hz", 60, 1000);
        assert_eq!(db.roads("c1").unwrap(), []);

        // Given a home, its arrival draws a road from there.
        db.set_bot_home("b3", "ha", false, false).unwrap();
        assert_eq!(db.roads("c1").unwrap(), [road("ha", "hz", 1)]);

        // Moved, the road moves with it.
        db.set_bot_home("b3", "hb", false, false).unwrap();
        assert_eq!(db.roads("c1").unwrap(), [road("hb", "hz", 1)]);

        // With no home again, the road goes.
        db.conn().execute("DELETE FROM bot_homes WHERE agent_id = 'b3'", []).unwrap();
        assert_eq!(db.roads("c1").unwrap(), []);
    }

    #[test]
    fn building_a_house_makes_its_rooms() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);

        assert_eq!(db.hangout("hz-lr").unwrap().unwrap().name, "House hz Living Room");
        assert_eq!(db.hangout("hz-porch").unwrap().unwrap().name, "House hz Porch");
        assert_eq!(db.hangout("sq").unwrap().unwrap().name, "c1 Town Square");
        for room in ["hz-lr", "hz-porch"] {
            let people = db.people_in(room).unwrap();
            assert_eq!(people.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["p1"]);
        }
        assert_eq!(db.people_in("sq").unwrap().len(), 2);
        assert_eq!(db.residents("hz").unwrap().len(), 1);
        assert_eq!(db.resident_count("hz").unwrap(), 1);
        assert!(db.is_resident("hz", "p1").unwrap());
        assert!(!db.is_resident("hz", "p2").unwrap());

        db.create_hangout("plain", "Crew", "plain-code", "p1", 0).unwrap();
        for room in ["sq", "hz-lr", "hz-porch", "ha-lr"] {
            assert!(db.is_managed_room(room).unwrap(), "{room}");
        }
        assert!(!db.is_managed_room("plain").unwrap());
        assert!(!db.is_managed_room("nowhere").unwrap());
        assert_eq!(db.house_for_room("hz-porch").unwrap().unwrap().id, "hz");
        assert_eq!(db.house_for_room("hz-lr").unwrap().unwrap().id, "hz");
        assert!(db.house_for_room("sq").unwrap().is_none());
        assert_eq!(db.city_for_square("sq").unwrap().unwrap().id, "c1");
        assert!(db.city_for_square("hz-porch").unwrap().is_none());

        // Houses come back in the order they were built, whatever their ids sort like.
        let built: Vec<_> = db.houses_in_city("c1").unwrap().into_iter().map(|h| h.id).collect();
        assert_eq!(built, ["hz", "ha"]);
        assert_eq!(
            db.city_residents("c1").unwrap(),
            [("hz".to_string(), "p1".to_string()), ("ha".to_string(), "p2".to_string())]
        );
        assert_eq!(db.house_of_person_in_city("c1", "p2").unwrap().unwrap().id, "ha");
        assert!(db.house_of_person_in_city("c1", "p9").unwrap().is_none());
        assert_eq!(db.houses_for_person("p1").unwrap().len(), 1);

        assert_eq!(db.house_by_invite("hz-code").unwrap().unwrap().id, "hz");
        db.set_house_invite("hz", "fresh").unwrap();
        assert!(db.house_by_invite("hz-code").unwrap().is_none());
        assert_eq!(db.house_by_invite("fresh").unwrap().unwrap().id, "hz");

        // What serializes is what the city page may show: no invite code, no room ids.
        let json = serde_json::to_value(db.house("hz").unwrap().unwrap()).unwrap();
        assert_eq!(json, serde_json::json!({"id": "hz", "name": "House hz", "city_id": "c1"}));
        let json = serde_json::to_value(db.city("c1").unwrap().unwrap()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"id": "c1", "name": "c1", "square_id": "sq", "created_by": "p1"})
        );
    }

    #[test]
    fn a_city_that_fails_to_start_leaves_nothing_behind() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        city(&db, "c1", "sq", "p1");

        let clash = NewCity {
            id: "c2",
            name: "Elm",
            invite_code: "c1-code",
            square_id: "sq2",
            square_code: "sq2-code",
            created_by: "p1",
            now: 0,
        };
        assert!(db.create_city(&clash).is_err());
        assert!(db.hangout("sq2").unwrap().is_none());
        assert!(db.city("c2").unwrap().is_none());
        assert_eq!(db.cities_for_person("p1").unwrap().len(), 1);

        assert_eq!(db.city_by_invite("c1-code").unwrap().unwrap().id, "c1");
        db.set_city_invite("c1", "new-code").unwrap();
        assert!(db.city_by_invite("c1-code").unwrap().is_none());
        assert_eq!(db.city_by_invite("new-code").unwrap().unwrap().id, "c1");
    }

    #[test]
    fn joining_a_house_joins_the_city_and_leaving_empties_the_rooms() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.create_person("p3", "Cy", "key3", "feed3", 0).unwrap();
        bot(&db, "b3", "p3");

        db.add_resident("hz", "p3", 50).unwrap();
        db.add_resident("hz", "p3", 51).unwrap();
        assert!(db.is_city_member("c1", "p3").unwrap());
        for room in ["sq", "hz-lr", "hz-porch"] {
            assert!(db.is_member(room, "p3").unwrap(), "{room}");
        }
        assert_eq!(db.resident_count("hz").unwrap(), 2);
        assert!(db.add_resident("nowhere", "p3", 52).is_err());

        for room in ["sq", "hz-lr", "hz-porch"] {
            db.add_agent_to_room(room, "b3").unwrap();
        }
        db.set_bot_home("b3", "hz", false, false).unwrap();
        db.remove_resident("hz", "p3").unwrap();

        for room in ["hz-lr", "hz-porch"] {
            assert!(!db.is_member(room, "p3").unwrap(), "{room}");
            assert!(!db.agent_in_room(room, "b3", 60).unwrap(), "{room}");
        }
        assert!(db.bot_home("b3").unwrap().is_none());
        assert!(!db.is_resident("hz", "p3").unwrap());
        assert_eq!(db.resident_count("hz").unwrap(), 1);
        // Still in the city.
        assert!(db.is_city_member("c1", "p3").unwrap());
        assert!(db.is_member("sq", "p3").unwrap());
        assert!(db.agent_in_room("sq", "b3", 60).unwrap());
        // Other residents' bots stay where they were.
        assert!(db.agent_in_room("hz-lr", "b1", 60).unwrap());
        assert!(db.bot_home("b1").unwrap().is_some());
    }

    #[test]
    fn leaving_a_city_ends_their_bots_trips() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.add_agent_to_room("sq", "b2").unwrap();
        start_trip(&db, "t1", "b2", "hz", 60, 1000);
        agreed_trip(&db, "t2", "b2", "hz", 60, 1000);
        db.insert_pass(&NewPass {
            id: "ps1",
            house_id: "hz",
            agent_id: "b2",
            max_minutes: 60,
            granted_by: "p1",
            now: 1000,
        })
        .unwrap();
        start_trip(&db, "other", "b1", "ha", 60, 1000);

        db.remove_city_member("c1", "p2", "Kevin", 2000).unwrap();
        assert!(!db.is_city_member("c1", "p2").unwrap());
        assert!(!db.is_member("sq", "p2").unwrap());
        assert!(!db.agent_in_room("sq", "b2", 2000).unwrap());
        assert!(!db.agent_in_room("hz-porch", "b2", 2000).unwrap());
        let ended = db.trip("t1", 2000).unwrap().unwrap();
        assert_eq!((ended.status.as_str(), ended.ended_by.as_deref()), ("ended", Some("Kevin")));
        assert_eq!(db.trip("t2", 2000).unwrap().unwrap().status, "declined");
        assert!(db.pass("ps1").unwrap().is_none());
        // Nobody else's trip is touched.
        assert!(db.agent_in_room("ha-porch", "b1", 2000).unwrap());
        assert!(db.is_city_member("c1", "p1").unwrap());
    }

    #[test]
    fn a_bot_that_stays_home_keeps_one_room_and_loses_its_trip() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.add_agent_to_room("sq", "b2").unwrap();
        db.add_agent_to_room("ha-porch", "b2").unwrap();
        start_trip(&db, "t1", "b2", "hz", 60, 1000);
        agreed_trip(&db, "t2", "b2", "hz", 60, 1000);

        db.remove_agent_from_other_rooms("b2", "ha-lr").unwrap();
        assert_eq!(db.end_trips_of_bot("b2", "p2", 1500).unwrap(), 2);
        assert_eq!(room_ids(&db, "b2", 1500), ["ha-lr"]);
        assert_eq!(db.end_trips_of_bot("b2", "p2", 1500).unwrap(), 0);
        assert_eq!(db.trip("t2", 1500).unwrap().unwrap().status, "declined");
    }

    #[test]
    fn a_house_has_one_bot_and_it_is_in_no_rooms() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);

        assert!(db.create_house_bot("hz", "hb1", "p1", "Home", "token-hb1", 100).unwrap());
        assert!(!db.create_house_bot("hz", "hb2", "p1", "Again", "token-hb2", 101).unwrap());
        assert!(db.agent("hb2", 101).unwrap().is_none());
        assert_eq!(db.house_bot("hz", 102).unwrap().unwrap().id, "hb1");
        assert!(db.house_bot("ha", 102).unwrap().is_none());
        assert!(db.hangouts_for_agent("hb1", 102).unwrap().is_empty());
        let home = db.bot_home("hb1").unwrap().unwrap();
        assert_eq!((home.is_house, home.stays_home), (true, false));

        // Changing the home or the flag never turns a bot into the house bot, or back.
        db.set_bot_home("hb1", "hz", true, false).unwrap();
        let home = db.bot_home("hb1").unwrap().unwrap();
        assert_eq!((home.is_house, home.stays_home), (true, true));
        db.set_bot_home("b1", "hz", true, true).unwrap();
        let home = db.bot_home("b1").unwrap().unwrap();
        assert_eq!((home.is_house, home.stays_home), (false, true));
        assert_eq!(db.house_bot("hz", 102).unwrap().unwrap().id, "hb1");
    }

    #[test]
    fn the_maps_show_where_bots_live_and_where_they_are() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        db.touch_agent_active("b2", 1000).unwrap();
        assert!(db.create_house_bot("hz", "hb1", "p1", "Home", "token-hb1", 100).unwrap());
        start_trip(&db, "t1", "b2", "hz", 60, 1000);

        let city_map = db.bots_in_city("c1", 1500).unwrap();
        // House bots stay off the map: when one wakes would say when someone's home.
        let ids: Vec<_> = city_map.iter().map(|b| b.id.as_str()).collect();
        assert_eq!(ids, ["b1", "b2"]);
        let away = &city_map[1];
        assert_eq!(away.home_house_id.as_deref(), Some("ha"));
        assert_eq!(away.away_at_house_id.as_deref(), Some("hz"));
        assert_eq!(away.away_until, Some(4600));
        assert_eq!((away.state, away.stays_home, away.is_house), ("active", false, false));
        assert_eq!(city_map[0].away_at_house_id, None);
        // Once the trip runs out the bot is simply home.
        let home = db.bots_in_city("c1", 4600).unwrap();
        assert_eq!((home[1].away_at_house_id.clone(), home[1].away_until), (None, None));

        let house_map = db.bots_of_house("ha", 1500).unwrap();
        assert_eq!(house_map.len(), 1);
        assert_eq!(house_map[0].away_at_house_id.as_deref(), Some("hz"));
        let ids: Vec<_> = db.bots_of_house("hz", 1500).unwrap().into_iter().map(|b| b.id).collect();
        assert_eq!(ids, ["b1", "hb1"]);

        // Another city shows neither where these bots live nor where they're visiting.
        city(&db, "c2", "sq2", "p1");
        db.add_city_member("c2", "p2", 0).unwrap();
        let other = db.bots_in_city("c2", 1500).unwrap();
        assert_eq!(other.len(), 2);
        for bot in &other {
            assert_eq!((bot.home_house_id.clone(), bot.away_at_house_id.clone()), (None, None));
        }
    }

    #[test]
    fn a_scene_request_lapses_on_its_own_clock() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        assert!(db.insert_scene("s1", "hz", "Movie Night", 0).unwrap());
        assert!(!db.insert_scene("s2", "hz", "Movie Night", 1).unwrap());
        assert!(db.insert_scene("s3", "ha", "Movie Night", 1).unwrap());
        assert_eq!(db.scenes("hz").unwrap().len(), 1);

        let ask = |id: &str, status: &'static str, expires_at: i64, now: i64| {
            db.insert_scene_request(&NewSceneRequest {
                id,
                scene_id: "s1",
                house_id: "hz",
                asked_by: "b1",
                status,
                answered_by: None,
                expires_at,
                now,
            })
            .unwrap()
        };
        let status = |id: &str, now: i64| db.scene_request(id, now).unwrap().unwrap().status;

        // Waiting lapses at its time, and only one request per scene is open at once.
        assert!(ask("r1", "waiting", 900, 0));
        assert!(!ask("r2", "waiting", 950, 10));
        assert_eq!(db.open_scene_request("s1", 899).unwrap().unwrap().id, "r1");
        assert_eq!(status("r1", 899), "waiting");
        assert_eq!(status("r1", 900), "expired");
        assert!(db.open_scene_request("s1", 900).unwrap().is_none());
        assert!(!db.answer_scene_request("r1", "approved", "p1", Some(1500), 900).unwrap());

        // An approval gets a shorter life from when it was given, and the queue honours it.
        assert!(ask("r3", "waiting", 1500, 900));
        assert!(db.answer_scene_request("r3", "approved", "p1", Some(1100), 1000).unwrap());
        assert!(!db.answer_scene_request("r3", "declined", "p1", None, 1001).unwrap());
        assert_eq!(status("r3", 1099), "approved");
        assert_eq!(status("r3", 1100), "expired");
        assert_eq!(
            db.scene_queue("hz", 1099).unwrap(),
            [QueueItem { id: "r3".to_string(), scene: "Movie Night".to_string() }]
        );
        assert!(db.scene_queue("hz", 1100).unwrap().is_empty());
        assert!(db.scene_queue("ha", 1099).unwrap().is_empty());
        assert!(!db.finish_scene_request("r3", "done", 1100).unwrap());
        assert!(db.finish_scene_request("r3", "done", 1099).unwrap());
        assert_eq!(status("r3", 1099), "done");
        assert!(db.scene_queue("hz", 1099).unwrap().is_empty());

        // A resident asking for themselves is approved from the start, and a no is final.
        assert!(db
            .insert_scene_request(&NewSceneRequest {
                id: "r4",
                scene_id: "s1",
                house_id: "hz",
                asked_by: "p1",
                status: "approved",
                answered_by: Some("p1"),
                expires_at: 1700,
                now: 1100,
            })
            .unwrap());
        assert_eq!(db.scene_queue("hz", 1100).unwrap().len(), 1);
        assert!(db.finish_scene_request("r4", "failed", 1101).unwrap());
        assert!(ask("r5", "waiting", 2000, 1200));
        assert!(db.answer_scene_request("r5", "declined", "p1", None, 1201).unwrap());
        let declined = db.scene_request("r5", 1202).unwrap().unwrap();
        assert_eq!((declined.status.as_str(), declined.expires_at), ("declined", 2000));
        assert_eq!(declined.answered_by.as_deref(), Some("p1"));

        let recent = db.recent_scene_requests("hz", 1000, 3000).unwrap();
        let ids: Vec<_> = recent.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r5", "r4"]);
        assert_eq!(db.recent_scene_requests("hz", 0, 3000).unwrap().len(), 4);

        // Removing a scene takes its requests with it.
        assert!(db.delete_scene("s1").unwrap());
        assert!(db.scene_request("r5", 1202).unwrap().is_none());
        assert!(!db.delete_scene("s1").unwrap());
    }

    #[test]
    fn a_house_status_is_ok_or_attention() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        assert_eq!(db.house_status("hz").unwrap(), None);
        db.set_house_status("hz", "ok", 10).unwrap();
        db.set_house_status("hz", "attention", 20).unwrap();
        let status = db.house_status("hz").unwrap().unwrap();
        assert_eq!((status.state.as_str(), status.updated_at), ("attention", 20));
        assert!(db.set_house_status("hz", "locked", 30).is_err());
        assert_eq!(db.house_status("ha").unwrap(), None);
    }

    #[test]
    fn trips_are_listed_for_the_house_the_bot_and_its_owner() {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        let now = 100_000;
        // Over, lapsed, still waiting from long ago, under way since long ago, and just asked.
        start_trip(&db, "old", "b2", "hz", 60, 100);
        agreed_trip(&db, "lapsed", "b2", "hz", 60, 100);
        agreed_trip(&db, "open-old", "b2", "hz", 60, 50_000);
        start_trip(&db, "stay", "b2", "hz", 7 * 24 * 60, 60_000);
        db.insert_trip(&NewTrip {
            id: "recent",
            agent_id: "b2",
            house_id: "hz",
            kind: "call",
            goal: "Say hi",
            minutes: 5,
            asked_by: "b2",
            owner_ok_by: None,
            host_ok_by: None,
            pass_id: None,
            expires_at: now + 100,
            now: now - 10,
        })
        .unwrap();

        let since = now - 60 * 60;
        let ids = |trips: Vec<Trip>| trips.into_iter().map(|t| t.id).collect::<Vec<_>>();
        // Old trips only show while they're open or under way, newest first.
        let listed = ["recent", "stay", "open-old"];
        assert_eq!(ids(db.trips_for_house("hz", since, now).unwrap()), listed);
        assert_eq!(ids(db.trips_for_house("hz", 0, now).unwrap()).len(), 5);
        assert_eq!(ids(db.trips_for_bot("b2", since, now).unwrap()), listed);
        assert_eq!(ids(db.trips_for_owner("p2", since, now).unwrap()), listed);
        assert!(db.trips_for_owner("p1", 0, now).unwrap().is_empty());
        assert!(db.trips_for_house("ha", 0, now).unwrap().is_empty());

        let recent = db.trip("recent", now).unwrap().unwrap();
        assert_eq!(recent.goal, "Say hi");
        assert_eq!((recent.agent_name.as_str(), recent.owner_id.as_str()), ("b2", "p2"));
        assert_eq!(recent.house_name, "House hz");
        assert_eq!(recent.status, "asking");
    }

    fn new_passkey<'a>(id: &'a str, credential_id: &'a str) -> NewPasskey<'a> {
        NewPasskey { id, credential_id, passkey: "{\"counter\":0}", label: "Phone" }
    }

    #[test]
    fn a_session_lasts_until_it_lapses_and_the_cap_ends_the_oldest() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_session("old", "p1", 100, 0).unwrap();
        assert_eq!(db.person_by_session("old", 99).unwrap().unwrap().name, "Ann");
        assert!(db.person_by_session("old", 100).unwrap().is_none(), "lapsed at its time");
        assert!(db.person_by_session("nope", 0).unwrap().is_none());

        // A new session clears the lapsed ones, and past the cap the oldest go first.
        for n in 0..MAX_SESSIONS_PER_PERSON + 5 {
            db.create_session(&format!("s{n}"), "p1", 10_000, 200 + n).unwrap();
        }
        assert_eq!(db.session_count("p1").unwrap(), MAX_SESSIONS_PER_PERSON);
        assert!(db.person_by_session("old", 0).unwrap().is_none());
        assert!(db.person_by_session("s0", 300).unwrap().is_none(), "the oldest went");
        assert!(db.person_by_session("s5", 300).unwrap().is_some());
        assert!(db
            .person_by_session(&format!("s{}", MAX_SESSIONS_PER_PERSON + 4), 300)
            .unwrap()
            .is_some());

        db.delete_session("s5").unwrap();
        assert!(db.person_by_session("s5", 300).unwrap().is_none());
    }

    #[test]
    fn replacing_the_owner_key_ends_every_session_and_keeps_passkeys() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        db.user_handle("p1", "handle1").unwrap();
        db.add_passkey("p1", &new_passkey("k1", "c1"), 5, 0).unwrap();
        db.create_session("a", "p1", 1_000, 0).unwrap();
        db.create_session("b", "p1", 1_000, 0).unwrap();
        db.create_session("c", "p2", 1_000, 0).unwrap();

        db.replace_owner_key("p1", "key1-new").unwrap();
        assert!(db.person_by_key("key1").unwrap().is_none());
        assert_eq!(db.person_by_key("key1-new").unwrap().unwrap().id, "p1");
        assert_eq!(db.session_count("p1").unwrap(), 0);
        assert!(db.person_by_session("c", 1).unwrap().is_some(), "someone else's is untouched");
        assert_eq!(db.passkey_count("p1").unwrap(), 1);
    }

    #[test]
    fn a_person_keeps_one_handle_for_good() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        assert_eq!(db.user_handle("p1", "first").unwrap(), "first");
        assert_eq!(db.user_handle("p1", "second").unwrap(), "first");
        assert_eq!(db.user_handle("p2", "other").unwrap(), "other");
    }

    #[test]
    fn a_credential_is_stored_once_for_anyone() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        db.user_handle("p1", "h1").unwrap();
        db.user_handle("p2", "h2").unwrap();
        assert_eq!(
            db.add_passkey("p1", &new_passkey("k1", "cred"), 2, 5).unwrap(),
            PasskeyAdded::Added
        );
        assert_eq!(
            db.add_passkey("p2", &new_passkey("k2", "cred"), 2, 5).unwrap(),
            PasskeyAdded::AlreadyRegistered
        );
        assert_eq!(
            db.add_passkey("p1", &new_passkey("k3", "cred"), 2, 5).unwrap(),
            PasskeyAdded::AlreadyRegistered
        );
        assert_eq!(
            db.add_passkey("p1", &new_passkey("k4", "two"), 2, 6).unwrap(),
            PasskeyAdded::Added
        );
        assert_eq!(
            db.add_passkey("p1", &new_passkey("k5", "three"), 2, 7).unwrap(),
            PasskeyAdded::TooMany
        );

        let found = db.passkey_by_credential("cred").unwrap().unwrap();
        assert_eq!(
            (found.id.as_str(), found.person_id.as_str(), found.user_handle.as_str()),
            ("k1", "p1", "h1")
        );
        assert!(db.passkey_by_credential("missing").unwrap().is_none());
        assert_eq!(db.credential_ids_of("p1").unwrap(), ["cred", "two"]);
        assert_eq!(db.passkeys_of("p2").unwrap(), []);

        // Only their own can be removed.
        assert!(!db.remove_passkey("p2", "k1", "").unwrap());
        assert!(db.remove_passkey("p1", "k1", "").unwrap());
        assert!(!db.remove_passkey("p1", "k1", "").unwrap());
        assert_eq!(db.passkey_count("p1").unwrap(), 1);
    }

    #[test]
    fn removing_a_passkey_ends_the_persons_other_sessions_and_nobody_elses() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        db.user_handle("p1", "h1").unwrap();
        db.add_passkey("p1", &new_passkey("k1", "c1"), 5, 0).unwrap();
        db.add_passkey("p1", &new_passkey("k2", "c2"), 5, 0).unwrap();
        for (session, person) in [("a", "p1"), ("b", "p1"), ("c", "p2")] {
            db.create_session(session, person, 1_000, 0).unwrap();
        }

        // Not hers to remove: nothing is ended.
        assert!(!db.remove_passkey("p1", "nope", "a").unwrap());
        assert_eq!(db.session_count("p1").unwrap(), 2);

        assert!(db.remove_passkey("p1", "k1", "a").unwrap());
        assert!(db.person_by_session("a", 1).unwrap().is_some(), "the one that asked stays");
        assert!(db.person_by_session("b", 1).unwrap().is_none());
        assert!(db.person_by_session("c", 1).unwrap().is_some(), "someone else's is untouched");

        // An owner key made the request, so no session is the one to keep.
        assert!(db.remove_passkey("p1", "k2", "owner-key-hash").unwrap());
        assert_eq!(db.session_count("p1").unwrap(), 0);
    }

    #[test]
    fn a_sign_up_that_fails_leaves_no_account() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.user_handle("p1", "h1").unwrap();
        db.add_passkey("p1", &new_passkey("k1", "cred"), 5, 0).unwrap();

        let refused = db
            .create_account_with_passkey(
                "p2",
                "Bo",
                "key2",
                "feed2",
                "h2",
                &new_passkey("k2", "cred"),
                1,
            )
            .unwrap();
        assert!(refused.is_none());
        assert_eq!(db.person_count().unwrap(), 1);
        assert!(db.person("p2").unwrap().is_none());
        assert!(
            db.user_handle("p2", "later").is_err(),
            "no handle was kept for the person who wasn't made"
        );
    }

    #[test]
    fn a_sign_up_makes_the_account_the_handle_and_the_passkey_together() {
        let db = Db::open(":memory:").unwrap();
        let person = db
            .create_account_with_passkey(
                "p1",
                "Ann",
                "key1",
                "feed1",
                "h1",
                &new_passkey("k1", "cred"),
                3,
            )
            .unwrap()
            .unwrap();
        assert_eq!(person.name, "Ann");
        assert_eq!(db.person_by_key("key1").unwrap().unwrap().id, "p1");
        assert_eq!(db.passkey_by_credential("cred").unwrap().unwrap().user_handle, "h1");
        assert_eq!(db.passkeys_of("p1").unwrap()[0].created_at, 3);
    }

    #[test]
    fn an_account_and_its_link_go_in_together_or_not_at_all() {
        let db = Db::open(":memory:").unwrap();
        let make = |id: &str, name: &str, key: &str, feed: &str, site: &str| {
            db.create_account_with_sign_in(id, name, key, feed, site, "1001", "ann-codes", 3)
                .unwrap()
        };
        assert_eq!(make("p1", "Ann", "key1", "feed1", "github").unwrap().name, "Ann");
        assert_eq!(db.person_by_key("key1").unwrap().unwrap().id, "p1");
        assert_eq!(
            db.sign_in_links("p1").unwrap(),
            [("github".to_string(), "ann-codes".to_string())]
        );

        assert!(make("p2", "Bo", "key2", "feed2", "github").is_none(), "that account is taken");
        assert!(db.person("p2").unwrap().is_none());
        assert_eq!(db.person_count().unwrap(), 1);

        // The same id on the other site is a different account.
        assert!(make("p2", "Bo", "key2", "feed2", "discord").is_some());
    }

    #[test]
    fn a_sign_in_only_lands_if_the_passkey_is_the_one_that_was_checked() {
        let db = Db::open(":memory:").unwrap();
        db.create_account_with_passkey(
            "p1",
            "Ann",
            "key1",
            "feed1",
            "h1",
            &new_passkey("k1", "cred"),
            3,
        )
        .unwrap();
        let checked = db.passkey_by_credential("cred").unwrap().unwrap().passkey;

        // A passkey that moved on (its counter) while another sign-in was being checked is refused.
        assert!(db.record_sign_in("k1", &checked, Some("{\"counter\":5}"), 10).unwrap());
        assert!(!db.record_sign_in("k1", &checked, Some("{\"counter\":4}"), 11).unwrap());
        assert!(!db.record_sign_in("k1", &checked, None, 11).unwrap());
        let now = db.passkey_by_credential("cred").unwrap().unwrap().passkey;
        assert_eq!(now, "{\"counter\":5}");
        assert!(db.record_sign_in("k1", &now, None, 12).unwrap());
        assert_eq!(db.passkeys_of("p1").unwrap()[0].last_used_at, Some(12));
    }

    fn bot_player(db: &Db, agent: &str, owner: &str, token: &str, now: i64) -> PlayerRow {
        match db.join_as_bot(agent, owner, token, "commons", now).unwrap() {
            BotJoined::Done(player) => player,
            other => panic!("the bot should have joined: {other:?}"),
        }
    }

    #[test]
    fn replacing_a_bots_token_ends_its_players_token_and_the_owner_keeps_one_player_of_their_own() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        db.create_agent("a2", "p1", "Rover", "token2", 0).unwrap();
        let own = db.own_player("p1", 1).unwrap();
        assert_eq!(own.kind(), PlayerKind::Person);
        let player = bot_player(&db, "a1", "p1", "first", 2);
        bot_player(&db, "a2", "p1", "other", 2);
        assert_eq!(player.kind(), PlayerKind::Bot);
        assert!(db.player_by_token("first").unwrap().is_some());

        db.set_agent_token("a1", "new-token").unwrap();
        db.set_agent_token("a2", "new-token2").unwrap();
        assert!(db.player_by_token("first").unwrap().is_none());
        let after = db.player(player.id).unwrap().unwrap();
        assert_eq!((after.kind(), after.has_token, after.removed), (PlayerKind::Bot, false, false));
        // A bot's player with no token is never taken for the person's own.
        assert_eq!(db.own_player("p1", 3).unwrap().id, own.id);
        // Joining again brings back the same player with a token of its own.
        let back = bot_player(&db, "a1", "p1", "second", 4);
        assert_eq!(back.id, player.id);
        assert!(db.player_by_token("second").unwrap().is_some());
    }

    #[test]
    fn a_look_goes_with_whoever_wore_it() {
        let db = Db::open(":memory:").unwrap();
        for person in ["p1", "p2"] {
            db.create_person(
                person,
                person,
                &format!("key-{person}"),
                &format!("feed-{person}"),
                0,
            )
            .unwrap();
        }
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        db.change_look(Wearer::Person, "p2", &[("hat", Some("cap"))]).unwrap();
        db.change_look(Wearer::Bot, "a1", &[("hat", Some("crown")), ("shell", Some("teal"))])
            .unwrap();
        // Set twice is still one row, and clearing one leaves the other.
        db.change_look(Wearer::Bot, "a1", &[("hat", Some("cap"))]).unwrap();
        db.change_look(Wearer::Bot, "a1", &[("shell", None)]).unwrap();
        let ids = ["a1".to_string()];
        assert_eq!(
            db.looks(Wearer::Bot, &ids).unwrap()["a1"],
            BTreeMap::from([("hat".to_string(), "cap".to_string())])
        );

        db.conn().execute("DELETE FROM agents WHERE id = 'a1'", []).unwrap();
        db.conn().execute("DELETE FROM people WHERE id = 'p2'", []).unwrap();
        assert!(db.looks(Wearer::Bot, &ids).unwrap().is_empty());
        assert!(db.looks(Wearer::Person, &["p2".to_string()]).unwrap().is_empty());
    }

    #[test]
    fn deleting_a_bot_or_its_person_ends_its_player_and_leaves_what_it_built() {
        let db = Db::open(":memory:").unwrap();
        for person in ["p1", "p2"] {
            db.create_person(
                person,
                person,
                &format!("key-{person}"),
                &format!("feed-{person}"),
                0,
            )
            .unwrap();
        }
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        db.create_agent("a2", "p2", "Rover", "token2", 0).unwrap();
        let first = bot_player(&db, "a1", "p1", "first", 2);
        let second = bot_player(&db, "a2", "p2", "second", 2);
        db.conn().execute("DELETE FROM agents WHERE id = 'a1'", []).unwrap();
        db.conn().execute("DELETE FROM people WHERE id = 'p2'", []).unwrap();
        for id in [first.id, second.id] {
            // The foreign key let go of the bot, which would have left a claimed player with a token.
            let after = db.player(id).unwrap().unwrap();
            assert!(after.agent_id.is_none() && after.removed, "player {id}: {after:?}");
        }
        assert!(db.player_by_token("first").unwrap().is_none());
        assert!(db.player_by_token("second").unwrap().is_none());
        assert!(db.players_of("p1").unwrap().is_empty());
    }

    #[test]
    fn a_bot_barred_from_a_world_is_refused_before_its_token_is_touched() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Justin", "key", "feed", 0).unwrap();
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        let commons = NewWorld {
            id: "commons",
            name: "The Commons",
            kind: "public",
            seed: 1,
            size: 64,
            owner_id: None,
            open: true,
            invite_code: None,
            highway: &[],
            now: 1,
        };
        db.insert_world(&commons, 1).unwrap();
        let player = bot_player(&db, "a1", "p1", "first", 2);
        db.remove_from_world("commons", player.id, true, false).unwrap();
        assert!(matches!(
            db.join_as_bot("a1", "p1", "second", "commons", 3).unwrap(),
            BotJoined::Barred
        ));
        assert!(db.player_by_token("first").unwrap().is_some(), "the token it had is as it was");
        assert!(db.player_by_token("second").unwrap().is_none());
        // Barred from one world is not barred from another.
        assert!(matches!(
            db.join_as_bot("a1", "p1", "third", "elsewhere", 3).unwrap(),
            BotJoined::Done(_)
        ));
    }

    #[test]
    fn a_player_ended_by_the_person_who_claimed_it_keeps_what_it_built() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key", "feed", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        let guest = db.create_guest("token", "claim", 1).unwrap();
        let own = db.own_player("p1", 1).unwrap();
        // A guest nobody claimed, and a person's own player, can't be ended this way.
        assert!(!db.end_claimed_player(guest.id, "p1").unwrap());
        assert!(!db.end_claimed_player(own.id, "p1").unwrap());
        assert!(matches!(db.claim_player("claim", "p1", 20, 0).unwrap(), Claimed::Done(_)));
        assert!(!db.end_claimed_player(guest.id, "p2").unwrap(), "not Bo's");
        assert!(db.end_claimed_player(guest.id, "p1").unwrap());
        assert!(!db.end_claimed_player(guest.id, "p1").unwrap(), "only once");
        assert!(db.player_by_token("token").unwrap().is_none());
        assert!(db.player(guest.id).unwrap().is_some());
    }

    #[test]
    fn a_world_is_only_added_where_there_is_room_for_it() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key", "feed", 0).unwrap();
        let new = |id, owner| NewWorld {
            id,
            name: "Quiet Harbour",
            kind: "own",
            seed: 1,
            size: 64,
            owner_id: owner,
            open: true,
            invite_code: None,
            highway: &[],
            now: 1,
        };
        assert_eq!(db.insert_world(&new("w1", Some("p1")), 1).unwrap(), WorldAdded::Added);
        assert_eq!(db.insert_world(&new("w2", Some("p1")), 2).unwrap(), WorldAdded::OwnerHasOne);
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        assert_eq!(db.insert_world(&new("w2", Some("p2")), 1).unwrap(), WorldAdded::Full);
        assert_eq!(db.insert_world(&new("w2", Some("p2")), 2).unwrap(), WorldAdded::Added);
    }

    #[test]
    fn every_new_player_starts_with_a_random_name_seed_of_its_own() {
        let db = Db::open(":memory:").unwrap();
        db.create_person("p1", "Ann", "key", "feed", 0).unwrap();
        db.create_agent("a1", "p1", "Scout", "token", 0).unwrap();
        let players = [
            db.create_guest("t1", "c1", 1).unwrap(),
            db.own_player("p1", 1).unwrap(),
            bot_player(&db, "a1", "p1", "t2", 1),
        ];
        for player in &players {
            assert_ne!(player.name_seed, player.id, "{player:?}");
            assert!(player.name_seed > 0);
        }
    }

    /// Ann's space "s1", its first floor and the attic "f2", with Bo on the first floor.
    fn space_with_an_attic(db: &Db) {
        db.create_person("p1", "Ann", "key1", "feed1", 0).unwrap();
        db.create_person("p2", "Bo", "key2", "feed2", 0).unwrap();
        db.start_third_space("s1", "Studio", "ground-code", "p1", 0, 3).unwrap().unwrap();
        let attic = NewFloor {
            id: "f2",
            space_id: "s1",
            name: "Attic",
            invite_code: "attic-code",
            created_by: "p1",
            now: 0,
        };
        db.add_floor(&attic, 5).unwrap().unwrap();
        db.add_member("s1", "p2", 0).unwrap();
    }

    #[test]
    fn leaving_a_floor_is_kept_whatever_the_room_says_afterwards() {
        let db = Db::open(":memory:").unwrap();
        space_with_an_attic(&db);
        db.add_member("f2", "p2", 1).unwrap();
        db.remove_member("f2", "p2").unwrap();

        let mut floors = floors_seen_by(&db.conn(), "s1", "p2", "no such line").unwrap();
        floors.sort();
        assert_eq!(floors, ["f2", "s1"]);
    }

    // S27: anyone who left before `floors_left` is only in the room's history.
    #[test]
    fn a_line_only_the_server_writes_says_who_left_and_one_anyone_could_type_doesnt() {
        let db = Db::open(":memory:").unwrap();
        space_with_an_attic(&db);
        let left = "Bo left the hangout.";
        let seen = || {
            let mut floors = floors_seen_by(&db.conn(), "s1", "p2", left).unwrap();
            floors.sort();
            floors
        };
        let line = |kind: &str| {
            let sql = "INSERT INTO messages (hangout_id, speaker, kind, body, created_at)
                       VALUES ('f2', 'Bo', ?, ?, 1)";
            db.conn().execute(sql, params![kind, left]).unwrap();
        };
        assert_eq!(seen(), ["s1"]);

        line("chat");
        assert_eq!(seen(), ["s1"], "anyone could type that");

        line("system");
        assert_eq!(seen(), ["f2", "s1"]);
    }
    // HC20/HC21: reserves, awards and refunds conserve the same credits as pack spends.
    fn funded_question_fixture() -> Db {
        let db = Db::open(":memory:").unwrap();
        two_houses(&db);
        {
            let conn = db.conn();
            conn.execute("INSERT INTO boards(id,hangout_id,kind,name,created_by,created_at) VALUES('board','sq','discussion','Questions','p1',0)", []).unwrap();
            for card in ["q1", "q2"] {
                conn.execute("INSERT INTO board_cards(id,board_id,title,body,person_id,created_at,updated_at) VALUES(?,'board','Question','','p1',0,0)", [card]).unwrap();
            }
            conn.execute("INSERT INTO credit_paid(person_id,kind,source_id,city_id,amount,day,created_at) VALUES('p1','gathering','seed','c1',20,0,0)", []).unwrap();
        }
        db
    }

    #[test]
    fn funded_question_pays_once_and_refunds_on_card_deletion() {
        let db = funded_question_fixture();
        assert_eq!(db.fund_question("bq1", "q1", "p1", 12, 1).unwrap(), None);
        assert_eq!(
            db.credit_totals("p1", 0).unwrap().paid - db.credit_totals("p1", 0).unwrap().spent,
            8
        );
        assert!(db.fund_question("bq2", "q2", "p1", 9, 1).unwrap().is_some());
        assert!(db.answer_question("self", "q1", "b1", "p1", "answer", 1).unwrap().is_some());
        assert_eq!(db.answer_question("answer", "q1", "b2", "p2", "evidence", 1).unwrap(), None);
        assert!(db.resolve_question("q1", "p2", Some("answer")).unwrap().is_some());
        assert_eq!(db.resolve_question("q1", "p1", Some("answer")).unwrap(), None);
        assert!(db.resolve_question("q1", "p1", Some("answer")).unwrap().is_some());
        assert_eq!(db.credit_totals("p2", 0).unwrap().paid, 12);
        db.conn().execute("DELETE FROM board_cards WHERE id='q1'", []).unwrap();
        let answers: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM question_answers WHERE bounty_id='bq1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(answers, 0);
        assert_eq!(db.credit_totals("p2", 0).unwrap().paid, 12);
        assert_eq!(db.fund_question("bq2", "q2", "p1", 8, 1).unwrap(), None);
        db.conn().execute("DELETE FROM boards WHERE id='board'", []).unwrap();
        let totals = db.credit_totals("p1", 0).unwrap();
        assert_eq!(totals.paid - totals.spent, 8);
    }

    // HC28: the site pays a starter question's bounty, so its sponsor spends nothing and a cancel
    // pays them nothing back. They can have one at a time, and a cancelled one frees the slot.
    #[test]
    fn a_starter_question_is_paid_by_the_site_one_per_sponsor() {
        let db = funded_question_fixture();
        let balance = |person| {
            let totals = db.credit_totals(person, 0).unwrap();
            totals.paid - totals.spent
        };
        assert_eq!(db.starter_question_refusal("p1", 1).unwrap(), None);
        assert_eq!(db.fund_starter_question("s1", "q1", "p1", 1).unwrap(), None);
        assert_eq!(balance("p1"), 20, "the sponsor spent nothing");
        assert!(db.open_questions("p1").unwrap().is_empty(), "nothing of theirs is reserved");
        assert_eq!(db.question_bounty("q1").unwrap().unwrap()["starter"], true);
        assert_eq!(
            db.fund_starter_question("s2", "q2", "p1", 2).unwrap(),
            Some("you've already sponsored a starter question")
        );

        assert_eq!(db.resolve_question("q1", "p1", None).unwrap(), None);
        assert_eq!(balance("p1"), 20, "a cancel pays nothing back");
        assert!(db
            .credit_history("p1", 50)
            .unwrap()
            .iter()
            .all(|line| line.kind != "question_refund"));
        assert_eq!(db.fund_starter_question("s2", "q2", "p1", 3).unwrap(), None);
    }

    // HC28: a person is paid by one starter question at most, and that award buys packs but never
    // funds a question, so throwaway sponsors can't pool the site's credits in one account.
    #[test]
    fn a_starter_award_pays_once_and_never_funds_a_question() {
        let db = funded_question_fixture();
        db.create_person("p3", "Cy", "key3", "feed3", 0).unwrap();
        db.conn().execute("INSERT INTO board_cards(id,board_id,title,body,person_id,created_at,updated_at) VALUES('q3','board','Question','','p3',0,0)", []).unwrap();
        db.conn().execute("INSERT INTO board_cards(id,board_id,title,body,person_id,created_at,updated_at) VALUES('q4','board','Question','','p2',0,0)", []).unwrap();
        assert_eq!(db.fund_starter_question("s1", "q1", "p1", 1).unwrap(), None);
        assert_eq!(db.answer_question("a1", "q1", "b2", "p2", "evidence", 1).unwrap(), None);
        assert_eq!(db.resolve_question("q1", "p1", Some("a1")).unwrap(), None);
        assert_eq!(db.credit_totals("p2", 0).unwrap().paid, STARTER_QUESTION_HC);

        // A second sponsor can't pay Bo another starter award.
        assert_eq!(db.fund_starter_question("s3", "q3", "p3", 2).unwrap(), None);
        assert_eq!(db.answer_question("a3", "q3", "b2", "p2", "more", 2).unwrap(), None);
        assert_eq!(
            db.resolve_question("q3", "p3", Some("a3")).unwrap(),
            Some("that answer's owner has already had a starter question's award")
        );
        assert_eq!(db.credit_totals("p2", 0).unwrap().paid, STARTER_QUESTION_HC);

        // Bo can't pass the award on through a question of his own.
        assert_eq!(
            db.fund_question("b4", "q4", "p2", 5, 3).unwrap(),
            Some("credits from your first gathering, a starter question or a contribution buy packs but can't fund a question")
        );
    }

    // HC28: the whole site funds only so many starter questions a week, however many people or
    // cities ask, and the week moves on.
    #[test]
    fn the_site_funds_only_so_many_starter_questions_a_week() {
        let db = funded_question_fixture();
        for n in 0..STARTER_QUESTIONS_A_WEEK {
            let person = format!("sp{n}");
            let card = format!("sq{n}");
            db.create_person(&person, "Sp", &format!("{person}-key"), &format!("{person}-feed"), 0)
                .unwrap();
            db.conn().execute("INSERT INTO board_cards(id,board_id,title,body,person_id,created_at,updated_at) VALUES(?1,'board','Question','',?2,0,0)", params![card, person]).unwrap();
            assert_eq!(
                db.fund_starter_question(&format!("s{n}"), &card, &person, 100).unwrap(),
                None
            );
        }
        assert_eq!(
            db.fund_starter_question("over", "q2", "p1", 200).unwrap(),
            Some("this week's starter questions are all taken")
        );
        let next_week = 100 + 7 * crate::ops::DAY_SECS + 1;
        assert_eq!(db.fund_starter_question("over", "q2", "p1", next_week).unwrap(), None);
    }

    #[test]
    fn competing_funding_and_resolution_cannot_duplicate_credits() {
        let db = std::sync::Arc::new(funded_question_fixture());
        let a = db.clone();
        let b = db.clone();
        let x = std::thread::spawn(move || a.fund_question("a", "q1", "p1", 15, 1).unwrap());
        let y = std::thread::spawn(move || b.fund_question("b", "q2", "p1", 15, 1).unwrap());
        let results = [x.join().unwrap(), y.join().unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_none()).count(), 1);
        let card = if db.question_bounty("q1").unwrap().is_some() { "q1" } else { "q2" };
        db.answer_question("answer", card, "b2", "p2", "evidence", 1).unwrap();
        let a = db.clone();
        let b = db.clone();
        let x = std::thread::spawn(move || a.resolve_question(card, "p1", Some("answer")).unwrap());
        let y = std::thread::spawn(move || b.resolve_question(card, "p1", None).unwrap());
        let results = [x.join().unwrap(), y.join().unwrap()];
        assert_eq!(results.iter().filter(|r| r.is_none()).count(), 1);
        let p1 = db.credit_totals("p1", 0).unwrap();
        let p2 = db.credit_totals("p2", 0).unwrap();
        assert_eq!(p1.paid - p1.spent + p2.paid - p2.spent, 20);
    }

    #[test]
    fn a_pack_and_a_question_cannot_spend_the_same_credits() {
        let db = std::sync::Arc::new(funded_question_fixture());
        let a = db.clone();
        let b = db.clone();
        let x = std::thread::spawn(move || {
            a.fund_question("reserve", "q1", "p1", 15, 1).unwrap().is_none()
        });
        let y = std::thread::spawn(move || {
            matches!(b.spend_credits("p1", "test-pack", 15, None, 1).unwrap(), Spend::Spent { .. })
        });
        assert_ne!(x.join().unwrap(), y.join().unwrap());
        let total = db.credit_totals("p1", 0).unwrap();
        assert_eq!(total.paid - total.spent, 5);
    }
    #[test]
    fn deleting_a_contributor_redacts_their_answer_without_paying_twice() {
        let db = funded_question_fixture();
        db.fund_question("bq", "q1", "p1", 10, 1).unwrap();
        db.answer_question("answer", "q1", "b2", "p2", "private words", 1).unwrap();
        db.resolve_question("q1", "p1", Some("answer")).unwrap();
        db.delete_person("p2", "Bo").unwrap();
        let bounty = db.question_bounty("q1").unwrap().unwrap();
        assert_eq!(bounty["status"], "paid");
        assert!(!bounty.to_string().contains("private words"));
        assert!(!bounty.to_string().contains("p2"));
        assert!(db.resolve_question("q1", "p1", Some("answer")).unwrap().is_some());
    }
}
