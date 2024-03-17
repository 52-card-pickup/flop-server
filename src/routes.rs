use std::collections::HashMap;

use crate::{
    game, models,
    state::{self, SharedState},
};

use aide::axum::{
    routing::{get_with, post_with},
    ApiRouter,
};
use axum::{
    extract::{self, Path, State},
    http::StatusCode,
    Json,
};
use tracing::info;

type JsonResult<T> = Result<Json<T>, StatusCode>;

pub(crate) fn api_routes(state: state::SharedState) -> ApiRouter {
    ApiRouter::new()
        .api_route("/room", get_with(room, docs::room))
        .api_route("/room/close", post_with(close_room, docs::close_room))
        .api_route("/room/reset", post_with(reset_room, docs::reset_room))
        .api_route("/player/:player_id", get_with(player, docs::player))
        .api_route("/join", post_with(join, docs::join))
        .api_route("/play", post_with(play, docs::play))
        .with_state(state)
}

pub(crate) async fn room(
    extract::State(state): State<SharedState>,
    extract::Query(query): extract::Query<HashMap<String, String>>,
) -> Json<models::GameClientRoom> {
    utils::wait_for_update(&state, query).await;

    let state = state.read().unwrap();

    let game_client_state = models::GameClientRoom {
        state: game::game_phase(&state),
        players: game::room_players(&state),
        pot: state.round.pot,
        cards: game::cards_on_table(&state),
        completed: game::completed_game(&state),
        last_update: state.last_update.as_u64(),
    };

    Json(game_client_state)
}

pub(crate) async fn player(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    extract::Query(query): extract::Query<HashMap<String, String>>,
) -> JsonResult<models::GamePlayerState> {
    utils::wait_for_update(&state, query).await;

    let state = state.read().unwrap();
    let player = utils::validate_player(&player_id, &state)?;

    let game_player_state = models::GamePlayerState {
        state: game::game_phase(&state),
        balance: player.balance,
        cards: game::cards_in_hand(&state, &player.id),
        your_turn: state.round.players_turn.as_ref() == Some(&player.id),
        call_amount: game::call_amount(&state).unwrap_or(0),
        min_raise_by: game::min_raise_by(&state),
        turn_expires_dt: game::turn_expires_dt(&state, &player.id),
        last_update: state.last_update.as_u64(),
    };

    Ok(Json(game_player_state))
}

pub(crate) async fn play(
    State(shared_state): State<SharedState>,
    Json(payload): Json<models::PlayRequest>,
) -> JsonResult<()> {
    let mut state = shared_state.write().unwrap();
    let player = utils::validate_player(&payload.player_id, &state)?;
    if let Err(err) = game::reset_ttl(&mut state, &player.id) {
        info!("Player {} failed to play: {}", payload.player_id, err);
        return Err(StatusCode::BAD_REQUEST);
    }

    let result = match payload.action {
        models::PlayAction::Fold => game::fold_player(&mut state, &player.id),
        _ => game::accept_player_stake(&mut state, &player.id, payload.stake, payload.action),
    };

    if let Err(err) = result {
        info!(
            "Player {} tried to play, but failed: {}",
            payload.player_id, err
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    state.last_update.set_now();
    info!("Player {} played round", payload.player_id);
    Ok(Json(()))
}

pub(crate) async fn join(
    State(shared_state): State<SharedState>,
    Json(payload): Json<models::JoinRequest>,
) -> JsonResult<models::JoinResponse> {
    let mut state = shared_state.write().unwrap();

    if payload.name.is_empty()
        || payload.name.len() > 20
        || payload.name.contains(|c: char| c.is_control())
    {
        info!("Player failed to join: name is invalid");
        return Err(StatusCode::BAD_REQUEST);
    }

    let id = match game::add_new_player(&mut state, &payload.name) {
        Ok(id) => id,
        Err(err) => {
            info!("Player failed to join: {}", err);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    state.last_update.set_now();

    info!("Player {} joined", id);
    Ok(Json(models::JoinResponse { id: id.to_string() }))
}

pub(crate) async fn close_room(State(shared_state): State<SharedState>) -> JsonResult<()> {
    let mut state = shared_state.write().unwrap();

    game::start_game(&mut state).map_err(|err| {
        info!("Failed to close room: {}", err);
        StatusCode::BAD_REQUEST
    })?;

    state.last_update.set_now();

    info!("Room closed for new players, game started");
    Ok(Json(()))
}

pub(crate) async fn reset_room(State(shared_state): State<SharedState>) -> Json<()> {
    let mut state = shared_state.write().unwrap();

    *state = state::State::default();

    state.last_update.set_now();

    info!("Game reset");
    Json(())
}

mod utils {
    use std::collections::HashMap;

    use axum::http::StatusCode;
    use tracing::info;

    use crate::state;

    pub fn validate_player(
        player_id: &str,
        state: &state::State,
    ) -> Result<state::Player, StatusCode> {
        let player_id = player_id.parse().map_err(|_| {
            info!("Player {} failed: invalid player id", player_id);
            StatusCode::BAD_REQUEST
        })?;

        match state.players.get(&player_id) {
            Some(player) => Ok(player.clone()),
            None => Err(StatusCode::NOT_FOUND),
        }
    }

    pub async fn wait_for_update(
        state: &std::sync::Arc<std::sync::RwLock<state::State>>,
        query: HashMap<String, String>,
    ) {
        if let Some(last_update) = query.get("since").and_then(|s| s.parse::<u64>().ok()) {
            let rx = {
                let state = state.read().unwrap();
                state.last_update.wait_for(last_update.into())
            };

            let timeout_ms = query
                .get("timeout")
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(5_000);
            let timeout = std::time::Duration::from_millis(timeout_ms);

            tokio::select! {
                _ = rx => {}
                _ = tokio::time::sleep(timeout) => {}
            }
        }
    }
}

pub mod docs {
    use aide::transform::TransformOperation;

    pub fn room(op: TransformOperation) -> TransformOperation {
        op.description("Get the current state of the game room.")
    }

    pub fn player(op: TransformOperation) -> TransformOperation {
        op.description("Get the current state of a player.")
    }

    pub fn play(op: TransformOperation) -> TransformOperation {
        op.description("Play a round.")
    }

    pub fn join(op: TransformOperation) -> TransformOperation {
        op.description("Join the game room.")
    }

    pub fn close_room(op: TransformOperation) -> TransformOperation {
        op.description("Close the game room for new players to join and start the game.")
    }

    pub fn reset_room(op: TransformOperation) -> TransformOperation {
        op.description("Reset the game room.")
    }
}
