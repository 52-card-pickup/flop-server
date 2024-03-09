use std::collections::{BTreeMap, BTreeSet};

use crate::{
    cards, models,
    state::{self, SharedState},
    utils::{get_next_players_turn, now},
};

use axum::{extract::Path, Extension, Json};
use tracing::info;

pub(crate) async fn room(
    Extension(state): Extension<SharedState>,
) -> Json<models::GameClientState> {
    let state = state.read().unwrap().clone();
    let players = state
        .players
        .iter()
        .map(|(_, p)| models::GameClientPlayer {
            name: p.name.clone(),
            balance: p.balance,
        })
        .collect();

    let cards = state
        .cards_on_table
        .iter()
        .map(|c| (c.suite.clone(), c.value.clone()))
        .collect();

    Json(models::GameClientState {
        state: models::GamePhase::Waiting,
        players,
        pot: state.pot,
        cards,
        last_update: state.last_update,
    })
}

pub(crate) async fn reset(Extension(state): Extension<SharedState>) -> Json<()> {
    let mut state = state.write().unwrap();
    *state = state::State::default();
    state.last_update = now();

    info!("Game reset");
    Json(())
}

pub(crate) async fn player(
    Extension(state): Extension<SharedState>,
    Path(player_id): Path<String>,
) -> Json<models::GamePlayerState> {
    let state = state.read().unwrap();
    let player = state.players.get(&player_id).unwrap();
    let cards = player.cards.clone();
    let your_turn = state.players_turn == Some(player_id);
    Json(models::GamePlayerState {
        state: models::GamePhase::Playing,
        balance: player.balance,
        cards: (
            (cards.0.suite.clone(), cards.0.value.clone()),
            (cards.1.suite.clone(), cards.1.value.clone()),
        ),
        your_turn,
        last_update: state.last_update,
    })
}

pub(crate) async fn play(
    Extension(state): Extension<SharedState>,
    Json(payload): Json<models::PlayRequest>,
) -> Json<bool> {
    if payload.stake <= 0 {
        return Json(false);
    }
    let mut state = state.write().unwrap();
    if state.players_turn.as_ref() != Some(&payload.player_id) {
        return Json(false);
    }

    let player = state.players.get_mut(&payload.player_id).unwrap();
    player.stake += payload.stake;
    player.balance -= payload.stake;
    state.pot += payload.stake;
    state.players_turn = get_next_players_turn(&state.players, &payload.player_id);
    state.last_update = now();

    if state.players_turn.is_none() {
        complete_round(&mut state);
    }
    Json(true)
}

fn complete_round(state: &mut state::State) {
    match state.cards_on_table.len() {
        0 => {
            for _ in 0..3 {
                let next_card = state.deck.pop().unwrap();
                state.cards_on_table.push(next_card);
            }
            state.players_turn = state.players.keys().next().cloned();
        }
        3 | 4 => {
            let next_card = state.deck.pop().unwrap();
            state.cards_on_table.push(next_card);
            state.players_turn = state.players.keys().next().cloned();
        }
        5 => {
            complete_game(state);
            state.players_turn = None;
        }
        _ => unreachable!(),
    }
}

fn complete_game(state: &mut state::State) {
    state.status = state::GameStatus::Complete;
    let mut scores: Vec<_> = state
        .players
        .values_mut()
        .map(|p| {
            let score = cards::Card::evaluate_hand(&p.cards, &state.cards_on_table);
            (p, score)
        })
        .collect();
    let winning_hand = scores.iter_mut().max_by_key(|(_, score)| *score).unwrap();
    let (winner, score) = winning_hand;
    winner.balance += state.pot;
    state.pot = 0;
    info!(
        "Game complete, winner: {}, score: {} (rank {:?})",
        winner.name, score.1, score.0
    );
}

pub(crate) async fn join(
    Extension(state): Extension<SharedState>,
    Json(payload): Json<models::JoinRequest>,
) -> Json<models::JoinResponse> {
    let mut state = state.write().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let player = state::Player {
        name: payload.name.clone(),
        id: id.clone(),
        balance: 1000,
        stake: 0,
        cards: (state.deck.pop().unwrap(), state.deck.pop().unwrap()),
    };
    state.last_update = now();
    state.players.insert(id.clone(), player);
    Json(models::JoinResponse { id })
}
