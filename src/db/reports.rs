//! RE1 to RE7 (docs/reports.md): reports people and their bots file. The review queue that reads
//! them (RE8 to RE18) is in review.rs.
use super::*;

/// One report as it's filed.
pub struct NewReport<'a> {
    pub reporter_id: &'a str,
    pub via_bot_id: Option<&'a str>,
    pub target_kind: &'a str,
    pub target_id: &'a str,
    pub target_person_id: Option<&'a str>,
    pub hangout_id: Option<&'a str>,
    pub reason: &'a str,
    pub details: Option<&'a str>,
    pub snapshot: &'a str,
}

/// What filing did.
#[derive(Debug, PartialEq, Eq)]
pub enum Filed {
    /// A new report, which counts toward the day (RE4).
    New(i64),
    /// The reporter's open report on this target, brought up to date (RE3).
    Updated(i64),
    /// RE4: the person's reports today, with their bots', are at the cap.
    PersonFull,
    /// RE4: their bots' reports today are at the bots' share of it.
    BotsFull,
}

/// A report as its reporter sees it (RE5): never the action or who reviewed it.
#[derive(Debug, Clone)]
pub struct MyReport {
    pub id: i64,
    pub target_kind: String,
    pub reason: String,
    pub reviewed: bool,
    pub created_at: i64,
}

/// RE6: a deleted account's id comes off the reports it filed and the ones about it, in the
/// deletion's own transaction, except on a minor_safety report, which keeps everything. On a report
/// about the person themselves, `target_id` is their id, so it goes too.
pub(super) fn blank_person(conn: &Connection, person_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE reports SET reporter_id = NULL, via_bot_id = NULL
         WHERE reporter_id = ?1 AND reason <> 'minor_safety'",
        [person_id],
    )?;
    conn.execute(
        "UPDATE reports SET target_person_id = NULL
         WHERE target_person_id = ?1 AND reason <> 'minor_safety'",
        [person_id],
    )?;
    conn.execute(
        "UPDATE reports SET target_id = NULL
         WHERE target_kind = 'person' AND target_id = ?1 AND reason <> 'minor_safety'",
        [person_id],
    )?;
    Ok(())
}

impl Db {
    /// RE3, RE4: files a report, or updates the reporter's open one on the same target. The caps
    /// are counted in the same immediate transaction as the insert, so requests at once can't pass
    /// them together. An update never changes who filed it (`by_bot`, `via_bot_id`), never lowers a
    /// minor_safety reason, and a bot's never changes what its person filed themselves.
    pub fn file_report(
        &self,
        report: &NewReport,
        now: i64,
        per_day: i64,
        bots_per_day: i64,
    ) -> rusqlite::Result<Filed> {
        let day = now.div_euclid(86_400);
        let mut conn = self.conn();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let open: Option<(i64, bool)> = tx
            .query_row(
                "SELECT id, by_bot FROM reports WHERE reporter_id = ?1 AND target_kind = ?2
                   AND target_id = ?3 AND status = 'open'",
                params![report.reporter_id, report.target_kind, report.target_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, filed_by_bot)) = open {
            // RE3: a bot doesn't speak over its person. What they filed themselves stays theirs.
            if report.via_bot_id.is_none() || filed_by_bot {
                tx.execute(
                    "UPDATE reports SET
                         reason = CASE WHEN reason = 'minor_safety' THEN reason ELSE ?2 END,
                         details = ?3, snapshot = ?4, updated_at = ?5
                     WHERE id = ?1",
                    params![id, report.reason, report.details, report.snapshot, now],
                )?;
            }
            tx.commit()?;
            return Ok(Filed::Updated(id));
        }
        let (all, by_bots): (i64, i64) = tx.query_row(
            "SELECT COUNT(*), COALESCE(SUM(by_bot), 0) FROM reports
             WHERE reporter_id = ?1 AND day = ?2",
            params![report.reporter_id, day],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if all >= per_day {
            return Ok(Filed::PersonFull);
        }
        if report.via_bot_id.is_some() && by_bots >= bots_per_day {
            return Ok(Filed::BotsFull);
        }
        tx.execute(
            "INSERT INTO reports (reporter_id, via_bot_id, by_bot, target_kind, target_id,
                                  target_person_id, hangout_id, reason, details, snapshot,
                                  created_at, updated_at, day)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12)",
            params![
                report.reporter_id,
                report.via_bot_id,
                report.via_bot_id.is_some(),
                report.target_kind,
                report.target_id,
                report.target_person_id,
                report.hangout_id,
                report.reason,
                report.details,
                report.snapshot,
                now,
                day,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(Filed::New(id))
    }

    /// RE5: the reports this person filed, with their bots', newest first.
    pub fn reports_by(&self, reporter_id: &str) -> rusqlite::Result<Vec<MyReport>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, target_kind, reason, status <> 'open', created_at FROM reports
             WHERE reporter_id = ? ORDER BY id DESC LIMIT 200",
        )?;
        let rows = stmt.query_map([reporter_id], |row| {
            Ok(MyReport {
                id: row.get(0)?,
                target_kind: row.get(1)?,
                reason: row.get(2)?,
                reviewed: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        rows.collect()
    }

    /// For tests and the review queue: one report's columns as text, by name.
    pub fn report_row(&self, id: i64) -> rusqlite::Result<Option<BTreeMap<String, String>>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT * FROM reports WHERE id = ?")?;
        let names: Vec<String> = stmt.column_names().map(|n| n.to_string()).collect();
        stmt.query_row([id], |row| {
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
        })
        .optional()
    }

    /// For tests: closes a report as the review queue would.
    pub fn set_report_status(&self, id: i64, status: &str, at: i64) -> rusqlite::Result<()> {
        self.conn().execute(
            "UPDATE reports SET status = ?2, closed_at = ?3, updated_at = ?3 WHERE id = ?1",
            params![id, status, at],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report<'a>(reporter: &'a str, target: &'a str, bot: Option<&'a str>) -> NewReport<'a> {
        NewReport {
            reporter_id: reporter,
            via_bot_id: bot,
            target_kind: "person",
            target_id: target,
            target_person_id: Some(target),
            hangout_id: None,
            reason: "spam",
            details: None,
            snapshot: "{}",
        }
    }

    // RE3: one open report per reporter and target; a closed one doesn't stop a new one.
    #[test]
    fn the_one_open_report_index_allows_a_new_report_once_the_old_one_is_closed() {
        let db = Db::open(":memory:").unwrap();
        let first = match db.file_report(&report("p1", "p2", None), 1_000, 20, 5).unwrap() {
            Filed::New(id) => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            db.file_report(&report("p1", "p2", None), 1_001, 20, 5).unwrap(),
            Filed::Updated(first)
        );
        // The index itself refuses a second open row.
        let twice = db.conn().execute(
            "INSERT INTO reports (reporter_id, by_bot, target_kind, target_id, reason,
                                  created_at, updated_at, day)
             VALUES ('p1', 0, 'person', 'p2', 'spam', 1, 1, 0)",
            [],
        );
        assert!(twice.is_err(), "a second open report on one target");
        db.set_report_status(first, "dismissed", 1_002).unwrap();
        let second = db.file_report(&report("p1", "p2", None), 1_003, 20, 5).unwrap();
        assert!(matches!(second, Filed::New(id) if id != first), "{second:?}");
        let row = db.report_row(first).unwrap().unwrap();
        assert_eq!(row["status"], "dismissed");
        assert_eq!(row["closed_at"], "1002");
    }

    // RE3: an update keeps who filed it, never lowers minor_safety, and a bot's update leaves
    // what its person filed alone.
    #[test]
    fn an_update_keeps_who_filed_it_and_never_lowers_minor_safety() {
        let db = Db::open(":memory:").unwrap();
        let mut by_person = report("p1", "p2", None);
        by_person.reason = "minor_safety";
        by_person.details = Some("a child was asked for photos");
        let id = match db.file_report(&by_person, 1_000, 20, 5).unwrap() {
            Filed::New(id) => id,
            other => panic!("{other:?}"),
        };
        // Their bot reports the same person as spam: nothing the person filed changes.
        let mut by_bot = report("p1", "p2", Some("b1"));
        by_bot.details = Some("spam");
        assert_eq!(db.file_report(&by_bot, 1_001, 20, 5).unwrap(), Filed::Updated(id));
        let row = db.report_row(id).unwrap().unwrap();
        assert_eq!(row["reason"], "minor_safety");
        assert_eq!(row["details"], "a child was asked for photos");
        assert_eq!(row["by_bot"], "0");
        assert!(!row.contains_key("via_bot_id"), "{row:?}");
        // The person themselves can add details, but spam doesn't lower minor_safety.
        let mut again = report("p1", "p2", None);
        again.details = Some("and again today");
        assert_eq!(db.file_report(&again, 1_002, 20, 5).unwrap(), Filed::Updated(id));
        let row = db.report_row(id).unwrap().unwrap();
        assert_eq!(row["reason"], "minor_safety");
        assert_eq!(row["details"], "and again today");

        // A bot's report stays the bot's when its person files on it again.
        let bots = match db.file_report(&report("p1", "p3", Some("b1")), 1_003, 20, 5).unwrap() {
            Filed::New(id) => id,
            other => panic!("{other:?}"),
        };
        let mut over = report("p1", "p3", None);
        over.reason = "hate";
        assert_eq!(db.file_report(&over, 1_004, 20, 5).unwrap(), Filed::Updated(bots));
        let row = db.report_row(bots).unwrap().unwrap();
        assert_eq!(row["reason"], "hate");
        assert_eq!(row["by_bot"], "1");
        assert_eq!(row["via_bot_id"], "b1");
    }

    // RE4: the caps, counted per UTC day.
    #[test]
    fn the_caps_count_a_persons_day_and_their_bots_share_of_it() {
        let db = Db::open(":memory:").unwrap();
        for n in 0..5 {
            let target = format!("t{n}");
            let filed = db.file_report(&report("p1", &target, Some("b1")), 10, 20, 5).unwrap();
            assert!(matches!(filed, Filed::New(_)));
        }
        assert_eq!(
            db.file_report(&report("p1", "t5", Some("b1")), 10, 20, 5).unwrap(),
            Filed::BotsFull
        );
        for n in 5..20 {
            let target = format!("t{n}");
            assert!(matches!(
                db.file_report(&report("p1", &target, None), 10, 20, 5).unwrap(),
                Filed::New(_)
            ));
        }
        assert_eq!(
            db.file_report(&report("p1", "t20", None), 10, 20, 5).unwrap(),
            Filed::PersonFull
        );
        // A new UTC day starts again.
        assert!(matches!(
            db.file_report(&report("p1", "t20", None), 86_400 + 10, 20, 5).unwrap(),
            Filed::New(_)
        ));
    }
}
