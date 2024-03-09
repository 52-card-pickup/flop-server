use std::sync::{Arc, RwLock};

use axum::{
    routing::{get, post},
    Router,
};
use tracing::info;

mod cards;
mod game;
mod models;
mod routes;
mod state;

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    // initialize state
    let state = state::State::default();
    let state: state::SharedState = Arc::new(RwLock::new(state));

    // build our application with a route
    let app = Router::new()
        .with_state(state)
        .route("/api/v1/room", get(routes::room))
        .route("/api/v1/room/close", post(routes::close_room))
        .route("/api/v1/room/reset", post(routes::reset_room))
        .route("/api/v1/player/:player_id", get(routes::player))
        .route("/api/v1/join", post(routes::join))
        .route("/api/v1/play", post(routes::play));

    // run our app with hyper, listening globally on port 5000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:5000").await.unwrap();
    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

mod utils {
    use std::collections::BTreeMap;

    use crate::state::{Player, PlayerId};

    pub fn now() -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    pub fn get_next_players_turn(
        players: &BTreeMap<PlayerId, Player>,
        current_player_id: &PlayerId,
    ) -> Option<PlayerId> {
        players
            .iter()
            .chain(players.iter())
            .skip_while(|(id, _)| id != &current_player_id)
            .skip(1)
            .next()
            .map(|(id, _)| id.clone())
    }
}
