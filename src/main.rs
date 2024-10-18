use std::sync::Arc;

use axum::{routing::get, Router};
use handlers::queue::queue;
use match_making::MatchmakingState;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

mod match_making;

mod handlers;

#[derive(Clone)]
struct AppState {
    matchmaking_state: Arc<RwLock<MatchmakingState>>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().unwrap();
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(EnvFilter::from_default_env())
        .with_thread_ids(true)
        .with_thread_names(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let (matchmaking_state, command_rx) = MatchmakingState::new();

    let app_state = AppState { matchmaking_state };

    let matchmaking_state_clone = app_state.matchmaking_state.clone();
    tracing::debug!("Starting matchmaking loop");
    tokio::spawn(async move {
        MatchmakingState::start_matchmaking_loop(matchmaking_state_clone, command_rx).await;
    });

    let app = Router::new()
        .route("/queue", get(queue))
        .with_state(app_state);

    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());

    axum::serve(listener, app).await.unwrap();
}
