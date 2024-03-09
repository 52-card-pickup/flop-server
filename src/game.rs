use crate::{cards, models, state, utils::get_next_players_turn};

use tracing::info;

pub(crate) fn start_game(state: &mut state::State) -> Result<(), String> {
    if state.status == state::GameStatus::Playing {
        return Err("Game already started".to_string());
    }
    if state.players.len() < 2 {
        return Err("Not enough players".to_string());
    }

    state.cards_on_table.clear();
    state.pot = 0;
    state.players_turn = state.players.keys().next().cloned();
    if state.status == state::GameStatus::Complete {
        state.deck = cards::Deck::default();
    }

    state.status = state::GameStatus::Playing;

    Ok(())
}

pub(crate) fn add_new_player(
    state: &mut state::State,
    player_name: &str,
) -> Result<state::PlayerId, String> {
    if state.status != state::GameStatus::Joining {
        return Err("Game already started".to_string());
    }
    let player_id = state::PlayerId::default();
    let player = state::Player {
        name: player_name.to_owned(),
        id: player_id.clone(),
        balance: 1000,
        stake: 0,
        cards: (state.deck.pop().unwrap(), state.deck.pop().unwrap()),
    };
    state.players.insert(player_id.clone(), player);
    Ok(player_id)
}

pub(crate) fn accept_player_stake(
    state: &mut state::State,
    player_id: &state::PlayerId,
    stake: u64,
) -> Result<(), String> {
    if state.players_turn.as_ref() != Some(player_id) {
        return Err("Not your turn".to_string());
    }
    let player = state
        .players
        .get_mut(&player_id)
        .ok_or("Player not found".to_string())?;

    player.stake += stake;
    player.balance -= stake;
    state.pot += stake;
    state.players_turn = get_next_players_turn(&state.players, &player_id);

    if state.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
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

    for (player, score) in &scores {
        info!(
            "Player {} has score {} (rank {:?})",
            player.id, score.1, score.0
        );
    }

    let winning_hand = scores.iter_mut().max_by_key(|(_, score)| *score).unwrap();
    let (winner, score) = winning_hand;
    winner.balance += state.pot;
    state.pot = 0;
    info!(
        "Game complete, winner: {}, score: {} (rank {:?})",
        winner.id, score.1, score.0
    );
}

pub(crate) fn cards_on_table(state: &state::State) -> Vec<(cards::CardSuite, cards::CardValue)> {
    let cards = state
        .cards_on_table
        .iter()
        .map(|c| (c.suite.clone(), c.value.clone()))
        .collect();
    cards
}

pub(crate) fn cards_in_hand(
    state: &state::State,
    player_id: &state::PlayerId,
) -> (
    (cards::CardSuite, cards::CardValue),
    (cards::CardSuite, cards::CardValue),
) {
    let player = state.players.get(player_id).unwrap();
    let cards = player.cards.clone();
    let cards = (
        (cards.0.suite.clone(), cards.0.value.clone()),
        (cards.1.suite.clone(), cards.1.value.clone()),
    );
    cards
}

pub(crate) fn game_phase(state: &state::State) -> models::GamePhase {
    match state.status {
        state::GameStatus::Joining => models::GamePhase::Waiting,
        state::GameStatus::Playing => models::GamePhase::Playing,
        state::GameStatus::Complete => models::GamePhase::Complete,
    }
}

pub(crate) fn room_players(state: &state::State) -> Vec<models::GameClientPlayer> {
    let players = state
        .players
        .iter()
        .map(|(_, p)| models::GameClientPlayer {
            name: p.name.clone(),
            balance: p.balance,
        })
        .collect();
    players
}
