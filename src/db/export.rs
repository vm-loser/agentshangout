//! RE19 (docs/reports.md): what a person's own data export reads. Every query names the columns
//! it takes, so nothing is exported by accident: no key, token, session, wake-up URL or its key,
//! passkey, invite code, photo or anyone else's words. Rooms come back as their ids, and the caller
//! names only the ones the person can still see.
use super::*;
use serde_json::{Map, Value};

/// RE19: the most rows any one list holds.
pub const EXPORT_ROWS: usize = 10_000;

/// RE19: one list, newest first, and whether it was cut to `EXPORT_ROWS`.
pub struct Listed {
    pub rows: Vec<Value>,
    pub truncated: bool,
}

/// RE19: a person's own rows, by list. A row's room is under `hangout_id`.
pub struct ExportRows {
    pub person: Option<Value>,
    pub lists: Vec<(&'static str, Listed)>,
    /// RE19: each bot, with its own settings.
    pub bots: Vec<Value>,
    pub credits: Vec<CreditLine>,
}

fn to_json(value: rusqlite::types::ValueRef) -> Value {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Integer(n) => Value::from(n),
        ValueRef::Real(n) => Value::from(n),
        ValueRef::Text(text) => Value::from(String::from_utf8_lossy(text).into_owned()),
        // No list selects a blob, and none is ever exported.
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
    }
}

/// Runs `sql` with the person's id as `?1`, at most `EXPORT_ROWS` rows, each as an object keyed by
/// its column names. A column holding JSON is named with a `json_` prefix, and is parsed.
fn listed(conn: &Connection, sql: &str, id: &str) -> rusqlite::Result<Listed> {
    let sql = format!("{sql} LIMIT {}", EXPORT_ROWS + 1);
    let mut stmt = conn.prepare(&sql)?;
    let names: Vec<String> = stmt.column_names().map(str::to_string).collect();
    let mut rows = stmt.query([id])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = to_json(row.get_ref(index)?);
            match name.strip_prefix("json_") {
                Some(name) => {
                    let parsed = value
                        .as_str()
                        .and_then(|text| serde_json::from_str(text).ok())
                        .unwrap_or(Value::Null);
                    object.insert(name.to_string(), parsed);
                }
                None => {
                    object.insert(name.clone(), value);
                }
            }
        }
        out.push(Value::Object(object));
    }
    let truncated = out.len() > EXPORT_ROWS;
    out.truncate(EXPORT_ROWS);
    Ok(Listed { rows: out, truncated })
}

/// RE19: what each list reads, by the name it has in the export. `?1` is the person's id.
const LISTS: &[(&str, &str)] = &[
    ("account.looks", "SELECT part, choice FROM person_looks WHERE person_id = ?1 ORDER BY part"),
    (
        "account.passkeys",
        "SELECT created_at, last_used_at FROM passkeys WHERE person_id = ?1 ORDER BY created_at",
    ),
    (
        "account.linked",
        "SELECT site AS method, linked_at AS since FROM sign_in_links WHERE person_id = ?1
         ORDER BY site",
    ),
    (
        "account.rules_accepted",
        "SELECT version, accepted_at FROM rules_acceptances WHERE person_id = ?1
         ORDER BY accepted_at DESC",
    ),
    (
        "account.notices",
        "SELECT kind, text, created_at, seen_at FROM moderation_notices WHERE person_id = ?1
         ORDER BY id DESC",
    ),
    ("account.holds", "SELECT hold, at FROM person_holds WHERE person_id = ?1 ORDER BY at DESC"),
    (
        "messages",
        "SELECT m.id, m.hangout_id, m.kind, m.body AS text, m.created_at, m.agent_id AS bot_id,
                a.name AS bot
           FROM messages m LEFT JOIN agents a ON a.id = m.agent_id
          WHERE m.kind IN ('chat', 'note')
            AND ((m.person_id = ?1 AND m.agent_id IS NULL)
                 OR m.agent_id IN (SELECT id FROM agents WHERE owner_id = ?1))
          ORDER BY m.id DESC",
    ),
    (
        "item_notes",
        "SELECT n.id, i.hangout_id, n.body AS text, n.via_agent AS bot_id, n.created_at
           FROM item_notes n JOIN floor_items i ON i.id = n.item_id
          WHERE n.person_id = ?1 ORDER BY n.created_at DESC",
    ),
    (
        "whiteboards",
        "SELECT hangout_id, body AS text, agent_id AS bot_id, updated_at FROM whiteboards
          WHERE person_id = ?1 ORDER BY updated_at DESC",
    ),
    (
        "pins",
        "SELECT id, hangout_id, kind, title, url, caption, image_alt, agent_id AS bot_id,
                pinned_at, expires_at
           FROM room_pins WHERE person_id = ?1 ORDER BY pinned_at DESC",
    ),
    (
        "board_cards",
        "SELECT c.id, b.hangout_id, b.name AS board, c.title, c.body AS text, c.status,
                c.agent_id AS bot_id, c.created_at, c.updated_at
           FROM board_cards c JOIN boards b ON b.id = c.board_id
          WHERE c.person_id = ?1 ORDER BY c.created_at DESC",
    ),
    (
        "plans",
        "SELECT id, hangout_id, title, location, notes, starts_at, ends_at, status, created_at,
                updated_at
           FROM events WHERE proposed_by = ?1 ORDER BY created_at DESC",
    ),
    (
        "check_ins",
        "SELECT id, hangout_id, about, agent_id AS bot_id, status, expires_at, created_at
           FROM check_ins WHERE asked_by = ?1 ORDER BY created_at DESC",
    ),
    (
        "owner_questions",
        "SELECT id, hangout_id, agent_id AS bot_id, question, answer, created_at, answered_at
           FROM owner_questions WHERE owner_id = ?1 ORDER BY created_at DESC",
    ),
    (
        "question_answers",
        "SELECT id, bounty_id, bot_id, text, created_at FROM question_answers
          WHERE owner_id = ?1 ORDER BY created_at DESC",
    ),
    (
        "friends",
        "SELECT p.name, f.status, f.created_at AS since,
                CASE WHEN f.from_id = ?1 THEN 'asked' ELSE 'were_asked' END AS who_asked
           FROM explicit_friendships f
           JOIN people p ON p.id = CASE WHEN f.from_id = ?1 THEN f.to_id ELSE f.from_id END
          WHERE f.kind = 'person' AND (f.from_id = ?1 OR f.to_id = ?1)
          ORDER BY f.created_at DESC",
    ),
    (
        "blocks",
        "SELECT p.name, b.created_at AS since FROM person_blocks b
           JOIN people p ON p.id = b.blocked_id
          WHERE b.blocker_id = ?1 ORDER BY b.created_at DESC",
    ),
    (
        "mutes",
        "SELECT p.name, m.created_at AS since FROM person_mutes m
           JOIN people p ON p.id = m.muted_id
          WHERE m.muter_id = ?1 ORDER BY m.created_at DESC",
    ),
    (
        "places",
        "SELECT id, kind, name, about, created_at FROM spaces WHERE owner_id = ?1
          ORDER BY created_at DESC",
    ),
    (
        "rooms_started",
        "SELECT id AS hangout_id, created_at FROM hangouts WHERE created_by = ?1
          ORDER BY created_at DESC",
    ),
    (
        "purchases",
        "SELECT s.pack, s.amount, a.name AS bot, s.created_at FROM credit_spends s
           LEFT JOIN agents a ON a.id = s.agent_id
          WHERE s.person_id = ?1 ORDER BY s.id DESC",
    ),
    (
        "photos",
        "SELECT id, hangout_id, created_at AS taken_at, expires_at,
                json_object('caption', json_extract(metadata, '$.caption'),
                            'frame', json_extract(metadata, '$.frame')) AS json_about
           FROM room_photos WHERE owner_id = ?1 ORDER BY created_at DESC",
    ),
    (
        "reports_filed",
        "SELECT id, target_kind, reason,
                CASE WHEN status = 'open' THEN 'received' ELSE 'reviewed' END AS status,
                created_at
           FROM reports WHERE reporter_id = ?1 ORDER BY id DESC",
    ),
];

/// RE19: what each of a person's bots exports. `?1` is the bot's id.
const BOT_LISTS: &[(&str, &str)] = &[
    ("look", "SELECT part, choice FROM bot_looks WHERE agent_id = ?1 ORDER BY part"),
    (
        "routine",
        "SELECT days, starts_min, ends_min, max_social_runs, public_places, updated_at
           FROM bot_routines WHERE agent_id = ?1",
    ),
    (
        "schedule",
        "SELECT every_day AS json_every_day, days AS json_days, updated_at FROM bot_schedules
          WHERE agent_id = ?1",
    ),
    ("role", "SELECT body AS text, updated_at FROM bot_roles WHERE agent_id = ?1"),
    ("job", "SELECT hangout_id, job, set_at FROM bot_workplaces WHERE agent_id = ?1"),
    ("trust", "SELECT settings AS json_settings FROM bot_trust_settings WHERE agent_id = ?1"),
    ("suspended", "SELECT at FROM bot_suspensions WHERE agent_id = ?1"),
];

impl Db {
    /// RE19: everything a person's export holds, read in one go.
    pub fn export_rows(&self, person_id: &str) -> rusqlite::Result<ExportRows> {
        let credits = self.credit_history(person_id, EXPORT_ROWS as i64 + 1)?;
        let conn = self.conn();
        let person = conn
            .query_row(
                "SELECT name, color, created_at FROM people WHERE id = ?",
                [person_id],
                |row| {
                    Ok(serde_json::json!({
                        "name": row.get::<_, String>(0)?,
                        "color": row.get::<_, String>(1)?,
                        "created_at": row.get::<_, i64>(2)?,
                    }))
                },
            )
            .optional()?;
        let mut lists = Vec::new();
        for (name, sql) in LISTS {
            lists.push((*name, listed(&conn, sql, person_id)?));
        }
        let mut bots = Vec::new();
        let found = listed(
            &conn,
            "SELECT a.id, a.name, a.status, a.created_at, a.last_active_at,
                    EXISTS (SELECT 1 FROM notify_targets t WHERE t.owner_id = a.id)
                      AS has_wake_up_url,
                    EXISTS (SELECT 1 FROM wake_settings w WHERE w.agent_id = a.id AND w.only_me = 1)
                      AS wakes_only_for_me,
                    EXISTS (SELECT 1 FROM agent_doorbell d WHERE d.agent_id = a.id AND d.paused = 1)
                      AS wake_ups_paused
               FROM agents a WHERE a.owner_id = ?1 ORDER BY a.created_at",
            person_id,
        )?;
        for mut bot in found.rows {
            let id = bot["id"].as_str().unwrap_or_default().to_string();
            for flag in ["has_wake_up_url", "wakes_only_for_me", "wake_ups_paused"] {
                bot[flag] = Value::Bool(bot[flag].as_i64() == Some(1));
            }
            for (name, sql) in BOT_LISTS {
                let mut rows = listed(&conn, sql, &id)?.rows;
                bot[*name] = if *name == "look" {
                    Value::Array(rows)
                } else if *name == "trust" {
                    rows.pop().map_or(Value::Null, |mut row| row["settings"].take())
                } else {
                    rows.pop().unwrap_or(Value::Null)
                };
            }
            bots.push(bot);
        }
        Ok(ExportRows { person, lists, bots, credits })
    }
}
