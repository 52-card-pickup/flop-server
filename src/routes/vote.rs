use crate::models;
use crate::state::State;
use crate::vote::player_start_vote;
use aide::axum::ApiRouter;
use axum::{extract::Extension, routing::post, Json};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

pub fn vote_subroutes(state: Arc<RwLock<State>>) -> ApiRouter {
    ApiRouter::new()
        .route("/start", post(start_vote))
        .layer(Extension(state))
}

async fn start_vote(
    state: Extension<Arc<RwLock<State>>>,
    Json(payload): Json<models::StartVoteRequest>,
) -> Json<()> {
    let mut state = state.write().await;

    state.last_update.set_now();

    let id = payload.player_id;
    let motion = payload.motion;
    player_start_vote(&mut state, motion).unwrap();

    info!("Vote started by player {}", id);
    Json(())
}
