use crate::{cards, models, state, utils::get_next_players_turn};

use tracing::info;

pub(crate) fn start_game(state: &mut state::State) -> Result<(), String> {
    if state.status == state::GameStatus::Playing {
        return Err("Game already started".to_string());
    }
    if state.players.len() < 2 {
        return Err("Not enough players".to_string());
    }

    state.round.cards_on_table.clear();
    state.round.pot = 0;
    state.round.players_turn = state.players.keys().next().cloned();
    if state.status == state::GameStatus::Complete {
        state.round.deck = cards::Deck::default();
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
    let card_1 = state.round.deck.pop().unwrap();
    let card_2 = state.round.deck.pop().unwrap();
    let player = state::Player {
        name: player_name.to_owned(),
        id: player_id.clone(),
        balance: 1000,
        stake: 0,
        folded: false,
        cards: (card_1, card_2),
    };
    state.players.insert(player_id.clone(), player);
    Ok(player_id)
}

pub(crate) fn accept_player_stake(
    state: &mut state::State,
    player_id: &state::PlayerId,
    stake: u64,
    action: models::PlayAction,
) -> Result<(), String> {
    if state.round.players_turn.as_ref() != Some(player_id) {
        return Err("Not your turn".to_string());
    }

    let stake = validate_player_stake(state, stake, player_id, action)?;

    let player = state
        .players
        .get_mut(&player_id)
        .ok_or("Player not found".to_string())?;

    let new_balance = player
        .balance
        .checked_sub(stake)
        .ok_or("Not enough balance".to_string())?;

    player.balance = new_balance;
    player.stake += stake;
    state.round.pot += stake;
    state.round.players_turn = get_next_players_turn(&state.players, &player_id);

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}

fn validate_player_stake(
    state: &mut state::State,
    stake: u64,
    player_id: &state::PlayerId,
    action: models::PlayAction,
) -> Result<u64, String> {
    let stake = match action {
        models::PlayAction::Check if !state.round.raises.is_empty() => {
            return Err("Cannot check after a raise".to_string())
        }
        models::PlayAction::Raise if stake == 0 => {
            return Err("Stake cannot be 0 for raise".to_string())
        }
        models::PlayAction::Fold => unreachable!("Cannot handle fold action here"),
        models::PlayAction::Check => 0,
        models::PlayAction::Call => state
            .round
            .raises
            .last()
            .map(|(_, last_stake)| *last_stake)
            .ok_or("No raises to call".to_string())?,
        models::PlayAction::Raise => {
            let raises: Vec<_> = [0_u64]
                .into_iter()
                .chain(state.round.raises.iter().map(|(_, s)| *s))
                .collect();

            let min_raise = raises.windows(2).map(|w| w[1] - w[0]).last().unwrap_or(0);
            if stake < min_raise {
                return Err(format!("Raise must be at least {}", min_raise));
            }
            state.round.raises.push((player_id.clone(), stake));
            stake
        }
    };
    Ok(stake)
}

fn complete_round(state: &mut state::State) {
    match state.round.cards_on_table.len() {
        0 => {
            for _ in 0..3 {
                let next_card = state.round.deck.pop().unwrap();
                state.round.cards_on_table.push(next_card);
            }
            state.round.players_turn = state.players.keys().next().cloned();
            state.round.raises.clear();
        }
        3 | 4 => {
            let next_card = state.round.deck.pop().unwrap();
            state.round.cards_on_table.push(next_card);
            state.round.players_turn = state.players.keys().next().cloned();
            state.round.raises.clear();
        }
        5 => {
            complete_game(state);
            state.round.players_turn = None;
            state.round.raises.clear();
        }
        _ => unreachable!(),
    }
}

fn complete_game(state: &mut state::State) {
    state.status = state::GameStatus::Complete;
    let round = &mut state.round;
    let mut scores: Vec<_> = state
        .players
        .values_mut()
        .map(|p| {
            let score = cards::Card::evaluate_hand(&p.cards, &round.cards_on_table);
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
    winner.balance += round.pot;
    round.pot = 0;
    info!(
        "Game complete, winner: {}, score: {} (rank {:?})",
        winner.id, score.1, score.0
    );
}

pub(crate) fn cards_on_table(state: &state::State) -> Vec<(cards::CardSuite, cards::CardValue)> {
    let cards = state
        .round
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

pub(crate) fn fold_player(
    state: &mut state::State,
    player_id: &state::PlayerId,
) -> Result<(), String> {
    if state.round.players_turn.as_ref() != Some(player_id) {
        return Err("Not your turn".to_string());
    }
    let player = state
        .players
        .get_mut(&player_id)
        .ok_or("Player not found".to_string())?;

    player.folded = true;
    state.round.players_turn = get_next_players_turn(&state.players, &player_id);

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}
