use crate::{
    game, models,
    state::{self, SharedState},
};

use aide::axum::{
    routing::{get_with, post_with},
    ApiRouter,
};
use axum::{
    body,
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderValue, StatusCode},
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
        .api_route(
            "/player/:player_id/photo",
            get_with(get_player_photo, docs::get_player_photo)
                .post_with(post_player_photo, docs::post_player_photo),
        )
        .api_route("/join", post_with(join, docs::join))
        .api_route("/play", post_with(play, docs::play))
        .with_state(state)
}

pub(crate) async fn room(
    State(state): State<SharedState>,
    Query(query): Query<models::PollQuery>,
) -> Json<models::GameClientRoom> {
    utils::wait_for_update(&state, query).await;

    let state = state.read().await;

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
    Query(query): Query<models::PollQuery>,
) -> JsonResult<models::GamePlayerState> {
    utils::wait_for_update(&state, query).await;

    let state = state.read().await;
    let player = utils::validate_player(&player_id, &state)?;

    let game_player_state = models::GamePlayerState {
        state: game::game_phase(&state),
        balance: player.balance,
        cards: game::cards_in_hand(&state, &player.id),
        your_turn: state.round.players_turn.as_ref() == Some(&player.id),
        call_amount: game::call_amount(&state).unwrap_or(0),
        min_raise_to: game::min_raise_to(&state),
        turn_expires_dt: game::turn_expires_dt(&state, &player.id),
        last_update: state.last_update.as_u64(),
        current_round_stake: game::player_stake_in_round(&state, &player.id),
    };

    Ok(Json(game_player_state))
}

pub(crate) async fn get_player_photo(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    Query(hash): Query<Option<(String, String)>>,
) -> Result<(header::HeaderMap, body::Bytes), StatusCode> {
    let state = state.read().unwrap();
    let player = utils::validate_player(&player_id, &state)?;

    let photo = player.photo.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let mut headers = header::HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str("inline").unwrap(),
    );
    if hash.map_or(false, |(key, _)| key == "hash") {
        headers.insert(
            header::ETAG,
            HeaderValue::from_str(&photo.1.to_string()).unwrap(),
        );
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000"),
        );
    }

    Ok((headers, photo.clone().0.into()))
}

pub(crate) async fn post_player_photo(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    mut multipart: Multipart,
) -> JsonResult<()> {
    let player_id = {
        let state = state.read().unwrap();
        utils::validate_player(&player_id, &state)?.id
    };

    let field = multipart
        .next_field()
        .await
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .ok_or(StatusCode::BAD_REQUEST)?;

    if field.content_type() != Some("image/jpeg") {
        info!(
            "Player {} failed to upload photo: invalid content type",
            player_id
        );
        return Err(StatusCode::BAD_REQUEST);
    }

    let name = field.name().unwrap().to_string();
    let data = field.bytes().await.unwrap();
    let size = data.len();

    let mut state = state.write().unwrap();
    let player = state
        .players
        .get_mut(&player_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let player_name = player.name.clone();
    let guid = uuid::Uuid::new_v4();
    player.photo = Some((data, guid));

    info!(
        "Player {} uploaded photo: name = {}, size = {}",
        player_name, name, size
    );

    Ok(Json(()))
}

pub(crate) async fn play(
    State(state): State<SharedState>,
    Json(payload): Json<models::PlayRequest>,
) -> JsonResult<()> {
    let mut state = state.write().await;
    let player = utils::validate_player(&payload.player_id, &state)?;
    if let Err(err) = game::reset_ttl(&mut state, &player.id) {
        info!("Player {} failed to play: {}", payload.player_id, err);
        return Err(StatusCode::BAD_REQUEST);
    }

    let result = match payload.action {
        models::PlayAction::Check => {
            game::accept_player_bet(&mut state, &player.id, state::BetAction::Check)
        }
        models::PlayAction::Call => {
            game::accept_player_bet(&mut state, &player.id, state::BetAction::Call)
        }
        models::PlayAction::RaiseTo => game::accept_player_bet(
            &mut state,
            &player.id,
            state::BetAction::RaiseTo(payload.stake),
        ),
        models::PlayAction::Fold => game::fold_player(&mut state, &player.id),
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
    let mut state = state.write().await;

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
    let mut state = state.write().await;

    game::start_game(&mut state).map_err(|err| {
        info!("Failed to close room: {}", err);
        StatusCode::BAD_REQUEST
    })?;

    state.last_update.set_now();

    info!("Room closed for new players, game started");
    Ok(Json(()))
}

pub(crate) async fn reset_room(State(state): State<SharedState>) -> Json<()> {
    let mut state = state.write().await;

    *state = state::State::default();

    state.last_update.set_now();

    info!("Game reset");
    Json(())
}

mod utils {
    use axum::http::StatusCode;
    use tracing::info;

    use crate::{
        models,
        state::{self, SharedState},
    };

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

    pub async fn wait_for_update(state: &SharedState, query: models::PollQuery) {
        if let Some(last_update) = query.since {
            let rx = {
                let state = state.read().await;
                state.last_update.wait_for(last_update.into())
            };

            let timeout_ms = query.timeout.unwrap_or(5_000);
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

    pub fn get_player_photo(op: TransformOperation) -> TransformOperation {
        op.description("Get a photo for a player.")
    }

    pub fn post_player_photo(op: TransformOperation) -> TransformOperation {
        op.description("Upload a photo for a player.")
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
