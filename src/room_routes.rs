use std::sync::Arc;

use aide::axum::{routing::get_with, ApiRouter};
use axum::extract::{Path, State};

use crate::{game, models, state};

pub fn room_routes(states: Vec<state::SharedState>) -> ApiRouter {
    let states = Arc::new(states);

    ApiRouter::new()
        .api_route("/available", get_with(available, docs::available))
        .api_route("/find/:join_code", get_with(find, docs::find))
        .with_state(states)
}

async fn available(
    State(states): State<Arc<Vec<state::SharedState>>>,
) -> axum::Json<Option<models::RoomAvailable>> {
    for (idx, state) in states.iter().enumerate() {
        let Ok(state) = state.read() else { continue };
        if state.last_update.triggered() {
            continue;
        }
        let available = models::RoomAvailable {
            room_url: room_url(idx),
            status: game::game_phase(&state),
            player_count: state.players.len(),
            join_code: state.join_code.to_string(),
        };
        return axum::Json(Some(available));
    }
    axum::Json(None)
}

async fn find(
    Path(join_code): Path<String>,
    State(states): State<Arc<Vec<state::SharedState>>>,
) -> Result<axum::Json<models::RoomAvailable>, axum::http::StatusCode> {
    for (idx, state) in states.iter().enumerate() {
        let Ok(state) = state.read() else { continue };
        match join_code.as_str().try_into() {
            Ok(join_code) if state.join_code == join_code => {
                let available = models::RoomAvailable {
                    room_url: room_url(idx),
                    status: game::game_phase(&state),
                    player_count: state.players.len(),
                    join_code: state.join_code.to_string(),
                };
                return Ok(axum::Json(available));
            }
            _ => continue,
        }
    }
    Err(axum::http::StatusCode::NOT_FOUND)
}

pub(crate) fn multi_room_router(
    count: usize,
    router_factory: impl Fn(state::SharedState) -> ApiRouter,
) -> (ApiRouter, Vec<state::SharedState>) {
    let mut router = ApiRouter::new();
    let mut states = vec![];

    for i in 0..count {
        let state = state::SharedState::default();
        game::spawn_game_worker(state.clone());

        let path = room_url(i);
        router = router.nest_api_service(&path, router_factory(state.clone()));
        states.push(state);
    }

    (router, states)
}

fn room_url(room_index: usize) -> String {
    format!("/api/v2/room/{}", room_index + 1)
}

pub mod docs {
    use aide::transform::TransformOperation;

    pub fn available(op: TransformOperation) -> TransformOperation {
        op.description("Get available room.")
    }

    pub fn find(op: TransformOperation) -> TransformOperation {
        op.description("Find room by join code.")
    }
}
