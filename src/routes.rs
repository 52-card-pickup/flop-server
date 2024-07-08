use std::sync::Arc;

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
            "/player/:player_id/send",
            post_with(player_send, docs::player_send),
        )
        .api_route(
            "/player/:player_id/transfer",
            get_with(get_player_transfer, docs::get_player_transfer)
                .post_with(post_player_transfer, docs::post_player_transfer),
        )
        .api_route(
            "/player/:player_id/photo",
            post_with(post_player_photo, docs::post_player_photo),
        )
        .api_route(
            "/player/photo/:token",
            get_with(get_player_photo, docs::get_player_photo),
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
        ticker: game::ticker(&state),
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
        cards: game::cards_in_hand(&state, &player.id).unwrap(),
        your_turn: game::is_player_turn(&state, &player.id),
        call_amount: game::call_amount(&state).unwrap_or(0),
        min_raise_to: game::min_raise_to(&state),
        turn_expires_dt: game::turn_expires_dt(&state, &player.id),
        last_update: state.last_update.as_u64(),
        current_round_stake: game::player_stake_in_round(&state, &player.id),
    };

    Ok(Json(game_player_state))
}

pub(crate) async fn player_send(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    Json(payload): Json<models::PlayerSendRequest>,
) -> JsonResult<()> {
    let mut state = state.write().await;
    let player = utils::validate_player(&player_id, &state)?;

    if payload.message.is_empty() {
        info!(
            "Player {} failed to send message: message is empty",
            player_id
        );
        return Err(StatusCode::BAD_REQUEST);
    }
    use state::ticker::emoji::TickerEmoji;
    let emoji = match payload.message.as_str() {
        "👍" | ":+1:" => TickerEmoji::thumbs_up(),
        "👎" | ":-1:" => TickerEmoji::thumbs_down(),
        "👏" | ":clapping:" => TickerEmoji::clapping(),
        "⏳" | ":time:" => TickerEmoji::time(),
        "🤔" | ":thinking:" => TickerEmoji::thinking(),
        "😂" | ":money:" => TickerEmoji::money(),
        "😡" | ":angry:" => TickerEmoji::angry(),
        _ => {
            info!("Player {} failed to send message: invalid emoji", player_id);
            return Err(StatusCode::BAD_REQUEST);
        }
    };
    state.players.get_mut(&player.id).unwrap().emoji =
        Some((emoji.clone(), state::dt::Instant::default()));
    state
        .ticker
        .emit(state::TickerEvent::PlayerSentEmoji(player.id, emoji));

    state.last_update.set_now();
    info!("Player {} sent message", player_id);
    Ok(Json(()))
}

pub(crate) async fn get_player_transfer(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
) -> JsonResult<models::PlayerAccountsResponse> {
    let state = state.read().await;
    let player = utils::validate_player(&player_id, &state)?;

    let accounts = state
        .players
        .values()
        .filter(|p| p.id != player.id)
        .map(|p| models::PlayerAccount {
            name: p.name.clone(),
            account_id: p.funds_token.to_string(),
        })
        .collect();

    Ok(Json(models::PlayerAccountsResponse { accounts }))
}

pub(crate) async fn post_player_transfer(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    Json(payload): Json<models::TransferRequest>,
) -> JsonResult<()> {
    let mut state = state.write().await;
    let player = utils::validate_player(&player_id, &state)?;

    if payload.amount == 0 {
        info!("Player {} failed to transfer: amount is zero", player_id);
        return Err(StatusCode::BAD_REQUEST);
    }

    game::transfer_funds(&mut state, &player.id, &payload).map_err(|_| StatusCode::BAD_REQUEST)?;

    info!(
        "Player {} transferred {} to player {}",
        player.id, payload.amount, payload.to
    );

    state.last_update.set_now();
    Ok(Json(()))
}

pub(crate) async fn get_player_photo(
    State(state): State<SharedState>,
    Path(token): Path<String>,
) -> Result<(header::HeaderMap, body::Bytes), StatusCode> {
    let state = state.read().await;
    let photo = state
        .players
        .values()
        .find(|p| p.photo.as_ref().map(|(_, token)| token.as_ref()) == Some(token.as_str()))
        .and_then(|p| p.photo.as_ref())
        .ok_or(StatusCode::NOT_FOUND)?;

    let mut headers = header::HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str("inline").unwrap(),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&photo.1.to_string()).unwrap(),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000"),
    );

    let bytes: body::Bytes = photo.0.as_ref().clone();
    Ok((headers, bytes.into()))
}

pub(crate) async fn post_player_photo(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    mut multipart: Multipart,
) -> JsonResult<()> {
    let player_id = {
        let state = state.read().await;
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

    let mut state = state.write().await;
    let player = state
        .players
        .get_mut(&player_id)
        .ok_or(StatusCode::NOT_FOUND)?;

    let guid = state::token::Token::default();
    player.photo = Some((Arc::new(data), guid));
    state
        .ticker
        .emit(state::TickerEvent::PlayerPhotoUploaded(player_id.clone()));

    state.last_update.set_now();
    info!(
        "Player {} uploaded photo: name = {}, size = {}",
        player_id, name, size
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
    if payload.name.is_empty()
        || payload.name.len() > 24
        || payload.name.contains(|c: char| c.is_control())
    {
        info!("Player failed to join: name is invalid");
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut state = state.write().await;
    let name = payload.name.replace(char::is_whitespace, " ");

    let id = match game::add_new_player(&mut state, name.trim()) {
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

    pub fn player_send(op: TransformOperation) -> TransformOperation {
        op.description("Send a message to the game room.")
    }

    pub fn get_player_transfer(op: TransformOperation) -> TransformOperation {
        op.description("Get the account details of other players.")
    }

    pub fn post_player_transfer(op: TransformOperation) -> TransformOperation {
        op.description("Transfer funds to another player.")
    }

    pub fn post_player_photo(op: TransformOperation) -> TransformOperation {
        op.description("Upload a photo for a player.")
    }

    pub fn get_player_photo(op: TransformOperation) -> TransformOperation {
        op.description("Get a photo for a player.")
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
