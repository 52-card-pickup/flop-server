use crate::{
    game, models,
    state::{self, SharedState},
    utils::now,
};

use axum::{extract::Path, http::StatusCode, Extension, Json};
use tracing::info;

type JsonResult<T> = Result<Json<T>, StatusCode>;

pub(crate) async fn room(Extension(state): Extension<SharedState>) -> Json<models::GameClientRoom> {
    let state = state.read().unwrap();

    let game_client_state = models::GameClientRoom {
        state: game::game_phase(&state),
        players: game::room_players(&state),
        pot: state.pot,
        cards: game::cards_on_table(&state),
        last_update: state.last_update,
    };

    Json(game_client_state)
}

pub(crate) async fn player(
    Extension(state): Extension<SharedState>,
    Path(player_id): Path<String>,
) -> JsonResult<models::GamePlayerState> {
    let state = state.read().unwrap();
    let player = validate_player(&player_id, &state)?;

    Ok(Json(models::GamePlayerState {
        state: game::game_phase(&state),
        balance: player.balance,
        cards: game::cards_in_hand(&state, &player.id),
        your_turn: state.players_turn == Some(player.id),
        last_update: state.last_update,
    }))
}

pub(crate) async fn play(
    Extension(state): Extension<SharedState>,
    Json(payload): Json<models::PlayRequest>,
) -> JsonResult<()> {
    if payload.stake <= 0 {
        info!(
            "Player {} tried to play, but failed: stake is {}",
            payload.player_id, payload.stake
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut state = state.write().unwrap();
    let player = validate_player(&payload.player_id, &state)?;

    if let Err(err) = game::accept_player_stake(&mut state, &player.id, payload.stake) {
        info!(
            "Player {} tried to play, but failed: {}",
            payload.player_id, err
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    state.last_update = now();
    info!("Player {} played round", payload.player_id);
    Ok(Json(()))
}

pub(crate) async fn join(
    Extension(state): Extension<SharedState>,
    Json(payload): Json<models::JoinRequest>,
) -> JsonResult<models::JoinResponse> {
    let mut state = state.write().unwrap();

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

    state.last_update = now();
    info!("Player {} joined", id);
    Ok(Json(models::JoinResponse { id: id.to_string() }))
}

pub(crate) async fn close_room(Extension(state): Extension<SharedState>) -> JsonResult<()> {
    let mut state = state.write().unwrap();

    if state.status == state::GameStatus::Playing {
        info!("Failed to close room: game already started");
        return Err(StatusCode::BAD_REQUEST);
    }

    game::start_game(&mut state).map_err(|err| {
        info!("Failed to close room: {}", err);
        StatusCode::BAD_REQUEST
    })?;

    state.last_update = now();
    info!("Room closed for new players, game started");
    Ok(Json(()))
}

pub(crate) async fn reset_room(Extension(state): Extension<SharedState>) -> Json<()> {
    let mut state = state.write().unwrap();

    *state = state::State::default();

    state.last_update = now();
    info!("Game reset");
    Json(())
}

fn validate_player(player_id: &str, state: &state::State) -> Result<state::Player, StatusCode> {
    let player_id = player_id.parse().map_err(|_| {
        info!("Player {} failed: invalid player id", player_id);
        StatusCode::BAD_REQUEST
    })?;

    match state.players.get(&player_id) {
        Some(player) => Ok(player.clone()),
        None => {
            info!("Player {} failed: player not found", player_id);
            Err(StatusCode::NOT_FOUND)
        }
    }
}
