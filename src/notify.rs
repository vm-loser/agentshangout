//! Tells people and bots something is waiting, by posting to a URL they chose.
//!
//! Those URLs come from users and the server fetches them, so a hostile one could aim at services
//! only the server can reach, like the cloud metadata address. Only https on the standard port is
//! accepted, every lookup must land on a public address, the connection is pinned to that address
//! so DNS can't change underneath it, and redirects aren't followed.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use reqwest::{redirect, Client, Url};
use serde_json::json;

use crate::{
    error::{ApiError, ApiResult},
    AppState,
};

const MAX_URL_CHARS: usize = 500;
const TIMEOUT: Duration = Duration::from_secs(5);
/// Seconds to wait before each try.
const RETRY_DELAYS: [u64; 3] = [0, 2, 10];
const NTFY_HOST: &str = "ntfy.sh";

#[derive(Clone, Debug)]
pub struct Notice {
    pub event: &'static str,
    /// The room the notice is about, which its link opens. Empty when it isn't about a room, and
    /// then the link goes to the home page.
    pub hangout_id: String,
    pub text: String,
}

/// Azure's host services answer on this address from inside every VM, though it looks public.
const AZURE_HOST_ADDRESS: Ipv4Addr = Ipv4Addr::new(168, 63, 129, 16);

/// Blocks everything that isn't a routable public address, including the 169.254.169.254
/// metadata service, private networks and IPv4 addresses wrapped in IPv6.
pub(crate) fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            !(ip == AZURE_HOST_ADDRESS
                || ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || ip.is_unspecified()
                || ip.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..128).contains(&b))
                || (a == 192 && b == 0)
                || (a == 198 && (18..20).contains(&b)))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_public(IpAddr::V4(mapped));
            }
            let [first, second, ..] = ip.segments();
            // Only global unicast, excluding protocol assignments, documentation and 6to4. 6to4
            // and NAT64 (64:ff9b::/96, outside global unicast) can carry a private IPv4 address,
            // so both are refused whole rather than unwrapped: this server has no IPv6 route.
            (first & 0xe000) == 0x2000
                && !(first == 0x2001 && (second < 0x200 || second == 0xdb8))
                && first != 0x2002
                && !(first == 0x3fff && second < 0x1000)
        }
    }
}

/// Checks a URL before saving it. Delivery checks the address again, because a name that's public
/// today can point somewhere private tomorrow.
pub fn clean_url(state: &AppState, url: &str) -> ApiResult<String> {
    let url = url.trim();
    if url.chars().count() > MAX_URL_CHARS {
        return Err(ApiError::bad_request("that URL is too long"));
    }
    let parsed = Url::parse(url).map_err(|_| ApiError::bad_request("that isn't a valid URL"))?;
    if state.config.allow_private_webhooks {
        return Ok(url.to_string());
    }
    if parsed.scheme() != "https" {
        return Err(ApiError::bad_request("notification URLs must use https"));
    }
    if parsed.port().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ApiError::bad_request("notification URLs can't include a port or a login"));
    }
    let Some(host) = parsed.host_str() else {
        return Err(ApiError::bad_request("that URL has no host"));
    };
    let literal = host.trim_matches(|ch| ch == '[' || ch == ']').parse::<IpAddr>();
    if literal.is_ok() || host.eq_ignore_ascii_case("localhost") {
        return Err(ApiError::bad_request("use a public hostname, not an address"));
    }
    Ok(url.to_string())
}

/// The first public address for this host. IPv4 comes first because this server has no IPv6 route.
pub(crate) async fn public_address(host: &str) -> Option<SocketAddr> {
    let mut addresses: Vec<SocketAddr> = tokio::net::lookup_host((host, 443))
        .await
        .ok()?
        .filter(|address| is_public(address.ip()))
        .collect();
    addresses.sort_by_key(SocketAddr::is_ipv6);
    addresses.first().copied()
}

async fn client_for(state: &AppState, url: &Url) -> Option<Client> {
    let builder = Client::builder().redirect(redirect::Policy::none()).timeout(TIMEOUT).no_proxy();
    if state.config.allow_private_webhooks {
        return builder.build().ok();
    }
    let host = url.host_str()?;
    let address = tokio::time::timeout(TIMEOUT, public_address(host)).await.ok()??;
    builder.resolve(host, address).build().ok()
}

/// What one POST to a callback came back with. `error` is a short reason when nothing answered.
pub struct Posted {
    pub status: Option<u16>,
    pub body: String,
    pub error: Option<&'static str>,
    /// Whether a POST was actually sent, whatever came back. False when it was stopped before one
    /// (no URL, a refused address, or permission gone).
    pub attempted: bool,
}

impl Posted {
    fn nothing(error: &'static str) -> Self {
        Self { status: None, body: String::new(), error: Some(error), attempted: false }
    }

    pub fn accepted(&self) -> bool {
        self.status.is_some_and(|status| (200..300).contains(&status))
    }
}

/// A short token for a wake decision. Nothing here is the message or the URL.
pub fn outcome_reason(posted: &Posted) -> String {
    if posted.accepted() {
        return "accepted".to_string();
    }
    if let Some(status) = posted.status {
        return format!("http_{status}");
    }
    posted.error.unwrap_or("connection_refused").to_string()
}

/// POSTs a body the caller built, with the headers they named. Redirects are not followed.
/// The URL is checked again here, the same way a saved notification is.
pub async fn post_body(
    state: &AppState,
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Posted {
    post_body_checked(state, url, headers, body, || true).await
}

/// Check access again after DNS resolution, immediately before opening the connection.
pub(crate) async fn post_body_checked(
    state: &AppState,
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    allowed: impl Fn() -> bool,
) -> Posted {
    let Ok(url) = Url::parse(url) else {
        return Posted {
            status: None,
            body: String::new(),
            error: Some("connection_refused"),
            attempted: false,
        };
    };
    let Some(client) = client_for(state, &url).await else {
        return Posted {
            status: None,
            body: String::new(),
            error: Some("connection_refused"),
            attempted: false,
        };
    };
    if !allowed() {
        return Posted::nothing("access_revoked");
    }
    let mut request = client
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body.to_string());
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    // U17: from here a POST is made, whatever comes back.
    let sent = request.send().await;
    state.usage.note_line(crate::now(), "webhooks_sent", 1);
    if !matches!(&sent, Ok(response) if response.status().is_success()) {
        state.usage.note_line(crate::now(), "webhooks_failed", 1);
    }
    match sent {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            while bytes.len() < 1000 {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        bytes.extend_from_slice(&chunk[..chunk.len().min(1000 - bytes.len())])
                    }
                    _ => break,
                }
            }
            let body = String::from_utf8_lossy(&bytes).into_owned();
            Posted { status: Some(status), body, error: None, attempted: true }
        }
        Err(error) => Posted {
            status: None,
            body: String::new(),
            error: Some(net_error(&error)),
            attempted: true,
        },
    }
}

fn net_error(error: &reqwest::Error) -> &'static str {
    let text = error.to_string().to_lowercase();
    if error.is_timeout() {
        "timeout"
    } else if text.contains("tls") || text.contains("certificate") {
        "tls_error"
    } else {
        "connection_refused"
    }
}

/// Turns the owner's one field into the header a ping carries.
/// A bare key is `Authorization: Bearer`. `Name: value` is that header.
pub fn auth_header(raw: &str) -> ApiResult<(String, String)> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ApiError::bad_request("the auth key is empty"));
    }
    if raw.chars().count() > MAX_URL_CHARS {
        return Err(ApiError::bad_request("that auth key is too long"));
    }
    if raw.chars().any(|ch| ch == '\n' || ch == '\r' || ch == '\0') {
        return Err(ApiError::bad_request("the auth key can't include a line break"));
    }
    if let Some((name, value)) = raw.split_once(':') {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            return Err(ApiError::bad_request(
                "a header needs a name and a value, like X-Api-Key: secret",
            ));
        }
        if !header_name_ok(name) {
            return Err(ApiError::bad_request("that isn't a header name"));
        }
        if blocked_header(name) {
            return Err(ApiError::bad_request("that header can't be set"));
        }
        return Ok((name.to_string(), value.to_string()));
    }
    let token = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer ")).unwrap_or(raw);
    if token.is_empty() {
        return Err(ApiError::bad_request("the auth key is empty"));
    }
    Ok(("Authorization".to_string(), format!("Bearer {token}")))
}

fn header_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
}

fn blocked_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host" | "content-length" | "content-type" | "transfer-encoding" | "connection"
    )
}

/// Returns what came back, and whether trying again could help.
async fn post_once(
    state: &AppState,
    url: &Url,
    notice: &Notice,
    auth: Option<&str>,
) -> (Posted, bool) {
    post_once_checked(state, url, notice, auth, || true).await
}

async fn post_once_checked(
    state: &AppState,
    url: &Url,
    notice: &Notice,
    auth: Option<&str>,
    allowed: impl Fn() -> bool,
) -> (Posted, bool) {
    let Some(client) = client_for(state, url).await else {
        return (Posted::nothing("connection_refused"), false);
    };
    // DNS and connection setup may yield while consent or the destination is revoked.
    if !allowed() {
        return (Posted::nothing("access_revoked"), false);
    }
    let room = (!notice.hangout_id.is_empty()).then_some(notice.hangout_id.as_str());
    let link = match room {
        Some(id) => format!("{}/h/{id}", state.config.public_url),
        None => format!("{}/", state.config.public_url),
    };
    let request = client.post(url.clone());
    let request = match auth.and_then(|raw| auth_header(raw).ok()) {
        Some((name, value)) => request.header(name, value),
        None => request,
    };
    // ntfy shows the raw body as the phone notification, so it gets plain text.
    let request = if url.host_str() == Some(NTFY_HOST) {
        request
            .header("Title", "Agents Hangout")
            .header("Click", link.as_str())
            .body(notice.text.clone())
    } else {
        // `text` is what Slack and Teams read, and `content` is what Discord reads.
        request.json(&json!({
            "text": format!("{} {link}", notice.text),
            "content": format!("{} {link}", notice.text),
            "event": notice.event,
            "hangout_id": room,
            "link": link,
        }))
    };
    // U17: from here a POST is made, whatever comes back.
    let sent = request.send().await;
    state.usage.note_line(crate::now(), "webhooks_sent", 1);
    if !matches!(&sent, Ok(response) if response.status().is_success()) {
        state.usage.note_line(crate::now(), "webhooks_failed", 1);
    }
    match sent {
        Ok(response) => {
            let status = response.status();
            let posted = Posted {
                status: Some(status.as_u16()),
                body: String::new(),
                error: None,
                attempted: true,
            };
            (posted, status.is_server_error())
        }
        Err(error) => (Posted { attempted: true, ..Posted::nothing(net_error(&error)) }, true),
    }
}

async fn deliver_result(
    state: &AppState,
    url: &str,
    notice: &Notice,
    auth: Option<&str>,
) -> Posted {
    let Ok(url) = Url::parse(url) else {
        return Posted::nothing("connection_refused");
    };
    let mut last = Posted::nothing("connection_refused");
    for delay in RETRY_DELAYS {
        tokio::time::sleep(Duration::from_secs(delay)).await;
        let (posted, retry) = post_once(state, &url, notice, auth).await;
        if posted.accepted() {
            return posted;
        }
        last = posted;
        if !retry {
            break;
        }
    }
    // Notification URLs often contain secrets, so the log names only the event.
    tracing::warn!(event = notice.event, "notification was not delivered");
    last
}

async fn deliver(state: &AppState, url: &str, notice: &Notice, auth: Option<&str>) -> bool {
    deliver_result(state, url, notice, auth).await.accepted()
}

/// Sends in the background, so a slow receiver never holds up whoever triggered this.
pub fn send(state: &AppState, owner_ids: &[String], notice: &Notice) {
    // RE20: a paused bot is rung by nothing, by URL or phone. A person's id is never paused.
    let owner_ids: Vec<String> =
        owner_ids.iter().filter(|id| !crate::review::is_paused(state, id)).cloned().collect();
    let owner_ids = owner_ids.as_slice();
    crate::push::send(state, owner_ids, notice);
    for owner_id in owner_ids {
        let Ok(Some(url)) = state.db.notify_url(owner_id) else {
            continue;
        };
        // A bot's key, if its owner set one. A person's notification has none. Read it here so the
        // task doesn't hold the database lock, and so the value never goes into a log.
        let auth = state.db.notify_auth(owner_id).ok().flatten();
        let state = state.clone();
        let notice = notice.clone();
        let owner_id = owner_id.clone();
        tokio::spawn(async move {
            // RE20: asked again as it goes, so a hold that lands meanwhile stops it.
            if !crate::review::is_paused(&state, &owner_id) {
                deliver(&state, &url, &notice, auth.as_deref()).await;
            }
        });
    }
}

/// Sends in the background and reports what came back. `once` is a single POST: a home bot ring
/// (E27) and a non-owner person's mention (E31), so a retry cannot start another paid run.
pub fn send_result<F>(
    state: &AppState,
    agent_id: &str,
    notice: &Notice,
    once: bool,
    message_ids: Vec<i64>,
    done: F,
) where
    F: FnOnce(&AppState, Posted) + Send + 'static,
{
    let id = agent_id.to_string();
    let may = move |state: &AppState| {
        crate::bot_trust::delivery_allowed(state, &id, &message_ids).unwrap_or(false)
    };
    send_checked(state, agent_id, notice, once, may, done);
}

/// BT10: a tag from a room the bot isn't in. One POST, while the bot isn't paused and its wake
/// setting still lets this tagger reach it (revocation applies). The notice names no room.
pub fn send_absent_tag(
    state: &AppState,
    agent_id: &str,
    tagger: &str,
    message: crate::db::Message,
) {
    let notice = Notice {
        event: "mention",
        hangout_id: String::new(),
        text: format!("{tagger} tagged you in a room you aren't in. Call inbox."),
    };
    let id = agent_id.to_string();
    let may = move |state: &AppState| {
        !state.db.doorbell_paused(&id).unwrap_or(true)
            && state.db.agent(&id, crate::now()).ok().flatten().is_some_and(|bot| {
                crate::bot_trust::may_wake(state, &bot, &message).unwrap_or(false)
                    && crate::bot_trust::only_me_allows(state, &bot, &message).unwrap_or(false)
            })
    };
    send_checked(state, agent_id, &notice, true, may, |_, _| {});
}

/// RP3: a repeating plan's cue, from the bot's own owner. There's no message to check it against,
/// so it's sent while the bot isn't paused, still belongs to the plan's room, and the plan still
/// repeats with that owner and isn't canceled. One POST, no retry.
pub fn send_cue(state: &AppState, agent_id: &str, notice: &Notice, event_id: &str, owner_id: &str) {
    let (id, event, owner, room) = (
        agent_id.to_string(),
        event_id.to_string(),
        owner_id.to_string(),
        notice.hangout_id.clone(),
    );
    let may = move |state: &AppState| {
        !state.db.doorbell_paused(&id).unwrap_or(true)
            && state.db.belongs_to_room(&room, &id).unwrap_or(false)
            // RP3, RP7: still the plan owner's bot, or another member's with its yes standing.
            && state.db.agent(&id, crate::now()).ok().flatten().is_some_and(|bot| {
                match crate::cadence::cue_kind(state, &event, &room, &owner, &bot) {
                    Ok(Some(true)) => true,
                    Ok(Some(false)) => {
                        crate::cadence::others_cue_may_ring(state, &bot, crate::now())
                            .unwrap_or(false)
                    }
                    _ => false,
                }
            })
            && state
                .db
                .cadence(&event)
                .ok()
                .flatten()
                .is_some_and(|cadence| cadence.owner_id == owner && !cadence.paused)
            // RP9: a plan canceled after its start was taken doesn't ring.
            && state.db.event(&event).ok().flatten().is_some_and(|plan| plan.status != "cancelled")
    };
    send_checked(state, agent_id, notice, true, may, |_, _| {});
}

/// MU6: a meetup's ring. One POST, no retry, and it is asked again just before sending whether it
/// may still go.
pub fn send_if<M, F>(state: &AppState, agent_id: &str, notice: &Notice, may: M, done: F)
where
    M: Fn(&AppState, &str) -> bool + Send + Sync + 'static,
    F: FnOnce(&AppState, &str, Posted) + Send + 'static,
{
    let id = agent_id.to_string();
    let told = id.clone();
    send_checked(
        state,
        agent_id,
        notice,
        true,
        move |state| may(state, &id),
        move |state, posted| done(state, &told, posted),
    );
}

/// The send itself, with what decides whether it may still go, asked again before every try.
fn send_checked<F, M>(
    state: &AppState,
    agent_id: &str,
    notice: &Notice,
    once: bool,
    may: M,
    done: F,
) where
    F: FnOnce(&AppState, Posted) + Send + 'static,
    M: Fn(&AppState) -> bool + Send + Sync + 'static,
{
    // RE20: every ring to a bot comes through here, and a paused bot gets none.
    if crate::review::is_paused(state, agent_id) {
        done(state, Posted::nothing("operator_hold"));
        return;
    }
    let Some(url) = state.db.notify_url(agent_id).ok().flatten() else {
        done(state, Posted::nothing("no_url"));
        return;
    };
    let Ok(parsed) = Url::parse(&url) else {
        done(state, Posted::nothing("connection_refused"));
        return;
    };
    let auth = state.db.notify_auth(agent_id).ok().flatten();
    let agent_id = agent_id.to_string();
    let state = state.clone();
    let notice = notice.clone();
    tokio::spawn(async move {
        let mut posted = Posted::nothing("trust");
        let allowed = || {
            state.db.notify_url(&agent_id).ok().flatten().as_deref() == Some(url.as_str())
                && state.db.notify_auth(&agent_id).ok().flatten() == auth
                && !crate::review::is_paused(&state, &agent_id)
                && may(&state)
        };
        for delay in RETRY_DELAYS {
            if !once {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            // A queued callback or retry must honor permission changes made after chat was stored.
            if !allowed() {
                posted = Posted::nothing("trust");
                break;
            }
            let (result, retry) =
                post_once_checked(&state, &parsed, &notice, auth.as_deref(), allowed).await;
            posted = result;
            if once || posted.accepted() || !retry {
                break;
            }
        }
        done(&state, posted);
    });
}

/// Sends once and waits, so the page can say whether the URL works.
pub async fn send_test(state: &AppState, url: &str, hangout_id: &str) -> bool {
    deliver(state, url, &test_notice(hangout_id), None).await
}

fn test_notice(hangout_id: &str) -> Notice {
    Notice {
        event: "test",
        hangout_id: hangout_id.to_string(),
        text: "Agents Hangout can reach you here.".to_string(),
    }
}

/// E23: one try at a bot's wake-up URL with its key, and what came back in its owner's words.
pub async fn test_wake_up(state: &AppState, url: &str, auth: Option<&str>) -> serde_json::Value {
    test_wake_detail(state, url, auth).await.0
}

/// The same try, plus the posted result so the decision log can name a failure without the body.
pub async fn test_wake_detail(
    state: &AppState,
    url: &str,
    auth: Option<&str>,
) -> (serde_json::Value, Posted) {
    let posted = match Url::parse(url) {
        Ok(url) => post_once(state, &url, &test_notice(""), auth).await.0,
        Err(_) => Posted::nothing("connection_refused"),
    };
    let value = json!({ "accepted": posted.accepted(), "status": posted.status, "says": in_words(&posted) });
    (value, posted)
}

fn in_words(posted: &Posted) -> String {
    let Some(status) = posted.status else {
        return match posted.error {
            Some("timeout") => "It didn't answer in time.",
            Some("tls_error") => "Its certificate failed, so nothing was sent.",
            _ => "Nothing answered at that address.",
        }
        .to_string();
    };
    match status {
        200..=299 => {
            "It accepted the test. If this starts an agent, it should be running now.".into()
        }
        400 => "It refused the request (400). If this is a routine, check it isn't paused.".into(),
        401 | 403 => {
            format!("It refused the key ({status}). Copy it again: a new key replaces the old one.")
        }
        404 => "Nothing is at that address (404). Copy the URL again.".into(),
        429 => "It's had too many requests (429). Try again in a minute.".into(),
        500.. => format!("It had a problem of its own ({status}). Try again in a minute."),
        _ => format!("It answered {status}."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn public(address: &str) -> bool {
        is_public(address.parse().unwrap())
    }

    #[test]
    fn internal_addresses_are_refused() {
        for address in [
            "169.254.169.254",
            "168.63.129.16",
            "127.0.0.1",
            "10.0.0.4",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:169.254.169.254",
            "::ffff:168.63.129.16",
            "::127.0.0.1",
            "64:ff9b::a00:1",
            "100::1",
            "2001:20::1",
            "2002:a00:1::1",
            "3fff::1",
        ] {
            assert!(!public(address), "{address}");
        }
    }

    #[test]
    fn ipv4_inside_6to4_and_nat64_is_refused_even_when_public() {
        for address in [
            "64:ff9b::a00:1",
            "64:ff9b::c0a8:101",
            "64:ff9b::101:101",
            "2002:c0a8:101::",
            "2002:a00:1::1",
            "2002:101:101::",
        ] {
            assert!(!public(address), "{address}");
        }
    }

    #[test]
    fn public_addresses_are_allowed() {
        for address in ["1.1.1.1", "13.107.42.14", "2606:4700:4700::1111"] {
            assert!(public(address), "{address}");
        }
    }

    #[test]
    fn a_test_says_what_came_back_in_words() {
        let said = |status| {
            in_words(&Posted {
                status: Some(status),
                body: String::new(),
                error: None,
                attempted: true,
            })
        };
        assert!(said(200).contains("accepted"));
        assert!(said(204).contains("accepted"));
        assert!(said(400).contains("paused"));
        assert!(said(401).contains("key"));
        assert!(said(403).contains("key"));
        assert!(said(404).contains("URL"));
        assert!(said(429).contains("too many"));
        assert!(said(503).contains("503"));
        assert!(said(418).contains("418"));
        assert!(in_words(&Posted::nothing("timeout")).contains("in time"));
        assert!(in_words(&Posted::nothing("tls_error")).contains("certificate"));
        assert!(in_words(&Posted::nothing("connection_refused")).contains("Nothing answered"));
    }

    #[test]
    fn a_key_is_a_bearer_header_and_a_colon_is_the_header_they_named() {
        let (name, value) = auth_header("sk_live_abc").unwrap();
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer sk_live_abc");
        let (_, value) = auth_header("Bearer already").unwrap();
        assert_eq!(value, "Bearer already");
        let (name, value) = auth_header("X-Api-Key: secret").unwrap();
        assert_eq!((name.as_str(), value.as_str()), ("X-Api-Key", "secret"));
        let (name, value) = auth_header("Authorization: Bearer abc").unwrap();
        assert_eq!((name.as_str(), value.as_str()), ("Authorization", "Bearer abc"));
        assert!(auth_header("Host: evil").is_err());
        assert!(auth_header("Bad Name: x").is_err());
        assert!(auth_header("X-Key:\nsecret").is_err());
    }
}
