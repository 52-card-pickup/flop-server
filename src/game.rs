use crate::{cards, models, state};

use tracing::info;

pub(crate) fn spawn_game_worker(state: state::SharedState) {
    fn run_tasks(state: &state::SharedState) {
        let now = state::dt::Instant::default();

        let current_player = {
            let state = state.read().unwrap();
            if state.status != state::GameStatus::Playing {
                return;
            }
            let players_turn = state.round.players_turn.clone();
            players_turn.and_then(|id| state.players.get(&id)).cloned()
        };

        if let Some(player) = current_player {
            let expired = player.ttl.map(|ttl| ttl < now).unwrap_or(false);
            if expired {
                info!("Player {} turn expired", player.id);
                let mut state = state.write().unwrap();

                fold_player(&mut state, &player.id).unwrap();

                // TODO: notify player, soft kick
                state.players.remove(&player.id);
                state.last_update.set_now();
            }
        }
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            run_tasks(&state);
        }
    });
}

pub(crate) fn start_game(state: &mut state::State) -> Result<(), String> {
    if state.status == state::GameStatus::Playing {
        return Err("Game already started".to_string());
    }
    if state.players.len() < 2 {
        return Err("Not enough players".to_string());
    }

    state.round.cards_on_table.clear();
    state.round.pot = 0;
    next_turn(state, None);
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
        balance: state::STARTING_BALANCE,
        stake: 0,
        folded: false,
        ttl: None,
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
    next_turn(state, Some(player_id));

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}

fn reset_players(state: &mut state::State) {
    for player in state.players.values_mut() {
        player.stake = 0;
        player.folded = false;
    }
}

fn next_turn(state: &mut state::State, current_player_id: Option<&state::PlayerId>) {
    let next_player_id = if let Some(player_id) = current_player_id {
        get_next_players_turn(&state, player_id)
    } else {
        reset_players(state);
        state.players.keys().next().cloned()
    };
    if let Some(next_player) = next_player_id
        .as_ref()
        .and_then(|id| state.players.get_mut(id))
    {
        let mut expires = state::dt::Instant::default();
        expires.add_seconds(state::PLAYER_TURN_TIMEOUT_SECONDS);
        next_player.ttl = Some(expires);
    }
    state.round.players_turn = next_player_id;
}

fn get_next_players_turn(
    state: &state::State,
    current_player_id: &state::PlayerId,
) -> Option<state::PlayerId> {
    let next_player = state
        .players
        .iter()
        .skip_while(|(id, _)| id != &current_player_id)
        .skip(1)
        .filter(|(_, player)| !player.folded)
        .next()
        .map(|(id, _)| id.clone());

    let target_stake = state
        .round
        .raises
        .last()
        .map(|(_, stake)| *stake)
        .unwrap_or(0);

    next_player.or_else(|| {
        state
            .players
            .iter()
            .skip_while(|(_, player)| !player.folded)
            .next()
            .filter(|(_, player)| player.stake < target_stake)
            .map(|(id, _)| id.clone())
    })
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
        models::PlayAction::Check => 0,
        models::PlayAction::Raise => {
            let call_amount = call_amount(state).unwrap_or(0);
            let min_raise_by = min_raise_by(state);
            let min_raise = call_amount + min_raise_by;
            if stake < min_raise {
                return Err(format!("Raise must be at least {}", min_raise));
            }
            state.round.raises.push((player_id.clone(), stake));
            stake
        }
        models::PlayAction::Call => call_amount(state).ok_or("No bets to call".to_string())?,
        models::PlayAction::Fold => unreachable!("Cannot handle fold action here"),
    };
    Ok(stake)
}

fn complete_round(state: &mut state::State) {
    match state.round.cards_on_table.len() {
        0 => {
            place_cards_on_table(state, 3);
            rotate_dealer(state);
            next_turn(state, None);
            state.round.raises.clear();
        }
        3 | 4 => {
            place_cards_on_table(state, 1);
            rotate_dealer(state);
            next_turn(state, None);
            state.round.raises.clear();
        }
        5 => {
            rotate_dealer(state);
            complete_game(state);
            state.round.raises.clear();
        }
        _ => unreachable!(),
    }
}

fn place_cards_on_table(state: &mut state::State, count: usize) {
    for _ in 0..count {
        let next_card = state.round.deck.pop().unwrap();
        state.round.cards_on_table.push(next_card);
    }
}

fn rotate_dealer(state: &mut state::State) {
    if let Some(old_dealer) = state.players.pop_first() {
        state.players.insert(old_dealer.0, old_dealer.1);
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
    state.round.players_turn = None;
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

pub(crate) fn completed_game(state: &state::State) -> Option<models::CompletedGame> {
    if state.status != state::GameStatus::Complete {
        return None;
    }
    let (winner, winning_hand) = state
        .players
        .values()
        .map(|p| {
            (
                p,
                cards::Card::evaluate_hand(&p.cards, &state.round.cards_on_table),
            )
        })
        .max_by_key(|(_, score)| *score)?;

    let winner_idx = state
        .players
        .keys()
        .position(|id| id == &winner.id)
        .unwrap();

    let player_cards = state
        .players
        .values()
        .map(|p| {
            (
                (p.cards.0.suite.clone(), p.cards.0.value.clone()),
                (p.cards.1.suite.clone(), p.cards.1.value.clone()),
            )
        })
        .collect();

    Some(models::CompletedGame {
        winner_idx,
        winning_hand: winning_hand.0.to_string(),
        player_cards,
    })
}

pub(crate) fn room_players(state: &state::State) -> Vec<models::GameClientPlayer> {
    let players = state
        .players
        .iter()
        .map(|(_, p)| models::GameClientPlayer {
            name: p.name.clone(),
            balance: p.balance,
            turn_expires_dt: p.ttl.map(|dt| dt.into()),
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
    next_turn(state, Some(player_id));

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}

pub(crate) fn reset_ttl(state: &mut state::State, id: &state::PlayerId) -> Result<(), String> {
    let now = state::dt::Instant::default();
    match state.players.get_mut(id) {
        Some(player) => match player.ttl {
            Some(ttl) if ttl < now => Err("Player's turn has expired".to_string()),
            _ => {
                player.ttl = None;
                Ok(())
            }
        },
        None => Err("Player not found".to_string()),
    }
}

pub(crate) fn call_amount(state: &state::State) -> Option<u64> {
    state.round.raises.last().map(|(_, last_stake)| *last_stake)
}

pub(crate) fn min_raise_by(state: &state::State) -> u64 {
    let raises: Vec<_> = [0_u64]
        .into_iter()
        .chain(state.round.raises.iter().map(|(_, s)| *s))
        .collect();

    let min_raise = raises
        .windows(2)
        .map(|w| w[1] - w[0])
        .last()
        .unwrap_or(state::BIG_BLIND);

    min_raise
}

pub(crate) fn turn_expires_dt(state: &state::State, player_id: &state::PlayerId) -> Option<u64> {
    state
        .players
        .get(player_id)
        .and_then(|p| p.ttl.map(|dt| dt.into()))
}
