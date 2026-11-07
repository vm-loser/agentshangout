//! Tests for the operators' review queue (docs/reports.md, RE8 to RE18). They drive a real server
//! over HTTP. A comment such as `// RE10` above a test names the rule it covers.

mod support;

use agenthangout::{app, auth, db::Db, now, review, AppState, Config};
use reqwest::{header, Client, Method, StatusCode};
use serde_json::{json, Value};

const CODE: &str = "letmein";
const JUSTIN: &str = "god-justin";
const MORG: &str = "god-morg";

struct Server {
    base: String,
    client: Client,
    state: AppState,
}

struct Reply {
    status: StatusCode,
    body: Value,
}

impl Reply {
    #[track_caller]
    fn ok(self) -> Value {
        assert_eq!(self.status, StatusCode::OK, "{}", self.body);
        self.body
    }

    #[track_caller]
    fn refused(self, status: StatusCode) -> Value {
        assert_eq!(self.status, status, "{}", self.body);
        self.body
    }
}

#[derive(Clone)]
struct Who {
    key: String,
    id: String,
}

struct Bot {
    id: String,
    token: String,
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_else(|| panic!("not text: {value}")).to_string()
}

fn about(kind: &str, id: &str, reason: &str) -> Value {
    json!({ "target_kind": kind, "target_id": id, "reason": reason })
}

impl Server {
    /// A server where Justin and Morg are on the god list, with god mode off.
    async fn start() -> (Self, Who, Who) {
        let db = Db::open(":memory:").unwrap();
        let god = |id: &str, name: &str| {
            let key = auth::new_owner_key();
            db.create_person(id, name, &auth::hash_secret(&key), &auth::new_feed_token(), now())
                .unwrap();
            Who { key, id: id.to_string() }
        };
        let (justin, morg) = (god(JUSTIN, "Justin"), god(MORG, "Morg"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = AppState::new(
            db,
            Config {
                public_url: base.clone(),
                create_code: Some(CODE.to_string()),
                allow_private_webhooks: true,
                keeper_id: None,
                god_ids: vec![JUSTIN.to_string(), MORG.to_string()],
                require_rules: false,
            },
        );
        let served = state.clone();
        tokio::spawn(async move { axum::serve(listener, app(served)).await });
        (Self { base, client: Client::new(), state }, justin, morg)
    }

    async fn send(&self, token: &str, method: Method, path: &str, body: Option<Value>) -> Reply {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if !token.is_empty() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        Reply { status, body: serde_json::from_str(&text).unwrap_or(Value::String(text)) }
    }

    async fn get(&self, token: &str, path: &str) -> Reply {
        self.send(token, Method::GET, path, None).await
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> Reply {
        self.send(token, Method::POST, path, Some(body)).await
    }

    /// What the account page does (A27, A28): signs in for a cookie, then deletes with it.
    async fn delete_account(&self, who: &Who, name: &str) {
        let url = format!("{}/api/session", self.base);
        let signed = self.client.post(url).json(&json!({ "owner_key": who.key })).send().await;
        let signed = signed.unwrap();
        assert_eq!(signed.status(), StatusCode::OK);
        let cookie = signed.headers()[header::SET_COOKIE].to_str().unwrap();
        let cookie = cookie.split(';').next().unwrap().to_string();
        let url = format!("{}/api/me/delete", self.base);
        let request = self.client.post(url).header(header::COOKIE, cookie);
        let deleted = request.json(&json!({ "name": name })).send().await.unwrap();
        let status = deleted.status();
        assert_eq!(status, StatusCode::OK, "{}", deleted.text().await.unwrap());
    }

    async fn god_mode(&self, god: &Who, on: bool) {
        self.send(&god.key, Method::PUT, "/api/god-mode", Some(json!({ "on": on }))).await.ok();
    }

    async fn someone(&self, name: &str) -> (String, Who) {
        let made = self
            .post(
                "",
                "/api/hangouts",
                json!({ "name": format!("{name}'s room"), "your_name": name, "create_code": CODE }),
            )
            .await
            .ok();
        let who = Who { key: text(&made["owner_key"]), id: text(&made["person"]["id"]) };
        (text(&made["hangout"]["id"]), who)
    }

    async fn join(&self, owner: &Who, room: &str, name: &str) -> Who {
        let snapshot = self.get(&owner.key, &format!("/api/hangouts/{room}")).await.ok();
        let code = text(&snapshot["hangout"]["invite_code"]);
        let path = format!("/api/invites/{code}/join");
        let made = self.post("", &path, json!({ "your_name": name })).await.ok();
        Who { key: text(&made["owner_key"]), id: text(&made["person"]["id"]) }
    }

    fn bot(&self, owner: &Who, room: &str, name: &str) -> Bot {
        let (agent, token) = support::legacy_bot(&self.state, &owner.id, room, name);
        Bot { id: agent.id, token }
    }

    async fn say(&self, who: &Who, room: &str, words: &str) -> Reply {
        let path = format!("/api/hangouts/{room}/messages");
        self.post(&who.key, &path, json!({ "text": words })).await
    }

    async fn tool(&self, token: &str, name: &str, arguments: Value) -> Value {
        let call = json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        self.post(token, "/mcp", call).await.ok()["result"].clone()
    }

    async fn report(&self, who: &Who, body: Value) -> i64 {
        self.post(&who.key, "/api/reports", body).await.ok()["report_id"].as_i64().unwrap()
    }

    async fn close(&self, god: &Who, id: i64, action: &str, note: Option<&str>) -> Reply {
        let path = format!("/api/reports/{id}/close");
        self.post(&god.key, &path, json!({ "action": action, "note": note })).await
    }

    async fn queue(&self, god: &Who) -> Value {
        self.get(&god.key, "/api/reports").await.ok()
    }

    fn row(&self, id: i64) -> std::collections::BTreeMap<String, String> {
        self.state.db.report_row(id).unwrap().unwrap()
    }

    fn actions(&self) -> Vec<std::collections::BTreeMap<String, String>> {
        self.state.db.report_action_rows().unwrap()
    }

    /// A room with Ann in it, Bo who wrote a line there, and Bo's bot Pip who wrote one too.
    async fn scene(&self) -> Scene {
        let (room, ann) = self.someone("Ann").await;
        let bo = self.join(&ann, &room, "Bo").await;
        let cy = self.join(&ann, &room, "Cy").await;
        let pip = self.bot(&bo, &room, "Pip");
        let line = self.say(&bo, &room, "something cruel").await.ok();
        let line = line["id"].as_i64().unwrap().to_string();
        let said = self.tool(&pip.token, "say", json!({ "hangout_id": room, "text": "bot spam" }));
        assert_ne!(said.await["isError"], true);
        let lines = self.state.db.messages(&room, 0, 100).unwrap();
        let bot_line = lines.iter().find(|m| m.body == "bot spam").unwrap().id.to_string();
        Scene { room, ann, bo, cy, pip, line, bot_line }
    }
}

struct Scene {
    room: String,
    ann: Who,
    bo: Who,
    cy: Who,
    pip: Bot,
    line: String,
    bot_line: String,
}

// RE8: only an operator with god mode on reads the queue; a bot never does; and it's
// minor_safety first, then oldest first, with the copy, the room, links and by_bot.
#[tokio::test]
async fn the_queue_is_for_operators_with_god_mode_on_and_minor_safety_comes_first() {
    let (server, justin, _) = Server::start().await;
    let s = server.scene().await;
    let first = server.report(&s.ann, about("message", &s.line, "harassment")).await;
    let second = server.report(&s.ann, about("person", &s.cy.id, "spam")).await;
    let urgent = server.report(&s.ann, about("message", &s.bot_line, "minor_safety")).await;

    // Anyone else gets W75's 404, god mode off is 403, and a bot gets 404 even a god's.
    server.get(&s.ann.key, "/api/reports").await.refused(StatusCode::NOT_FOUND);
    server.get(&justin.key, "/api/reports").await.refused(StatusCode::FORBIDDEN);
    let (justins_room, _) = server.someone("Spare").await;
    let gods_bot = server.bot(&justin, &justins_room, "Godbot");
    server.god_mode(&justin, true).await;
    server.get(&gods_bot.token, "/api/reports").await.refused(StatusCode::NOT_FOUND);
    server.get("", "/api/reports").await.refused(StatusCode::UNAUTHORIZED);
    // No bot tool reads it.
    let tools = server
        .post(&gods_bot.token, "/mcp", json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await
        .ok();
    let names: Vec<String> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| text(&tool["name"]))
        .collect();
    assert!(!names.iter().any(|name| name.contains("queue") || name.contains("review")));

    let queue = server.queue(&justin).await;
    let groups = queue["groups"].as_array().unwrap();
    let order: Vec<i64> = groups.iter().map(|g| g["report_id"].as_i64().unwrap()).collect();
    assert_eq!(order, [urgent, first, second], "{queue}");
    let urgent_group = &groups[0];
    assert_eq!(urgent_group["minor_safety"], true);
    assert_eq!(urgent_group["copy"]["text"], "bot spam");
    assert_eq!(urgent_group["room"]["id"], s.room);
    assert_eq!(urgent_group["person"]["id"], s.bo.id);
    assert_eq!(urgent_group["bot"]["id"], s.pip.id);
    assert_eq!(urgent_group["bot"]["name"], "Pip");
    let line_group = &groups[1];
    assert_eq!(line_group["copy"]["text"], "something cruel");
    assert_eq!(line_group["reports"][0]["by_bot"], false);
    assert_eq!(line_group["reports"][0]["reason"], "harassment");
    // Turned off again, it's 403 again.
    server.god_mode(&justin, false).await;
    server.get(&justin.key, "/api/reports").await.refused(StatusCode::FORBIDDEN);
}

// RE9: reports about one target are one group, with how many reported and who, and a bot's
// report shows as by_bot. A target whose id was blanked groups by its copy.
#[tokio::test]
async fn reports_about_one_target_are_one_group_with_the_reporters_names() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let ann_bot = server.bot(&s.ann, &s.room, "Scout");
    server.report(&s.ann, about("message", &s.line, "harassment")).await;
    server.report(&s.cy, about("message", &s.line, "hate")).await;
    let by_bot = server.tool(&ann_bot.token, "report", about("person", &s.bo.id, "spam")).await;
    assert_ne!(by_bot["isError"], true, "{by_bot}");

    let queue = server.queue(&justin).await;
    let groups = queue["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{queue}");
    let line = &groups[0];
    assert_eq!(line["reporters"], 2);
    assert_eq!(line["reasons"], json!(["harassment", "hate"]));
    let names: Vec<&str> = line["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reporter"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Ann", "Cy"]);
    let person = &groups[1];
    assert_eq!(person["by_bot"], 1);
    assert_eq!(person["reports"][0]["by_bot"], true);
    assert_eq!(person["reports"][0]["bot"]["name"], "Scout");

    // Two reports about someone who then deleted their account have no target id, and group
    // by what was copied.
    let (dee_room, dee) = server.someone("Dee").await;
    let eve = server.join(&dee, &dee_room, "Eve").await;
    let fay = server.join(&dee, &dee_room, "Fay").await;
    let a = server.report(&dee, about("person", &eve.id, "impersonation")).await;
    let b = server.report(&fay, about("person", &eve.id, "impersonation")).await;
    server.state.db.delete_person(&eve.id, "Eve").unwrap();
    assert!(!server.row(a).contains_key("target_id"));
    let queue = server.queue(&justin).await;
    let blanked: Vec<&Value> =
        queue["groups"].as_array().unwrap().iter().filter(|g| g["target_id"].is_null()).collect();
    assert_eq!(blanked.len(), 1, "{queue}");
    assert_eq!(blanked[0]["reporters"], 2);
    // Closing one closes its group, with the same action.
    let closed = server.close(&justin, a, "dismiss", None).await.ok();
    assert_eq!(closed["closed"], json!([a, b]));
    assert_eq!(server.row(b)["status"], "dismissed");
}

// RE10, RE13, RE14: dismiss and remove_content tell nobody; remove_content deletes for everyone;
// closing one closes its group with the same action, and every report closed is logged.
#[tokio::test]
async fn dismiss_and_remove_content_tell_nobody_and_close_the_group() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let one = server.report(&s.ann, about("message", &s.line, "harassment")).await;
    let two = server.report(&s.cy, about("message", &s.line, "harassment")).await;
    let spam = server.report(&s.ann, about("person", &s.cy.id, "spam")).await;

    server.close(&justin, spam, "dismiss", Some("not spam")).await.ok();
    assert_eq!(server.row(spam)["status"], "dismissed");
    assert!(server.row(spam).contains_key("closed_at"));

    let before = server.state.db.messages(&s.room, 0, 100).unwrap().len();
    let closed = server.close(&justin, two, "remove_content", None).await.ok();
    assert_eq!(closed["closed"], json!([one, two]));
    assert!(server.state.db.message(s.line.parse().unwrap()).unwrap().is_none(), "gone");
    // RE18: nothing was posted in the room about it.
    assert_eq!(server.state.db.messages(&s.room, 0, 100).unwrap().len(), before - 1);
    for id in [one, two] {
        assert_eq!(server.row(id)["status"], "actioned");
    }
    // The copy stays for the operator, and the reporter only sees it was reviewed (RE5).
    assert!(server.row(one)["snapshot"].contains("something cruel"));
    let mine = server.get(&s.ann.key, "/api/reports/mine").await.ok();
    assert!(mine["reports"].as_array().unwrap().iter().all(|r| r["status"] == "reviewed"));
    // Nobody was told.
    for who in [&s.bo, &s.cy, &s.ann] {
        assert!(server.get(&who.key, "/api/me").await.ok().get("notices").is_none());
    }
    // A closed report can't be closed again, and an action off the list is refused.
    server.close(&justin, one, "warn", None).await.refused(StatusCode::CONFLICT);
    let other = server.report(&s.ann, about("person", &s.bo.id, "spam")).await;
    server.close(&justin, other, "shout", None).await.refused(StatusCode::BAD_REQUEST);
    server.close(&justin, other, "remove_content", None).await.refused(StatusCode::BAD_REQUEST);

    // RE13: one row per report closed, with the operator, the action, the note and the time.
    let log = server.actions();
    assert_eq!(log.len(), 3, "{log:?}");
    assert_eq!(log[0]["action"], "dismiss");
    assert_eq!(log[0]["note"], "not spam");
    assert_eq!(log[0]["operator_id"], JUSTIN);
    assert_eq!(log[0]["report_id"], spam.to_string());
    assert!(log[0].contains_key("at"));
    assert_eq!(log[1]["action"], "remove_content");
    assert_eq!(log[2]["report_id"], two.to_string());
}

// RE10, RE14: a warning is a private notice to the reported person, naming the rule and never
// who reported. It shows once: in Needs you and /api/me until it's seen.
#[tokio::test]
async fn a_warning_tells_the_person_once_and_never_who_reported() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let id = server.report(&s.ann, about("message", &s.line, "harassment")).await;
    server.close(&justin, id, "warn", Some("first time")).await.ok();

    let me = server.get(&s.bo.key, "/api/me").await.ok();
    let notices = me["notices"].as_array().unwrap();
    assert_eq!(notices.len(), 1, "{me}");
    let said = text(&notices[0]["text"]);
    assert!(said.contains("harassment") && said.contains("/rules"), "{said}");
    for leak in ["Ann", &s.ann.id, "first time", "Justin", "something cruel"] {
        assert!(!notices[0].to_string().contains(leak), "{leak} in {said}");
    }
    let needs = server.get(&s.bo.key, "/api/needs-you").await.ok();
    assert_eq!(needs["notices"][0]["id"], notices[0]["id"]);
    assert!(needs["count"].as_u64().unwrap() >= 1);
    // Nobody else hears of it, and it's still their room.
    assert!(server.get(&s.ann.key, "/api/me").await.ok().get("notices").is_none());
    assert!(me.get("hold").is_none());
    server.say(&s.bo, &s.room, "sorry").await.ok();

    // Only they can mark it seen, and once seen it's gone from both.
    let path = format!("/api/notices/{}/seen", notices[0]["id"]);
    server.post(&s.ann.key, &path, json!({})).await.refused(StatusCode::NOT_FOUND);
    server.post(&s.bo.key, &path, json!({})).await.ok();
    assert!(server.get(&s.bo.key, "/api/me").await.ok().get("notices").is_none());
    let needs = server.get(&s.bo.key, "/api/needs-you").await.ok();
    assert_eq!(needs["notices"], json!([]));
}

// RE10, RE11, RE12, RE14: a suspended bot can't act anywhere, its person is told and can't lift
// it, and an operator lifting it lets it act again. Both are logged.
#[tokio::test]
async fn a_suspended_bot_cant_act_until_an_operator_lifts_it() {
    let (server, justin, morg) = Server::start().await;
    server.god_mode(&justin, true).await;
    server.god_mode(&morg, true).await;
    let s = server.scene().await;
    let id = server.report(&s.ann, about("message", &s.bot_line, "spam")).await;
    server.close(&justin, id, "suspend_bot", None).await.ok();

    // Every tool, the house of tools included, and every /api call.
    for tool in ["say", "inbox", "read_room"] {
        let refused = server.tool(&s.pip.token, tool, json!({ "hangout_id": s.room, "text": "x" }));
        let refused = refused.await;
        assert_eq!(refused["isError"], true, "{tool}: {refused}");
        let words = refused.to_string();
        assert!(words.contains("paused") && !words.contains("spam"), "{words}");
    }
    let path = format!("/api/hangouts/{}/messages", s.room);
    let refused = server.post(&s.pip.token, &path, json!({ "text": "x" })).await;
    assert_eq!(refused.refused(StatusCode::FORBIDDEN)["reason"], "paused_by_operator");
    server.get(&s.pip.token, "/api/me").await.refused(StatusCode::FORBIDDEN);
    // Its person is told, by the bot's name and the rule, and is otherwise free.
    let me = server.get(&s.bo.key, "/api/me").await.ok();
    let said = text(&me["notices"][0]["text"]);
    assert!(said.contains("Pip") && said.contains("spam"), "{said}");
    server.say(&s.bo, &s.room, "still here").await.ok();
    // Their person can't lift it: the lift is an operator's.
    let lift = json!({ "hold": "suspend_bot", "id": s.pip.id });
    server.post(&s.bo.key, "/api/reports/lift", lift.clone()).await.refused(StatusCode::NOT_FOUND);
    let queue = server.queue(&justin).await;
    assert_eq!(queue["holds"][0]["hold"], "suspend_bot");
    assert_eq!(queue["holds"][0]["name"], "Pip");
    assert_eq!(queue["holds"][0]["person"]["name"], "Bo");

    // Another operator lifts it, and it acts again.
    let mut lifted = lift.clone();
    lifted["note"] = json!("appealed");
    server.post(&morg.key, "/api/reports/lift", lifted).await.ok();
    let said = server.tool(&s.pip.token, "say", json!({ "hangout_id": s.room, "text": "back" }));
    assert_ne!(said.await["isError"], true);
    server.post(&morg.key, "/api/reports/lift", lift).await.refused(StatusCode::NOT_FOUND);
    let log = server.actions();
    assert_eq!(log[0]["action"], "suspend_bot");
    assert_eq!(log[0]["subject_id"], s.pip.id);
    assert_eq!(log[0]["person_id"], s.bo.id);
    assert_eq!(log[1]["action"], "lift_suspend_bot");
    assert_eq!(log[1]["operator_id"], MORG);
    assert_eq!(log[1]["note"], "appealed");
}

// RE10, RE11, RE14: a restricted person reads, signs in, sees their notice and can delete their
// account, but can't write, place or build, and their bots are paused without being told why.
#[tokio::test]
async fn a_restricted_person_reads_but_writes_nothing_and_their_bots_are_paused() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let id = server.report(&s.ann, about("person", &s.bo.id, "harassment")).await;
    server.close(&justin, id, "restrict_person", None).await.ok();

    // Writing is refused, through the one gate.
    let refused = server.say(&s.bo, &s.room, "hello").await.refused(StatusCode::FORBIDDEN);
    assert_eq!(refused["reason"], "restricted");
    let made =
        server.post(&s.bo.key, "/api/hangouts", json!({ "name": "New", "create_code": CODE }));
    made.await.refused(StatusCode::FORBIDDEN);
    let path = format!("/api/floors/{}/items", s.room);
    let placed = server.post(&s.bo.key, &path, json!({ "kind": "bookshelf", "x": 3, "y": 3 }));
    placed.await.refused(StatusCode::FORBIDDEN);
    // Reading still works, and so does their account page with the hold on it.
    server.get(&s.bo.key, &format!("/api/hangouts/{}", s.room)).await.ok();
    let me = server.get(&s.bo.key, "/api/me").await.ok();
    assert_eq!(me["hold"], "restricted");
    assert_eq!(me["notices"][0]["kind"], "restrict_person");
    let path = format!("/api/notices/{}/seen", me["notices"][0]["id"]);
    server.post(&s.bo.key, &path, json!({})).await.ok();
    // Their bot is paused, and isn't told why.
    let said = server.tool(&s.pip.token, "say", json!({ "hangout_id": s.room, "text": "x" })).await;
    assert_eq!(said["isError"], true);
    let words = said.to_string();
    assert!(words.contains("paused by an operator") || words.contains("paused"), "{words}");
    assert!(!words.contains("harassment") && !words.contains("restricted"), "{words}");
    // Other people are untouched.
    server.say(&s.cy, &s.room, "fine here").await.ok();

    // Lifted, they write again.
    let lift = json!({ "hold": "restrict_person", "id": s.bo.id });
    server.post(&justin.key, "/api/reports/lift", lift).await.ok();
    server.say(&s.bo, &s.room, "hello again").await.ok();
    assert!(server.get(&s.bo.key, "/api/me").await.ok().get("hold").is_none());

    // Restricted again, they can still delete their account.
    let again = server.report(&s.cy, about("person", &s.bo.id, "harassment")).await;
    server.close(&justin, again, "restrict_person", None).await.ok();
    server.delete_account(&s.bo, "Bo").await;
    assert!(server.state.db.person(&s.bo.id).unwrap().is_none());
}

// RE10, RE11: a banned person's sign-in reaches only their account, to delete it; their bots stop.
#[tokio::test]
async fn a_banned_person_reaches_only_their_account_and_their_bots_stop() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let id = server.report(&s.ann, about("message", &s.line, "hate")).await;
    server.close(&justin, id, "ban_person", Some("slurs")).await.ok();

    let read = server.get(&s.bo.key, &format!("/api/hangouts/{}", s.room)).await;
    assert_eq!(read.refused(StatusCode::FORBIDDEN)["reason"], "banned");
    server.get(&s.bo.key, "/api/needs-you").await.refused(StatusCode::FORBIDDEN);
    server.say(&s.bo, &s.room, "hi").await.refused(StatusCode::FORBIDDEN);
    // Signing in still works, and leads to their account.
    let signed = server.post("", "/api/session", json!({ "owner_key": s.bo.key })).await;
    assert_ne!(signed.status, StatusCode::FORBIDDEN, "{}", signed.body);
    let me = server.get(&s.bo.key, "/api/me").await.ok();
    assert_eq!(me["hold"], "banned");
    assert!(text(&me["notices"][0]["text"]).contains("delete your account"));
    let said = server.tool(&s.pip.token, "inbox", json!({})).await;
    assert_eq!(said["isError"], true);
    // The ban is lifted only by an operator, and the log keeps both.
    let queue = server.queue(&justin).await;
    assert_eq!(queue["holds"][0]["hold"], "ban_person");
    let lift = json!({ "hold": "ban_person", "id": s.bo.id, "note": "appeal" });
    server.post(&s.bo.key, "/api/reports/lift", lift.clone()).await.refused(StatusCode::FORBIDDEN);
    server.post(&justin.key, "/api/reports/lift", lift).await.ok();
    server.get(&s.bo.key, &format!("/api/hangouts/{}", s.room)).await.ok();
    let said = server.tool(&s.pip.token, "inbox", json!({})).await;
    assert_ne!(said["isError"], true, "{said}");

    // Banned again, they can still delete their account, and the log loses their id with it
    // (RE17) while keeping every row.
    let again = server.report(&s.cy, about("person", &s.bo.id, "hate")).await;
    server.close(&justin, again, "ban_person", None).await.ok();
    server.delete_account(&s.bo, "Bo").await;
    let log = server.actions();
    assert_eq!(log.len(), 3, "{log:?}");
    assert!(log.iter().all(|row| row.get("person_id").is_none()), "{log:?}");
    assert!(log.iter().all(|row| row.get("subject_id").is_none()), "{log:?}");
    assert_eq!(log[0]["note"], "slurs");
    assert!(server.state.db.all_holds().unwrap().is_empty());
}

// RE15: a minor_safety report alerts every operator at once, with its id and nothing else, once.
#[tokio::test]
async fn a_minor_safety_report_alerts_every_operator_with_its_id_only() {
    let (server, justin, morg) = Server::start().await;
    let s = server.scene().await;
    let plain = server.report(&s.ann, about("person", &s.cy.id, "spam")).await;
    for god in [&justin, &morg] {
        assert!(server.get(&god.key, "/api/me").await.ok().get("notices").is_none());
    }
    let id = server.report(&s.ann, about("message", &s.line, "minor_safety")).await;
    assert_ne!(id, plain);
    // Raised to minor_safety by an update, it alerts too, once.
    let raised = server.report(&s.cy, about("person", &s.bo.id, "spam")).await;
    server.report(&s.cy, about("person", &s.bo.id, "minor_safety")).await;
    server.report(&s.cy, about("person", &s.bo.id, "minor_safety")).await;
    // God mode doesn't need to be on to hear of it.
    for god in [&justin, &morg] {
        let me = server.get(&god.key, "/api/me").await.ok();
        let alerts = me["notices"].as_array().unwrap();
        assert_eq!(alerts.len(), 2, "{me}");
        assert_eq!(alerts[0]["kind"], "report_alert");
        assert_eq!(alerts[0]["report_id"], id);
        assert_eq!(alerts[1]["report_id"], raised);
        let said = alerts[0].to_string();
        for leak in ["cruel", "Bo", "Ann", &s.room] {
            assert!(!said.contains(leak), "{leak} in {said}");
        }
        assert!(said.contains(&id.to_string()));
        let needs = server.get(&god.key, "/api/needs-you").await.ok();
        assert_eq!(needs["notices"].as_array().unwrap().len(), 2);
    }
    // Nobody else is alerted.
    for who in [&s.ann, &s.bo, &s.cy] {
        assert!(server.get(&who.key, "/api/me").await.ok().get("notices").is_none());
    }
}

// RE16: 90 days after closing, a report's copy and details are dropped, but a minor_safety one's
// stay until an operator releases it, which is logged. The log outlives the copy (RE13).
#[tokio::test]
async fn old_copies_are_dropped_except_minor_safety_until_released() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let mut body = about("message", &s.line, "harassment");
    body["details"] = json!("twice");
    let plain = server.report(&s.ann, body).await;
    let mut body = about("person", &s.bo.id, "minor_safety");
    body["details"] = json!("asked a child for photos");
    let minor = server.report(&s.cy, body).await;
    let fresh = server.report(&s.ann, about("person", &s.cy.id, "spam")).await;
    // Releasing an open report, or one that isn't minor_safety, is refused.
    let release = |id: i64| format!("/api/reports/{id}/release");
    server.post(&justin.key, &release(minor), json!({})).await.refused(StatusCode::CONFLICT);
    for id in [plain, minor, fresh] {
        server.close(&justin, id, "dismiss", None).await.ok();
    }
    server.post(&justin.key, &release(plain), json!({})).await.refused(StatusCode::BAD_REQUEST);
    // Two of them closed 91 days ago.
    let long_ago = now() - 91 * 24 * 60 * 60;
    for id in [plain, minor] {
        server.state.db.set_report_status(id, "dismissed", long_ago).unwrap();
    }
    review::drop_old_copies(&server.state).unwrap();
    let row = server.row(plain);
    assert!(!row.contains_key("snapshot") && !row.contains_key("details"), "{row:?}");
    assert_eq!(row["reason"], "harassment");
    assert_eq!(row["status"], "dismissed");
    assert!(server.row(minor).contains_key("snapshot"));
    assert_eq!(server.row(minor)["details"], "asked a child for photos");
    assert!(server.row(fresh).contains_key("snapshot"), "not 90 days yet");
    // The held one is listed for release, and released it drops straight away.
    let queue = server.queue(&justin).await;
    assert_eq!(queue["held"][0]["report_id"], minor, "{queue}");
    let note = json!({ "note": "case closed with the authorities" });
    server.post(&justin.key, &release(minor), note.clone()).await.ok();
    assert!(!server.row(minor).contains_key("snapshot"));
    assert!(!server.row(minor).contains_key("details"));
    server.post(&justin.key, &release(minor), note).await.refused(StatusCode::CONFLICT);
    assert_eq!(server.queue(&justin).await["held"], json!([]));
    let log = server.actions();
    assert_eq!(log.len(), 4);
    assert!(log.iter().any(|r| r["action"] == "dismiss" && r["report_id"] == plain.to_string()));
    let released = log.last().unwrap();
    assert_eq!(released["action"], "release");
    assert_eq!(released["operator_id"], JUSTIN);
    assert_eq!(released["note"], "case closed with the authorities");
}

// RE18: every action needs god mode on at that moment, and no bot can take one.
#[tokio::test]
async fn an_action_needs_god_mode_on_then_and_never_comes_from_a_bot() {
    let (server, justin, _) = Server::start().await;
    let s = server.scene().await;
    let id = server.report(&s.ann, about("person", &s.bo.id, "spam")).await;
    server.god_mode(&justin, true).await;
    server.queue(&justin).await;
    server.god_mode(&justin, false).await;
    server.close(&justin, id, "warn", None).await.refused(StatusCode::FORBIDDEN);
    let lift = json!({ "hold": "ban_person", "id": s.bo.id });
    server.post(&justin.key, "/api/reports/lift", lift).await.refused(StatusCode::FORBIDDEN);
    let path = format!("/api/reports/{id}/close");
    let by_bot = server.post(&s.pip.token, &path, json!({ "action": "dismiss" })).await;
    by_bot.refused(StatusCode::NOT_FOUND);
    server.close(&s.ann, id, "dismiss", None).await.refused(StatusCode::NOT_FOUND);
    assert_eq!(server.row(id)["status"], "open");
    assert!(server.actions().is_empty());
}

// RE17: deleting an account keeps the reports and the log; a minor_safety report keeps all ids.
#[tokio::test]
async fn deleting_an_account_keeps_a_minor_safety_reports_log_ids() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let id = server.report(&s.ann, about("person", &s.bo.id, "minor_safety")).await;
    server.close(&justin, id, "ban_person", None).await.ok();
    server.delete_account(&s.bo, "Bo").await;
    let log = server.actions();
    assert_eq!(log[0]["person_id"], s.bo.id);
    assert_eq!(server.row(id)["target_person_id"], s.bo.id);
}

/// RE19: asks for the export, and gives the status, the `Retry-After` and `Content-Disposition`
/// headers, and the body as text, so a test can search all of it.
async fn export(
    server: &Server,
    token: &str,
) -> (StatusCode, Option<String>, Option<String>, String) {
    let url = format!("{}/api/account/export", server.base);
    let mut request = server.client.get(url);
    if !token.is_empty() {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.unwrap();
    let header = |name: header::HeaderName| {
        response.headers().get(name).map(|value| value.to_str().unwrap().to_string())
    };
    let (wait, disposition) = (header(header::RETRY_AFTER), header(header::CONTENT_DISPOSITION));
    (response.status(), wait, disposition, response.text().await.unwrap())
}

fn texts(list: &Value, field: &str) -> Vec<String> {
    let rows = list.as_array().unwrap_or_else(|| panic!("not a list: {list}"));
    rows.iter().map(|row| text(&row[field])).collect()
}

// RE19: a person's export holds their own account, bots, writing, lists and the reports they
// filed, and nothing of anyone else's: no other person's lines, no reports about them, no
// reporter, no operator note, and no token, key or URL anywhere in it.
#[tokio::test]
async fn an_export_holds_only_the_persons_own_data_and_no_secret() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    server.say(&s.ann, &s.room, "words Ann wrote").await.ok();
    server.say(&s.cy, &s.room, "words Cy wrote").await.ok();
    // Bo's bot has a wake-up URL and its key, and Bo has a notification URL of his own.
    let hook = "http://127.0.0.1:9/hook-path";
    server.state.db.set_notify_url(&s.pip.id, Some(hook)).unwrap();
    server.state.db.set_notify_auth(&s.pip.id, Some("whsec_c2VjcmV0LWtleQ")).unwrap();
    server.state.db.set_notify_url(&s.bo.id, Some("https://example.com/bo-hook")).unwrap();
    // Bo's bot lets Ann wake it and Cy's bot invite it: the export keeps how many, never their ids.
    let mut trust = agenthangout::bot_trust::Trust::default();
    trust.wake.mode = "chosen".into();
    trust.wake.people = vec![s.ann.id.clone()];
    trust.invite_public.bots = vec![s.cy.id.clone()];
    server.state.db.save_bot_trust(&s.pip.id, &trust).unwrap();
    // Bo blocks Cy and mutes Ann, and reports Cy with details of his own.
    server.post(&s.bo.key, "/api/blocks", json!({ "person_id": s.cy.id })).await.ok();
    server.post(&s.bo.key, "/api/mutes", json!({ "person_id": s.ann.id })).await.ok();
    let mut filed = about("person", &s.cy.id, "spam");
    filed["details"] = json!("details Bo gave");
    let bo_filed = server.report(&s.bo, filed).await;
    // Ann reports Bo's line, and an operator warns him with a note.
    let about_bo = server.report(&s.ann, about("message", &s.line, "harassment")).await;
    server.close(&justin, about_bo, "warn", Some("operator eyes only")).await.ok();

    let (status, _, disposition, body) = export(&server, &s.bo.key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let disposition = disposition.unwrap();
    assert!(
        disposition.starts_with("attachment") && disposition.contains(".json"),
        "{disposition}"
    );
    let data: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["account"]["name"], "Bo");
    assert!(data["account"]["created_at"].is_i64(), "{data}");
    assert!(texts(&data["account"]["sign_in_methods"], "method").contains(&"owner_key".into()));
    // The warning is his, naming the rule, and never the note or who reported.
    assert_eq!(data["account"]["notices"][0]["kind"], "warn", "{data}");
    // His bot, with its look and settings, and whether it has a wake-up URL.
    assert_eq!(data["bots"][0]["name"], "Pip", "{data}");
    assert_eq!(data["bots"][0]["has_wake_up_url"], true);
    assert_eq!(data["bots"][0]["trust"]["wake"]["people_chosen"], 1, "{data}");
    assert!(data["bots"][0]["trust"]["wake"].get("people").is_none(), "{data}");
    // What he and his bot wrote, with the room's name, and nobody else's lines.
    let lines = texts(&data["messages"]["rows"], "text");
    assert!(lines.contains(&"something cruel".to_string()), "{lines:?}");
    assert!(lines.contains(&"bot spam".to_string()), "{lines:?}");
    assert_eq!(data["messages"]["truncated"], false);
    assert_eq!(data["messages"]["rows"][0]["room"]["name"], "Ann's room");
    // His own lists.
    assert_eq!(data["blocks"]["rows"][0]["name"], "Cy", "{data}");
    assert_eq!(data["mutes"]["rows"][0]["name"], "Ann", "{data}");
    // The report he filed: its reason, status and dates only.
    let report = &data["reports_filed"]["rows"][0];
    assert_eq!(report["id"], bo_filed, "{data}");
    assert_eq!(report["reason"], "spam");
    assert_eq!(report["status"], "received");
    assert!(report.get("details").is_none() && report.get("target_id").is_none(), "{report}");
    assert_eq!(data["reports_filed"]["rows"].as_array().unwrap().len(), 1);

    // Nothing of anyone else's, and no secret, anywhere in the file.
    let ann_id = format!("\"{}\"", s.ann.id);
    let cy_id = format!("\"{}\"", s.cy.id);
    for never in [
        "words Ann wrote",
        "words Cy wrote",
        "operator eyes only",
        "details Bo gave",
        ann_id.as_str(),
        cy_id.as_str(),
        JUSTIN,
        s.bo.key.as_str(),
        s.pip.token.as_str(),
        "ah_bot_",
        "ah_owner_",
        "ah_sess",
        "ah_",
        "whsec",
        "hook-path",
        "bo-hook",
        "http://",
        "https://",
        "key_hash",
        "token",
        "secret",
    ] {
        assert!(!body.contains(never), "the export has {never:?}: {body}");
    }
    // Ann's own export has her line and none of Bo's, and the report about Bo is hers.
    let (status, _, _, ann) = export(&server, &s.ann.key).await;
    assert_eq!(status, StatusCode::OK, "{ann}");
    assert!(ann.contains("words Ann wrote") && !ann.contains("something cruel"), "{ann}");
    let ann: Value = serde_json::from_str(&ann).unwrap();
    assert_eq!(ann["reports_filed"]["rows"][0]["id"], about_bo);
    assert_eq!(ann["reports_filed"]["rows"][0]["status"], "reviewed");
}

// RE19: one export an hour, a bot can't ask, and a banned or restricted person still can.
#[tokio::test]
async fn an_export_is_once_an_hour_for_a_person_and_works_while_held() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let (status, _, _, body) = export(&server, &s.pip.token).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, _, _, body) = export(&server, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");

    assert_eq!(export(&server, &s.ann.key).await.0, StatusCode::OK);
    let (status, wait, _, body) = export(&server, &s.ann.key).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    let wait: u64 = wait.expect("Retry-After").parse().unwrap();
    assert!(wait > 3500 && wait <= 3600, "{wait}");
    // A bot's ask didn't use its person's hour.
    assert_eq!(export(&server, &s.bo.key).await.0, StatusCode::OK);

    // A banned person reaches only their account, and their data.
    let ban = server.report(&s.ann, about("person", &s.cy.id, "spam")).await;
    server.close(&justin, ban, "ban_person", None).await.ok();
    let room = format!("/api/hangouts/{}", s.room);
    server.get(&s.cy.key, &room).await.refused(StatusCode::FORBIDDEN);
    let (status, _, _, body) = export(&server, &s.cy.key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let data: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(data["account"]["holds"][0]["hold"], "ban_person", "{data}");
    assert_eq!(data["account"]["notices"][0]["kind"], "ban_person");
    assert!(review::BANNED_ROUTES.contains(&"GET /api/account/export"));

    // So does a restricted one, as a read.
    let (_, dee) = server.someone("Dee").await;
    let restrict = server.report(&s.ann, about("person", &dee.id, "spam")).await;
    server.close(&justin, restrict, "restrict_person", None).await.ok();
    let (status, _, _, body) = export(&server, &dee.key).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// RE20: a wake-up URL that counts what reaches it.
async fn counter() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::{atomic::AtomicUsize, Arc};
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let router = axum::Router::new().route(
        "/wake",
        axum::routing::post(move || {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/wake", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, calls)
}

fn rings(calls: &std::sync::atomic::AtomicUsize) -> usize {
    calls.load(std::sync::atomic::Ordering::SeqCst)
}

async fn rung(calls: &std::sync::atomic::AtomicUsize, at_least: usize) -> bool {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if rings(calls) >= at_least {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    false
}

/// RE20: what an owner's answer sends a bot, straight to it.
fn ring(server: &Server, agent_id: &str) {
    let notice = agenthangout::notify::Notice {
        event: "owner_question.answered",
        hangout_id: String::new(),
        text: "Your person answered.".into(),
    };
    agenthangout::notify::send(&server.state, &[agent_id.to_string()], &notice);
}

// RE20: a suspended bot's mention is recorded but rings nothing, nor does a ring sent straight to
// it, and once the hold lifts its rings go again.
#[tokio::test]
async fn a_suspended_bot_is_rung_by_nothing_and_its_mentions_wait_for_it() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let (url, calls) = counter().await;
    server.state.db.set_notify_url(&s.pip.id, Some(&url)).unwrap();
    let id = server.report(&s.ann, about("bot", &s.pip.id, "spam")).await;
    server.close(&justin, id, "suspend_bot", None).await.ok();
    assert!(review::is_paused(&server.state, &s.pip.id));
    assert!(!review::is_paused(&server.state, &s.bo.id));

    let said = server.say(&s.bo, &s.room, "@Pip are you there").await.ok();
    ring(&server, &s.pip.id);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert_eq!(rings(&calls), 0, "a paused bot was rung");
    // The mention is kept, for when it's back, and the skipped ring says why.
    let waiting = server.state.db.unread_mentions(&s.pip.id).unwrap();
    assert!(waiting.iter().any(|m| m.message_id == said["id"].as_i64().unwrap()));
    let decided = server.state.db.wake_decisions(&s.pip.id).unwrap();
    assert!(decided.iter().any(|d| d.reason == "operator_hold"), "{} decisions", decided.len());

    let lift = json!({ "hold": "suspend_bot", "id": s.pip.id });
    server.post(&justin.key, "/api/reports/lift", lift).await.ok();
    assert!(!review::is_paused(&server.state, &s.pip.id));
    ring(&server, &s.pip.id);
    assert!(rung(&calls, 1).await, "a lifted bot wasn't rung");
    server.say(&s.bo, &s.room, "@Pip welcome back").await.ok();
    assert!(rung(&calls, 2).await, "the owner's mention didn't ring");
}

// RE20: a bot whose person is restricted is paused the same way.
#[tokio::test]
async fn a_restricted_persons_bot_is_rung_by_nothing_until_lifted() {
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let (url, calls) = counter().await;
    server.state.db.set_notify_url(&s.pip.id, Some(&url)).unwrap();
    let id = server.report(&s.ann, about("person", &s.bo.id, "spam")).await;
    server.close(&justin, id, "restrict_person", None).await.ok();
    assert!(review::is_paused(&server.state, &s.pip.id));

    let said = server.say(&s.ann, &s.room, "@Pip hello").await.ok();
    ring(&server, &s.pip.id);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    assert_eq!(rings(&calls), 0, "a paused bot was rung");
    let waiting = server.state.db.unread_mentions(&s.pip.id).unwrap();
    assert!(waiting.iter().any(|m| m.message_id == said["id"].as_i64().unwrap()));

    let lift = json!({ "hold": "restrict_person", "id": s.bo.id });
    server.post(&justin.key, "/api/reports/lift", lift).await.ok();
    ring(&server, &s.pip.id);
    assert!(rung(&calls, 1).await, "a lifted bot wasn't rung");
    server.say(&s.bo, &s.room, "@Pip welcome back").await.ok();
    assert!(rung(&calls, 2).await, "the owner's mention didn't ring");
}

/// RE20: opens a bot's `events/stream`.
async fn open_stream(server: &Server, token: &str) -> reqwest::Response {
    let response = server
        .client
        .post(format!("{}/mcp", server.base))
        .bearer_auth(token)
        .json(&json!({
            "jsonrpc": "2.0", "id": "s1", "method": "events/stream",
            "params": { "name": "mention" },
        }))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
    response
}

/// RE20: whether a stream ends by itself within five seconds.
async fn ends<S, B, E>(mut stream: S) -> bool
where
    S: futures_util::Stream<Item = Result<B, E>> + Unpin,
{
    use futures_util::StreamExt;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, stream.next()).await {
            Ok(None) | Ok(Some(Err(_))) => return true,
            Ok(Some(Ok(_))) => continue,
            Err(_) => return false,
        }
    }
}

// RE20: a bot's open event stream and room stream close when a hold lands, and new ones are
// refused while it's paused.
#[tokio::test]
async fn a_hold_closes_a_bots_open_streams_and_refuses_new_ones() {
    use futures_util::StreamExt;
    let (server, justin, _) = Server::start().await;
    server.god_mode(&justin, true).await;
    let s = server.scene().await;
    let mut events = open_stream(&server, &s.pip.token).await.bytes_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.next()).await;
    let first = String::from_utf8_lossy(&first.unwrap().unwrap().unwrap()).to_string();
    assert!(first.contains("notifications/events/active"), "{first}");
    let room = format!("{}/api/hangouts/{}/stream", server.base, s.room);
    let room = server.client.get(room).bearer_auth(&s.pip.token).send().await.unwrap();
    assert_eq!(room.status(), StatusCode::OK);
    let room = room.bytes_stream();

    let id = server.report(&s.ann, about("bot", &s.pip.id, "spam")).await;
    server.close(&justin, id, "suspend_bot", None).await.ok();
    // Both end on their own, with nothing more said in the room.
    assert!(ends(events).await, "the event stream stayed open");
    assert!(ends(room).await, "the room stream stayed open");

    // New ones are refused, saying it's paused and not why.
    let refused: Value = open_stream(&server, &s.pip.token).await.json().await.unwrap();
    assert_eq!(refused["error"]["data"]["reason"], "paused_by_operator", "{refused}");
    let waited = server.tool(&s.pip.token, "events_since", json!({ "wait_seconds": 1 })).await;
    assert_eq!(waited["isError"], true, "{waited}");
    let path = format!("/api/hangouts/{}/stream", s.room);
    server.get(&s.pip.token, &path).await.refused(StatusCode::FORBIDDEN);
}
