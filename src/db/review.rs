//! RE8 to RE18 (docs/reports.md): the operators' side of reports. The queue reads what RE1 to RE7
//! filed; closing a group, holding a bot or a person, the notices, the log and the copy's retention
//! are written here, each step that belongs together in one transaction.
use super::*;

/// A report as the queue reads it (RE8).
#[derive(Debug, Clone)]
pub struct QueueReport {
    pub id: i64,
    pub reporter_id: Option<String>,
    pub via_bot_id: Option<String>,
    pub by_bot: bool,
    pub target_kind: String,
    pub target_id: Option<String>,
    pub target_person_id: Option<String>,
    pub hangout_id: Option<String>,
    pub reason: String,
    pub details: Option<String>,
    pub snapshot: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub closed_at: Option<i64>,
}

const QUEUE_COLS: &str = "id, reporter_id, via_bot_id, by_bot, target_kind, target_id,
    target_person_id, hangout_id, reason, details, snapshot, status, created_at, closed_at";

fn queue_report(row: &Row) -> rusqlite::Result<QueueReport> {
    Ok(QueueReport {
        id: row.get(0)?,
        reporter_id: row.get(1)?,
        via_bot_id: row.get(2)?,
        by_bot: row.get(3)?,
        target_kind: row.get(4)?,
        target_id: row.get(5)?,
        target_person_id: row.get(6)?,
        hangout_id: row.get(7)?,
        reason: row.get(8)?,
        details: row.get(9)?,
        snapshot: row.get(10)?,
        status: row.get(11)?,
        created_at: row.get(12)?,
        closed_at: row.get(13)?,
    })
}

/// RE10: what closing a group does besides closing it, written in the same transaction.
pub enum Hold<'a> {
    None,
    /// `suspend_bot`: this bot can't act.
    Bot {
        agent_id: &'a str,
    },
    /// `restrict_person` or `ban_person`, by that name.
    Person {
        person_id: &'a str,
        hold: &'a str,
    },
}

/// RE14: the one notice an action sends, to whom and saying what.
pub struct Tell<'a> {
    pub person_id: &'a str,
    pub kind: &'a str,
    pub text: &'a str,
}

/// RE10: one closing, as an operator asked for it.
pub struct Closing<'a> {
    pub report_id: i64,
    /// `actioned` or `dismissed`.
    pub status: &'a str,
    pub action: &'a str,
    pub note: Option<&'a str>,
    pub operator_id: &'a str,
    /// The bot or person the action reached, for the log.
    pub subject_id: Option<&'a str>,
    pub person_id: Option<&'a str>,
    pub hold: Hold<'a>,
    pub tell: Option<Tell<'a>>,
}

/// RE12: a bot's suspension or a person's restriction or ban, as the operators' list shows it.
#[derive(Debug, Clone)]
pub struct HoldRow {
    pub hold: String,
    pub subject_id: String,
    pub person_id: String,
    pub reason: String,
    pub report_id: Option<i64>,
    pub at: i64,
}

/// RE11: what holds bear on a caller right now.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Holds {
    pub bot_suspended: bool,
    pub restricted: bool,
    pub banned: bool,
}

/// RE14, RE15: a notice waiting to be seen.
#[derive(Debug, Clone, Serialize)]
pub struct ModerationNotice {
    pub id: i64,
    pub kind: String,
    pub report_id: i64,
    pub text: String,
    pub at: i64,
}

/// RE16: what a release found.
#[derive(Debug, PartialEq, Eq)]
pub enum Released {
    Released,
    NotFound,
    StillOpen,
    NotMinorSafety,
    AlreadyReleased,
}

/// RE17: a deleted account's id comes off the log the way RE6 takes it off reports: everywhere but
/// on rows about a minor_safety report.
pub(super) fn blank_person(conn: &Connection, person_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE report_actions SET operator_id = NULL
         WHERE operator_id = ?1 AND minor_safety = 0",
        [person_id],
    )?;
    conn.execute(
        "UPDATE report_actions SET person_id = NULL WHERE person_id = ?1 AND minor_safety = 0",
        [person_id],
    )?;
    conn.execute(
        "UPDATE report_actions SET subject_id = NULL WHERE subject_id = ?1 AND minor_safety = 0",
        [person_id],
    )?;
    Ok(())
}

fn is_minor_safety(conn: &Connection, report_id: Option<i64>) -> rusqlite::Result<bool> {
    let Some(id) = report_id else { return Ok(false) };
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM reports WHERE id = ? AND reason = 'minor_safety')",
        [id],
        |row| row.get(0),
    )
}

impl Db {
    /// RE8: every open report, minor_safety first and then oldest first.
    pub fn open_reports(&self, limit: i64) -> rusqlite::Result<Vec<QueueReport>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT {QUEUE_COLS} FROM reports WHERE status = 'open'
             ORDER BY reason = 'minor_safety' DESC, created_at, id LIMIT ?"
        ))?;
        let rows = stmt.query_map([limit], queue_report)?;
        rows.collect()
    }

    /// One report, open or closed.
    pub fn queue_report(&self, id: i64) -> rusqlite::Result<Option<QueueReport>> {
        self.conn()
            .query_row(
                &format!("SELECT {QUEUE_COLS} FROM reports WHERE id = ?"),
                [id],
                queue_report,
            )
            .optional()
    }

    /// RE9, RE10, RE13, RE14: closes the open reports in this report's group, logs each one, and
    /// makes the hold and the notice, all at once. None when the report isn't open any more.
    pub fn close_report_group(
        &self,
        closing: &Closing,
        now: i64,
    ) -> rusqlite::Result<Option<Vec<i64>>> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let first: Option<(String, Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT target_kind, target_id, snapshot FROM reports
                 WHERE id = ? AND status = 'open'",
                [closing.report_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((kind, target_id, snapshot)) = first else {
            return Ok(None);
        };
        // RE9: the same target, or the same copy once the target's id was blanked.
        let group: Vec<(i64, bool)> = {
            let mut stmt = tx.prepare(
                "SELECT id, reason = 'minor_safety' FROM reports
                 WHERE status = 'open' AND target_kind = ?1
                   AND ((?2 IS NOT NULL AND target_id = ?2)
                        OR (?2 IS NULL AND target_id IS NULL AND snapshot IS ?3))
                 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![kind, target_id, snapshot], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for (id, minor_safety) in &group {
            tx.execute(
                "UPDATE reports SET status = ?2, closed_at = ?3, updated_at = ?3 WHERE id = ?1",
                params![id, closing.status, now],
            )?;
            tx.execute(
                "INSERT INTO report_actions (report_id, operator_id, action, note, subject_id,
                                             person_id, minor_safety, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    closing.operator_id,
                    closing.action,
                    closing.note,
                    closing.subject_id,
                    closing.person_id,
                    minor_safety,
                    now
                ],
            )?;
        }
        match closing.hold {
            Hold::None => {}
            Hold::Bot { agent_id } => {
                tx.execute(
                    "INSERT OR IGNORE INTO bot_suspensions (agent_id, reason, report_id,
                                                            operator_id, at)
                     SELECT id, (SELECT reason FROM reports WHERE id = ?2), ?2, ?3, ?4
                     FROM agents WHERE id = ?1",
                    params![agent_id, closing.report_id, closing.operator_id, now],
                )?;
            }
            Hold::Person { person_id, hold } => {
                tx.execute(
                    "INSERT OR IGNORE INTO person_holds (person_id, hold, reason, report_id,
                                                         operator_id, at)
                     SELECT id, ?2, (SELECT reason FROM reports WHERE id = ?3), ?3, ?4, ?5
                     FROM people WHERE id = ?1",
                    params![person_id, hold, closing.report_id, closing.operator_id, now],
                )?;
            }
        }
        if let Some(tell) = &closing.tell {
            tx.execute(
                "INSERT OR IGNORE INTO moderation_notices (person_id, kind, report_id, text,
                                                           created_at)
                 SELECT id, ?2, ?3, ?4, ?5 FROM people WHERE id = ?1",
                params![tell.person_id, tell.kind, closing.report_id, tell.text, now],
            )?;
        }
        tx.commit()?;
        Ok(Some(group.into_iter().map(|(id, _)| id).collect()))
    }

    /// RE11: the holds on a person, and on their bot when a bot is asking.
    pub fn holds_on(&self, person_id: &str, agent_id: Option<&str>) -> rusqlite::Result<Holds> {
        let conn = self.conn();
        let (restricted, banned): (bool, bool) = conn.query_row(
            "SELECT
               EXISTS (SELECT 1 FROM person_holds WHERE person_id = ?1 AND hold = 'restrict_person'),
               EXISTS (SELECT 1 FROM person_holds WHERE person_id = ?1 AND hold = 'ban_person')",
            [person_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let bot_suspended = match agent_id {
            Some(agent_id) => conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM bot_suspensions WHERE agent_id = ?)",
                [agent_id],
                |row| row.get(0),
            )?,
            None => false,
        };
        Ok(Holds { bot_suspended, restricted, banned })
    }

    /// RE11: whether any hold exists at all, so a request needn't be looked at when none does.
    pub fn any_holds(&self) -> rusqlite::Result<bool> {
        self.conn().query_row(
            "SELECT EXISTS (SELECT 1 FROM person_holds) OR EXISTS (SELECT 1 FROM bot_suspensions)",
            [],
            |row| row.get(0),
        )
    }

    /// RE12: every hold in force, oldest first.
    pub fn all_holds(&self) -> rusqlite::Result<Vec<HoldRow>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT 'suspend_bot', s.agent_id, a.owner_id, s.reason, s.report_id, s.at
               FROM bot_suspensions s JOIN agents a ON a.id = s.agent_id
             UNION ALL
             SELECT hold, person_id, person_id, reason, report_id, at FROM person_holds
             ORDER BY 6, 2",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(HoldRow {
                hold: row.get(0)?,
                subject_id: row.get(1)?,
                person_id: row.get(2)?,
                reason: row.get(3)?,
                report_id: row.get(4)?,
                at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    /// RE12, RE13: lifts a hold and logs it in one step. False when there was no such hold.
    pub fn lift_hold(
        &self,
        hold: &str,
        subject_id: &str,
        operator_id: &str,
        note: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let found: Option<(Option<i64>, String)> = if hold == "suspend_bot" {
            tx.query_row(
                "SELECT s.report_id, a.owner_id FROM bot_suspensions s
                 JOIN agents a ON a.id = s.agent_id WHERE s.agent_id = ?",
                [subject_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
        } else {
            tx.query_row(
                "SELECT report_id, person_id FROM person_holds WHERE person_id = ?1 AND hold = ?2",
                params![subject_id, hold],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
        };
        let Some((report_id, person_id)) = found else {
            return Ok(false);
        };
        if hold == "suspend_bot" {
            tx.execute("DELETE FROM bot_suspensions WHERE agent_id = ?", [subject_id])?;
        } else {
            tx.execute(
                "DELETE FROM person_holds WHERE person_id = ?1 AND hold = ?2",
                params![subject_id, hold],
            )?;
        }
        let minor = is_minor_safety(&tx, report_id)?;
        tx.execute(
            "INSERT INTO report_actions (report_id, operator_id, action, note, subject_id,
                                         person_id, minor_safety, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                report_id,
                operator_id,
                format!("lift_{hold}"),
                note,
                subject_id,
                person_id,
                minor,
                now
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// RE14, RE15: this person's notices they haven't seen, oldest first.
    pub fn moderation_notices(&self, person_id: &str) -> rusqlite::Result<Vec<ModerationNotice>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, kind, report_id, text, created_at FROM moderation_notices
             WHERE person_id = ? AND seen_at IS NULL ORDER BY id LIMIT 50",
        )?;
        let rows = stmt.query_map([person_id], |row| {
            Ok(ModerationNotice {
                id: row.get(0)?,
                kind: row.get(1)?,
                report_id: row.get(2)?,
                text: row.get(3)?,
                at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// RE14: marks a notice seen, so it is shown once. False when it isn't this person's.
    pub fn see_moderation_notice(
        &self,
        id: i64,
        person_id: &str,
        now: i64,
    ) -> rusqlite::Result<bool> {
        Ok(self.conn().execute(
            "UPDATE moderation_notices SET seen_at = COALESCE(seen_at, ?3)
             WHERE id = ?1 AND person_id = ?2",
            params![id, person_id, now],
        )? > 0)
    }

    /// RE15: one alert for each of these people about this report, once each. Gives back the ones
    /// newly alerted, so only they are notified.
    pub fn alert_operators(
        &self,
        report_id: i64,
        operators: &[String],
        text: &str,
        now: i64,
    ) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut alerted = Vec::new();
        for person_id in operators {
            let added = conn.execute(
                "INSERT OR IGNORE INTO moderation_notices (person_id, kind, report_id, text,
                                                           created_at)
                 SELECT id, 'report_alert', ?2, ?3, ?4 FROM people WHERE id = ?1",
                params![person_id, report_id, text, now],
            )?;
            if added > 0 {
                alerted.push(person_id.clone());
            }
        }
        Ok(alerted)
    }

    /// RE16: drops the copy and details of every report closed at or before `before`, except a
    /// minor_safety one no operator has released. Gives back how many were dropped.
    pub fn drop_old_report_copies(&self, before: i64) -> rusqlite::Result<usize> {
        self.conn().execute(
            "UPDATE reports SET snapshot = NULL, details = NULL
             WHERE status <> 'open' AND closed_at IS NOT NULL AND closed_at <= ?1
               AND (snapshot IS NOT NULL OR details IS NOT NULL)
               AND (reason <> 'minor_safety'
                    OR EXISTS (SELECT 1 FROM report_actions a
                               WHERE a.report_id = reports.id AND a.action = 'release'))",
            [before],
        )
    }

    /// RE16: closed minor_safety reports whose copy is kept because nobody released it yet.
    pub fn held_report_copies(&self) -> rusqlite::Result<Vec<(i64, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, closed_at FROM reports r
             WHERE status <> 'open' AND reason = 'minor_safety'
               AND (snapshot IS NOT NULL OR details IS NOT NULL)
               AND NOT EXISTS (SELECT 1 FROM report_actions a
                               WHERE a.report_id = r.id AND a.action = 'release')
             ORDER BY closed_at, id LIMIT 200",
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect()
    }

    /// RE16, RE13: an operator releases a closed minor_safety report's copy to the usual timer.
    pub fn release_report(
        &self,
        id: i64,
        operator_id: &str,
        note: Option<&str>,
        now: i64,
    ) -> rusqlite::Result<Released> {
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let found: Option<(String, String)> = tx
            .query_row("SELECT status, reason FROM reports WHERE id = ?", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let Some((status, reason)) = found else { return Ok(Released::NotFound) };
        if reason != "minor_safety" {
            return Ok(Released::NotMinorSafety);
        }
        if status == "open" {
            return Ok(Released::StillOpen);
        }
        let already: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM report_actions WHERE report_id = ? AND action = 'release')",
            [id],
            |row| row.get(0),
        )?;
        if already {
            return Ok(Released::AlreadyReleased);
        }
        tx.execute(
            "INSERT INTO report_actions (report_id, operator_id, action, note, minor_safety, at)
             VALUES (?1, ?2, 'release', ?3, 1, ?4)",
            params![id, operator_id, note, now],
        )?;
        tx.commit()?;
        Ok(Released::Released)
    }

    /// For tests and the record: the log's rows, oldest first, each column as text by name.
    pub fn report_action_rows(&self) -> rusqlite::Result<Vec<BTreeMap<String, String>>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM report_actions ORDER BY id")?;
        let names: Vec<String> = stmt.column_names().map(|n| n.to_string()).collect();
        let rows = stmt.query_map([], |row| {
            let mut out = BTreeMap::new();
            for (i, name) in names.iter().enumerate() {
                let value: rusqlite::types::Value = row.get(i)?;
                let text = match value {
                    rusqlite::types::Value::Null => continue,
                    rusqlite::types::Value::Integer(n) => n.to_string(),
                    rusqlite::types::Value::Real(n) => n.to_string(),
                    rusqlite::types::Value::Text(t) => t,
                    rusqlite::types::Value::Blob(_) => continue,
                };
                out.insert(name.clone(), text);
            }
            Ok(out)
        })?;
        rows.collect()
    }

    /// RE10 `remove_content`: a message goes for everyone. A system line is never a report's.
    pub fn remove_message(&self, id: i64) -> rusqlite::Result<bool> {
        Ok(self.conn().execute("DELETE FROM messages WHERE id = ? AND kind <> 'system'", [id])? > 0)
    }
}
