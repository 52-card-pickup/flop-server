use std::sync::{Arc, OnceLock};

use crate::{
    app_metrics::{metrics_labels, Metrics},
    game, layer, models,
    state::{self, SharedState},
};

use aide::axum::{
    routing::{get_with, post_with},
    ApiRouter,
};
use autometrics::autometrics;
use axum::{
    body,
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderValue, StatusCode},
    Extension, Json,
};
use axum_extra::TypedHeader;
use tracing::info;

type JsonResult<T> = Result<Json<T>, StatusCode>;

pub(crate) fn api_routes(state: state::SharedState) -> ApiRouter {
    ApiRouter::new()
        .api_route("/room", get_with(room, docs::room))
        .api_route("/room/peek", post_with(peek_room, docs::peek_room))
        .api_route("/room/close", post_with(close_room, docs::close_room))
        .api_route("/room/reset", post_with(reset_room, docs::reset_room))
        .api_route("/pair", post_with(pair, docs::pair))
        .api_route("/player/:player_id", get_with(player, docs::player))
        .api_route(
            "/player/:player_id/leave",
            post_with(player_leave, docs::player_leave),
        )
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
        .api_route("/new", post_with(new_room, docs::new_room))
        .api_route("/join", post_with(join, docs::join))
        .api_route("/resume", post_with(resume, docs::resume))
        .api_route("/play", post_with(play, docs::play))
        .with_state(state)
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn room(
    State(state): State<SharedState>,
    Extension(layer::Apid(apid)): Extension<layer::Apid>,
    Query(query): Query<models::PollQuery>,
    room_code: Option<TypedHeader<models::headers::RoomCodeHeader>>,
) -> JsonResult<models::GameClientRoom> {
    static EMPTY: OnceLock<state::RoomState> = OnceLock::new();

    let shared_state = state.clone();
    let room_code = match utils::wait_by_room_code(&state, query.clone(), room_code).await {
        Ok(room_code) => Some(room_code),
        Err(StatusCode::NOT_FOUND) => None,
        Err(status) => return Err(status),
    };
    let state = match &room_code {
        Some(room_code) => state
            .get_room(&room_code)
            .await
            .ok_or(StatusCode::NOT_FOUND)?,
        None => EMPTY
            .get_or_init(|| {
                let mut state = state::State::default();
                state.status = state::GameStatus::Idle;
                state.into()
            })
            .clone(),
    };

    let state = state.read().await;
    let (room_code, pair_screen_code) = match state.status {
        state::GameStatus::Idle => utils::wait_by_screen_apid(&shared_state, query, &apid)
            .await
            .map(|(room, screen)| (room.or(room_code), Some(screen)))?,
        _ => (room_code, None),
    };

    let game_client_state = models::GameClientRoom {
        state: game::game_phase(&state),
        players: game::room_players(&state),
        pot: state.round.pot,
        cards: game::cards_on_table(&state),
        completed: game::completed_game(&state),
        ticker: game::ticker(&state),
        room_code: room_code.map(|r| r.to_string()),
        pair_screen_code: pair_screen_code.map(|c| c.to_string()),
        last_update: state.last_update.as_u64(),
    };

    Ok(Json(game_client_state))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn player(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    Query(query): Query<models::PollQuery>,
) -> JsonResult<models::GamePlayerState> {
    let player = utils::wait_by_player_id(&state, query, &player_id).await?;

    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let state = state.read().await;

    let game_player_state = models::GamePlayerState {
        state: game::game_phase(&state),
        balance: player.balance,
        cards: game::cards_in_hand(&state, &player.id).unwrap(),
        your_turn: game::is_player_turn(&state, &player.id),
        call_amount: game::call_amount(&state).unwrap_or(0),
        min_raise_to: game::min_raise_to(&state),
        players_count: state.players.len(),
        turn_expires_dt: game::turn_expires_dt(&state, &player.id),
        last_update: state.last_update.as_u64(),
        current_round_stake: game::player_stake_in_round(&state, &player.id),
    };

    Ok(Json(game_player_state))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn player_leave(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
) -> JsonResult<()> {
    let player = utils::validate_player(&player_id, &state).await?;
    let shared_state = state.clone();
    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let mut state = state.write().await;

    game::remove_player(&mut state, &player.id).map_err(|err| {
        info!("Player {} failed to leave: {}", player_id, err);
        StatusCode::BAD_REQUEST
    })?;

    shared_state.remove(&player.id).await;

    state.last_update.set_now();
    info!("Player {} left", player_id);

    Ok(Json(()))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn player_send(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    Json(payload): Json<models::PlayerSendRequest>,
) -> JsonResult<()> {
    let player = utils::validate_player(&player_id, &state).await?;
    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let mut state = state.write().await;

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

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn get_player_transfer(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
) -> JsonResult<models::PlayerAccountsResponse> {
    let player = utils::validate_player(&player_id, &state).await?;
    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let state = state.read().await;

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

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn post_player_transfer(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    Json(payload): Json<models::TransferRequest>,
) -> JsonResult<()> {
    let player = utils::validate_player(&player_id, &state).await?;
    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let mut state = state.write().await;

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

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn get_player_photo(
    State(state): State<SharedState>,
    Path(token): Path<String>,
) -> Result<(header::HeaderMap, body::Bytes), StatusCode> {
    // TODO: accept room code to prevent scanning all rooms
    let state = {
        let mut matched = None;
        for room_state in state.iter().await {
            let state = room_state.read().await;
            if state.players.values().any(|p| {
                p.photo
                    .as_ref()
                    .map(|state::PlayerPhoto(_, t)| t.to_string())
                    .as_deref()
                    == Some(token.as_str())
            }) {
                drop(state);
                matched = Some(room_state);
                break;
            }
        }
        matched.ok_or(StatusCode::NOT_FOUND)?
    };

    let state = state.read().await;
    let photo = state
        .players
        .values()
        .find(|p| {
            p.photo
                .as_ref()
                .map(|state::PlayerPhoto(_, token)| token.as_ref())
                == Some(token.as_str())
        })
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

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn post_player_photo(
    State(state): State<SharedState>,
    Path(player_id): Path<String>,
    mut multipart: Multipart,
) -> JsonResult<()> {
    let player = utils::validate_player(&player_id, &state).await?;
    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let player_id = player.id;

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
    player.photo = Some(state::PlayerPhoto(Arc::new(data), guid));
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

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn play(
    State(state): State<SharedState>,
    Json(payload): Json<models::PlayRequest>,
) -> JsonResult<()> {
    let player = utils::validate_player(&payload.player_id, &state).await?;
    let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
    let mut state = state.write().await;
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

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn join(
    State(state): State<SharedState>,
    Extension(layer::Apid(apid)): Extension<layer::Apid>,
    Json(payload): Json<models::JoinRequest>,
) -> JsonResult<models::JoinResponse> {
    if payload.name.is_empty()
        || payload.name.len() > 24
        || payload.name.contains(|c: char| c.is_control())
    {
        info!("Player failed to join: name is invalid");
        return Err(StatusCode::BAD_REQUEST);
    }

    let req_room_code: Option<state::room::RoomCode> = match payload.room_code {
        Some(room_code) => Some(room_code.parse().map_err(|_| StatusCode::BAD_REQUEST)?),
        None => None,
    };
    let player_id = state::PlayerId::default();
    info!("Player {} joining room = {:?}", player_id, req_room_code);
    let room_code = state
        .join_room(&player_id, req_room_code.as_ref())
        .await
        .map_err(|_| {
            info!(
                "Player failed to join room, room not found: room code = {:?}, player id = {}",
                req_room_code, player_id
            );
            StatusCode::NOT_FOUND
        })?;
    info!("Player {} joined room = {:?}", player_id, room_code);

    Metrics::c_room_requests_total_incr(metrics_labels::room_requests(&room_code.to_string()));

    let state = state
        .get_room(&room_code)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut state = state.write().await;

    let id = match game::add_new_player(&mut state, &payload.name, player_id) {
        Ok(id) => id,
        Err(err) => {
            info!("Player failed to join: {}", err);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    game::set_player_apid(&mut state, &id, &apid);

    state.last_update.set_now();

    info!("Player {} joined with name '{}'", id, payload.name);
    Metrics::c_players_total_incr();

    Ok(Json(models::JoinResponse {
        id: id.to_string(),
        room_code: room_code.to_string(),
    }))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn resume(
    State(state): State<SharedState>,
    Extension(layer::Apid(apid)): Extension<layer::Apid>,
    Json(payload): Json<models::ResumeRequest>,
) -> JsonResult<models::ResumeResponse> {
    info!("Resuming previous session for anonymous player id {}", apid);

    let shared_state = state.clone();
    let room_state = utils::query_room_state(&state, payload.room_code.clone()).await?;
    let mut state = room_state.write().await;

    let player = {
        match state.players.promote_dormant(&apid) {
            Some(player) => {
                let room_code = payload
                    .room_code
                    .as_ref()
                    .and_then(|room_code| room_code.parse().ok());

                _ = shared_state.join_room(&player.id, room_code.as_ref()).await;

                state
                    .players
                    .get_mut(&player.id)
                    .expect("player not found")
                    .folded = true;

                Metrics::c_players_total_incr();

                Some(player)
            }
            None => state.players.get_non_dormant(&apid).cloned(),
        }
    }
    .ok_or_else(|| StatusCode::NOT_FOUND)?;

    state
        .ticker
        .emit(state::TickerEvent::PlayerResumed(player.id.clone()));

    state.last_update.set_now();
    info!("Player {} resumed", player.id);

    Ok(Json(models::ResumeResponse {
        id: player.id.to_string(),
        name: player.name,
    }))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn new_room(
    State(state): State<SharedState>,
    Extension(layer::Apid(apid)): Extension<layer::Apid>,
    Json(payload): Json<models::NewRoomRequest>,
) -> JsonResult<models::NewRoomResponse> {
    let player_id = state::PlayerId::default();
    info!("Creating new room for player {}", player_id);

    let room_code = state.create_room(&player_id).await;

    info!("New room created for player {}: {:?}", player_id, room_code);
    Metrics::c_room_requests_total_incr(metrics_labels::room_requests(&room_code.to_string()));

    let state = state
        .get_room(&room_code)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut state = state.write().await;

    let id = match game::add_new_player(&mut state, &payload.name, player_id) {
        Ok(id) => id,
        Err(err) => {
            info!("Player failed to join: {}", err);
            return Err(StatusCode::BAD_REQUEST);
        }
    };

    game::set_player_apid(&mut state, &id, &apid);

    state.last_update.set_now();

    info!("Player {} joined with name '{}'", id, payload.name);
    Metrics::c_players_total_incr();

    Ok(Json(models::NewRoomResponse {
        id: id.to_string(),
        room_code: room_code.to_string(),
    }))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn peek_room(
    State(state): State<SharedState>,
    Extension(layer::Apid(apid)): Extension<layer::Apid>,
    Json(payload): Json<models::PeekRoomRequest>,
) -> JsonResult<models::PeekRoomResponse> {
    let state = utils::query_room_state(&state, Some(payload.room_code)).await?;
    let state = state.read().await;

    let resume_player_name = state
        .players
        .peek_dormant(&apid)
        .or_else(|| state.players.get_non_dormant(&apid))
        .map(|p| p.name.clone());

    let peek = models::PeekRoomResponse {
        state: game::game_phase(&state),
        players_count: state.players.len(),
        can_resume: resume_player_name.is_some(),
        resume_player_name,
    };

    Ok(Json(peek))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn close_room(
    State(state): State<SharedState>,
    json: Option<Json<models::CloseRoomRequest>>,
) -> JsonResult<()> {
    let room_code = json.and_then(|Json(payload)| payload.room_code);
    let state = utils::query_room_state(&state, room_code).await?;
    let mut state = state.write().await;

    game::start_game(&mut state).map_err(|err| {
        info!("Failed to close room: {}", err);
        StatusCode::BAD_REQUEST
    })?;

    state.last_update.set_now();

    info!("Room closed for new players, game started");
    Ok(Json(()))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn reset_room(
    State(state): State<SharedState>,
    room_code: Option<TypedHeader<models::headers::RoomCodeHeader>>,
) -> JsonResult<()> {
    let room_code = room_code.map(|TypedHeader(room_code)| room_code.into());
    let state = utils::query_room_state(&state, room_code).await?;
    let mut state = state.write().await;

    *state = state::State::default();

    state.last_update.set_now();

    info!("Game reset");
    Ok(Json(()))
}

#[autometrics(ok_if = metrics::is_success)]
pub(crate) async fn pair(
    State(state): State<SharedState>,
    Json(payload): Json<models::PairRequest>,
) -> JsonResult<()> {
    let screen_code = payload.screen_code.parse().map_err(|_| {
        info!(
            "Failed to pair big screen: invalid screen code '{}'",
            payload.screen_code
        );
        StatusCode::BAD_REQUEST
    })?;

    let room_code = payload.room_code.parse().map_err(|_| {
        info!(
            "Failed to pair big screen: invalid room code '{}'",
            payload.room_code
        );
        StatusCode::BAD_REQUEST
    })?;

    state
        .pair_screen_with_room(&screen_code, &room_code)
        .await
        .map_err(|_| {
            info!(
                "Failed to pair big screen: screen code '{:?}' or room code '{:?}' not found",
                screen_code, room_code
            );
            StatusCode::NOT_FOUND
        })?;

    Ok(Json(()))
}

mod utils {
    use autometrics::autometrics;
    use axum::http::StatusCode;
    use axum_extra::TypedHeader;
    use tracing::info;

    use crate::{
        app_metrics::{metrics_labels, Metrics},
        models, state,
    };

    #[autometrics]
    pub async fn validate_player(
        player_id: &str,
        state: &state::SharedState,
    ) -> Result<state::Player, StatusCode> {
        let player_id = player_id.parse().map_err(|_| {
            info!("Player {} failed: invalid player id", player_id);
            StatusCode::BAD_REQUEST
        })?;

        let state = state.get(&player_id).await.ok_or(StatusCode::NOT_FOUND)?;
        let state = state.read().await;

        match state.players.get(&player_id) {
            Some(player) => Ok(player.clone()),
            None => Err(StatusCode::NOT_FOUND),
        }
    }

    pub async fn query_room_state(
        state: &state::SharedState,
        room_code: Option<String>,
    ) -> Result<state::RoomState, StatusCode> {
        let state = match room_code.filter(|s: &String| !s.is_empty()) {
            Some(room_code_str) => {
                let room_code = room_code_str.parse().map_err(|_| {
                    info!(
                        "Failed to wait for room update: invalid room code '{}'",
                        room_code_str
                    );
                    StatusCode::BAD_REQUEST
                })?;

                let room_state = state.get_room(&room_code).await;
                if room_state.is_some() {
                    let labels = metrics_labels::room_requests(&room_code_str);
                    Metrics::c_room_requests_total_incr(labels);
                }
                room_state
            }
            None => state.get_default_room().await,
        };

        state.ok_or(StatusCode::NOT_FOUND)
    }

    pub async fn wait_by_player_id(
        state: &state::SharedState,
        query: models::PollQuery,
        player_id: &str,
    ) -> Result<state::Player, StatusCode> {
        let player = validate_player(player_id, state).await?;
        let state = state.get(&player.id).await.ok_or(StatusCode::NOT_FOUND)?;
        wait_for_update(&state, query).await;

        Ok(player)
    }

    pub async fn wait_by_room_code(
        state: &state::SharedState,
        query: models::PollQuery,
        room_code: Option<TypedHeader<models::headers::RoomCodeHeader>>,
    ) -> Result<state::room::RoomCode, StatusCode> {
        let room_code: Option<String> = room_code.map(|TypedHeader(room_code)| room_code.into());
        let room_code = match room_code.filter(|s: &String| !s.is_empty()) {
            Some(room_code) => {
                let room_code: state::room::RoomCode = room_code.parse().map_err(|_| {
                    info!(
                        "Failed to wait for room update: invalid room code '{}'",
                        room_code
                    );
                    StatusCode::BAD_REQUEST
                })?;

                room_code
            }
            None => state
                .get_default_room_code()
                .await
                .ok_or(StatusCode::NOT_FOUND)?,
        };

        let state = state
            .get_room(&room_code)
            .await
            .ok_or(StatusCode::NOT_FOUND)?;

        wait_for_update(&state, query).await;

        Ok(room_code)
    }

    pub async fn wait_by_screen_apid(
        state: &state::SharedState,
        query: models::PollQuery,
        apid: &str,
    ) -> Result<
        (
            Option<state::room::RoomCode>,
            state::screens::PairScreenCode,
        ),
        StatusCode,
    > {
        let (room_code, pair_screen_code) = match state.register_big_screen(&apid).await {
            Some(code) => (None, code),
            None => {
                let (code, screen) = state
                    .get_big_screen_by_apid(&apid)
                    .await
                    .ok_or(StatusCode::NOT_FOUND)?;
                let changed = wait_for_screen_update(&screen, query).await;
                if changed {
                    let screen = state.get_big_screen_by_code(&code).await;
                    let screen = screen.ok_or(StatusCode::NOT_FOUND)?;
                    (screen.room_code, code)
                } else {
                    (screen.room_code, code)
                }
            }
        };

        Ok((room_code, pair_screen_code))
    }

    async fn wait_for_update(state: &state::RoomState, query: models::PollQuery) {
        if let Some(last_update) = query.since {
            let rx = {
                let state = state.read().await;
                state.last_update.wait_for(last_update.into())
            };

            tokio::select! {
                _ = rx => {}
                _ = sleep_from_timeout_query(query.timeout) => {}
            }
        }
    }

    async fn wait_for_screen_update(
        screen: &state::screens::Screen,
        query: models::PollQuery,
    ) -> bool {
        match query.since {
            Some(last_update) => {
                let rx = screen.last_update.wait_for(last_update.into());

                tokio::select! {
                    _ = rx => true,
                    _ = sleep_from_timeout_query(query.timeout) => false,
                }
            }
            _ => false,
        }
    }

    async fn sleep_from_timeout_query(timeout: Option<u64>) {
        let timeout_ms = timeout.unwrap_or(5_000);
        let timeout = std::time::Duration::from_millis(timeout_ms);
        tokio::time::sleep(timeout).await;
    }
}

mod metrics {
    use axum::http::StatusCode;

    pub fn is_success<T>(response: &Result<T, StatusCode>) -> bool {
        !matches!(
            response.as_ref().err(),
            Some(&StatusCode::OK) | Some(&StatusCode::NOT_FOUND)
        )
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

    pub fn player_leave(op: TransformOperation) -> TransformOperation {
        op.description("Leave the game room.")
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

    pub fn new_room(op: TransformOperation) -> TransformOperation {
        op.description("Create and join a new game room.")
    }

    pub fn join(op: TransformOperation) -> TransformOperation {
        op.description("Join the game room.")
    }

    pub fn resume(op: TransformOperation) -> TransformOperation {
        op.description("Resume previous session in the game room.")
    }

    pub fn peek_room(op: TransformOperation) -> TransformOperation {
        op.description("Peek at the game room from join code.")
    }

    pub fn close_room(op: TransformOperation) -> TransformOperation {
        op.description("Close the game room for new players to join and start the game.")
    }

    pub fn reset_room(op: TransformOperation) -> TransformOperation {
        op.description("Reset the game room.")
    }

    pub fn pair(op: TransformOperation) -> TransformOperation {
        op.description("Pairs a big screen with a room.")
    }
}
