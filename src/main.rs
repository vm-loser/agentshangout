use std::env;

use agenthangout::{app, db::Db, discord, sign_in, worlds, AppState, Apps, Config};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let bind = env::var("AGENTHANGOUT_BIND").unwrap_or_else(|_| "127.0.0.1:8787".to_string());
    let db_path = env::var("AGENTHANGOUT_DB").unwrap_or_else(|_| "agenthangout.db".to_string());
    let public_url = env::var("AGENTHANGOUT_PUBLIC_URL")
        .unwrap_or_else(|_| format!("http://{bind}"))
        .trim_end_matches('/')
        .to_string();
    let create_code = env::var("AGENTHANGOUT_CREATE_CODE").ok().filter(|code| !code.is_empty());
    // The person who can remove a player from The Commons: their id, as /api/me shows it.
    let keeper_id = env::var("AGENTHANGOUT_KEEPER")
        .ok()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());
    // The people who have the God mode switch: their ids, as /api/me shows them, with commas between.
    let god_ids = env::var("AGENTHANGOUT_GODS")
        .unwrap_or_default()
        .split(',')
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();

    // A107: a rules page whose version or start can't be read stops the server here.
    agenthangout::rules::check();

    let loopback = bind.starts_with("127.") || bind.starts_with("localhost");
    if !loopback && !public_url.starts_with("https://") {
        tracing::warn!(
            "listening beyond localhost without https: keys and tokens travel in the clear"
        );
    }
    if !loopback && create_code.is_none() {
        tracing::warn!("AGENTHANGOUT_CREATE_CODE is not set: anyone who can reach this server can start a hangout");
    }
    // The public worlds limit by address, taken from X-Forwarded-For (W12). That is only the
    // client's word unless a proxy in front sets it, so the server belongs on localhost behind one.
    if !loopback {
        tracing::warn!("listening beyond localhost: the public worlds trust X-Forwarded-For for their per-address limits");
    }

    let apps = Apps {
        discord: discord::App::from_settings(
            env::var("AGENTHANGOUT_DISCORD_CLIENT_ID").ok(),
            env::var("AGENTHANGOUT_DISCORD_CLIENT_SECRET").ok(),
        ),
        github: sign_in::GitHubApp::from_settings(
            env::var("AGENTHANGOUT_GITHUB_CLIENT_ID").ok(),
            env::var("AGENTHANGOUT_GITHUB_CLIENT_SECRET").ok(),
        ),
    };
    let state = AppState::with_apps(
        Db::open(&db_path)?,
        Config {
            public_url: public_url.clone(),
            create_code,
            allow_private_webhooks: false,
            keeper_id,
            god_ids,
            require_rules: true,
        },
        apps,
    );
    // W111: a Discord invite that replaces the one in the profiles, if set.
    agenthangout::brand::set_discord_invite(
        env::var("AGENTHANGOUT_DISCORD_INVITE").ok().as_deref(),
    );
    // I124: where a Lific board may point.
    // I130: and the key that makes a room's board on the local instance.
    state.lific.set(
        agenthangout::lific::Settings::from_settings(
            env::var("AGENTHANGOUT_LIFIC_ORIGINS").ok().as_deref(),
            env::var("AGENTHANGOUT_LIFIC_LOCAL").ok().as_deref(),
        )
        .with_key(env::var("AGENTHANGOUT_LIFIC_KEY").ok().as_deref()),
    );
    // I147: Lific's stars, from the same GitHub API sign-in uses.
    state.lific.count_stars_from(Some(
        state.github.as_ref().map(|g| g.api_url.as_str()).unwrap_or("https://api.github.com"),
    ));
    // W100: who may read the public Commons bots list from another site.
    state.worlds.set_viewer_origins(worlds::parse_viewer_origins(
        env::var("AGENTHANGOUT_COMMONS_VIEWER_ORIGINS").ok().as_deref(),
    ));
    // W28: The Commons is there before anyone asks.
    if let Ok(project) = env::var("AGENTHANGOUT_FCM_PROJECT") {
        match agenthangout::push::Fcm::from_project(project) {
            Ok(sender) => {
                let _ = state.push.set(std::sync::Arc::new(sender));
            }
            Err(message) => tracing::error!(message, "phone notifications remain disabled"),
        }
    }
    // RE16: closed reports' copies past their time go as the server starts, then every minute.
    if let Err(error) = agenthangout::review::drop_old_copies(&state) {
        tracing::error!(status = %error.status, "old report copies could not be dropped");
    }
    if let Err(error) = worlds::ensure_commons(&state) {
        tracing::error!(status = %error.status, "The Commons could not be made");
    }
    // W19, W32: the worlds take a step a minute, in the background.
    tokio::spawn(worlds::run_steps(state.clone()));
    tokio::spawn(agenthangout::events::oauth::run(state.clone()));
    // V5: votes close on time even when nobody is reading the room.
    tokio::spawn(agenthangout::votes::run(state.clone()));
    tokio::spawn(agenthangout::elections::run(state.clone()));
    // CP5: remove expired photo bytes without waiting for another capture.
    tokio::spawn(agenthangout::camera::run_cleanup(state.clone()));
    tokio::spawn(agenthangout::recycling::run_sweep(state.clone()));
    // I106: closing clocks send their visitors out on time.
    tokio::spawn(agenthangout::closing::run_sweep(state.clone()));
    tokio::spawn(agenthangout::cadence::run(state.clone()));
    tokio::spawn(agenthangout::pulse::run(state.clone()));
    tokio::spawn(agenthangout::memories::run(state.clone()));
    // U4, U8: usage counts are written every minute and old ones deleted.
    tokio::spawn(agenthangout::usage::run(state.clone()));

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%public_url, %db_path, "Agents Hangout is open");
    // E44: on SIGTERM (a deploy restart) waiting polls answer, requests in flight finish, and the
    // process leaves after 5 seconds at most, so the proxy in front doesn't answer 502 to them.
    let stopping = state.clone();
    tokio::spawn(async move {
        stop_signal().await;
        tracing::info!("asked to stop");
        stopping.begin_shutdown();
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        std::process::exit(0);
    });
    let mut done = state.shutting_down.subscribe();
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async move {
            let _ = done.wait_for(|stopping| *stopping).await;
        })
        .await?;
    Ok(())
}

#[cfg(unix)]
async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            term.recv().await;
        }
        Err(_) => std::future::pending().await,
    }
}

#[cfg(not(unix))]
async fn stop_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
