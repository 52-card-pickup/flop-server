use crate::{
    game, models,
    state::{self, SharedState},
};

use aide::axum::{
    routing::{get_with, post_with},
    ApiRouter,
};
use axum::{
    extract::{Path, State},
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

pub(crate) async fn room(State(state): State<SharedState>) -> Json<models::GameClientRoom> {
    let state = state.read().unwrap();

    let game_client_state = models::GameClientRoom {
        state: game::game_phase(&state),
        players: game::room_players(&state),
        pot: state.round.pot,
        cards: game::cards_on_table(&state),
        last_update: state.last_update.into(),
    };

    Json(game_client_state)
}

pub(crate) async fn player(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
) -> JsonResult<models::GamePlayerState> {
    let state = state.read().unwrap();
    let player = validate_player(&player_id, &state)?;

    Ok(Json(models::GamePlayerState {
        state: game::game_phase(&state),
        balance: player.balance,
        cards: game::cards_in_hand(&state, &player.id),
        your_turn: state.round.players_turn.as_ref() == Some(&player.id),
        call_amount: game::call_amount(&state).unwrap_or(0),
        min_raise_by: game::min_raise_by(&state),
        turn_expires_dt: game::turn_expires_dt(&state, &player.id),
        last_update: state.last_update.into(),
    }))
}

pub(crate) async fn play(
    State(state): State<SharedState>,
    Json(payload): Json<models::PlayRequest>,
) -> JsonResult<()> {
    let mut state = state.write().unwrap();
    let player = validate_player(&payload.player_id, &state)?;
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
    State(state): State<SharedState>,
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

    state.last_update.set_now();
    info!("Player {} joined", id);
    Ok(Json(models::JoinResponse { id: id.to_string() }))
}

pub(crate) async fn close_room(State(state): State<SharedState>) -> JsonResult<()> {
    let mut state = state.write().unwrap();

    if state.status == state::GameStatus::Playing {
        info!("Failed to close room: game already started");
        return Err(StatusCode::BAD_REQUEST);
    }

    game::start_game(&mut state).map_err(|err| {
        info!("Failed to close room: {}", err);
        StatusCode::BAD_REQUEST
    })?;

    state.last_update.set_now();
    info!("Room closed for new players, game started");
    Ok(Json(()))
}

pub(crate) async fn reset_room(State(state): State<SharedState>) -> Json<()> {
    let mut state = state.write().unwrap();

    *state = state::State::default();

    state.last_update.set_now();
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

pub mod docs {
    use aide::transform::TransformOperation;

    // use crate::cards;

    pub fn room(op: TransformOperation) -> TransformOperation {
        op.description("Get the current state of the game room.")
        // .response_with::<200, Json<models::GameClientRoom>, _>(|res| {
        //     res.description("The current state of the game room.")
        //         .example(models::GameClientRoom {
        //             state: models::GamePhase::Playing,
        //             players: vec![models::GameClientPlayer {
        //                 name: "Player Name".to_string(),
        //                 balance: 1000,
        //             }],
        //             pot: 2000,
        //             cards: vec![
        //                 (cards::CardSuite::Hearts, cards::CardValue::Ace),
        //                 (cards::CardSuite::Hearts, cards::CardValue::Two),
        //                 (cards::CardSuite::Hearts, cards::CardValue::Three),
        //             ],
        //             last_update: 0,
        //         })
        // })
    }

    pub fn player(op: TransformOperation) -> TransformOperation {
        op.description("Get the current state of a player.")
        // .response_with::<200, Json<models::GamePlayerState>, _>(|res| {
        //     res.description("The current state of a player.")
        //         .example(models::GamePlayerState {
        //             state: models::GamePhase::Playing,
        //             balance: 1000,
        //             cards: (
        //                 (cards::CardSuite::Hearts, cards::CardValue::Ace),
        //                 (cards::CardSuite::Hearts, cards::CardValue::Two),
        //             ),
        //             your_turn: true,
        //             last_update: 0,
        //         })
        // })
    }

    pub fn play(op: TransformOperation) -> TransformOperation {
        op.description("Play a round.")
        // .request_body_with::<Json<models::PlayRequest>, _>(|req| {
        //     req.description("The player's stake.")
        //         .example(models::PlayRequest {
        //             player_id: "player-id".to_string(),
        //             stake: 100,
        //         })
        // })
        // .response_with::<200, Json<()>, _>(|res| {
        //     res.description("The player has played a round.")
        // })
    }

    pub fn join(op: TransformOperation) -> TransformOperation {
        op.description("Join the game room.")
        // .request_body_with::<Json<models::JoinRequest>, _>(|req| {
        //     req.description("The player's name.")
        //         .example(models::JoinRequest {
        //             name: "Player Name".to_string(),
        //         })
        // })
        // .response_with::<200, Json<models::JoinResponse>, _>(|res| {
        //     res.description("The player has joined the game room.")
        //         .example(models::JoinResponse {
        //             id: "player-id".to_string(),
        //         })
        // })
    }

    pub fn close_room(op: TransformOperation) -> TransformOperation {
        op.description("Close the game room for new players to join and start the game.")
        // .response_with::<200, Json<()>, _>(|res| {
        //     res.description("The game room is closed for new players.")
        // })
    }

    pub fn reset_room(op: TransformOperation) -> TransformOperation {
        op.description("Reset the game room.")
        // .response_with::<200, Json<()>, _>(|res| {
        //     res.description("The game room has been reset.")
        // })
    }
}
