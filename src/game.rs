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
    reset_players(state);
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
    if state.status == state::GameStatus::Playing {
        return Err("Game already started".to_string());
    }
    if state.players.len() >= state::MAX_PLAYERS {
        return Err("Room is full".to_string());
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

    let stake = validate_player_stake(state, stake, player_id, &action)?;

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

    if let models::PlayAction::Raise = action {
        state.round.raises.push((player_id.clone(), player.stake));
    }

    next_turn(state, Some(player_id));

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}

fn accept_blinds(
    state: &mut state::State,
    small_blind_player: state::PlayerId,
    big_blind_player: state::PlayerId,
) {
    let small_blind_player = state
        .players
        .get_mut(&small_blind_player)
        .expect("Small blind player not found");
    let small_blind_stake = small_blind_player.balance.min(state::SMALL_BLIND);
    small_blind_player.balance = small_blind_player.balance - small_blind_stake;
    small_blind_player.stake += small_blind_stake;
    state.round.pot += small_blind_stake;

    let big_blind_player = state
        .players
        .get_mut(&big_blind_player)
        .expect("Big blind player not found");

    let big_blind_stake = big_blind_player.balance.min(state::BIG_BLIND);

    big_blind_player.balance = big_blind_player.balance - big_blind_stake;
    big_blind_player.stake += big_blind_stake;
    state.round.pot += big_blind_stake;

    state
        .round
        .raises
        .push((big_blind_player.id.clone(), big_blind_stake));
}

fn reset_players(state: &mut state::State) {
    for player in state.players.values_mut() {
        player.stake = 0;
        player.folded = false;
    }
}

fn next_turn(state: &mut state::State, current_player_id: Option<&state::PlayerId>) {
    let next_player_id = match current_player_id {
        Some(player_id) => get_next_players_turn(&state, player_id),
        None if state.players.len() < 2 => {
            info!("Not enough players, pausing game");
            state.round.players_turn = None;
            return;
        }
        None if state.round.cards_on_table.is_empty() => {
            let mut player_ids = state.players.keys().cloned().cycle();
            let small_blind_player = player_ids.next().unwrap();
            let big_blind_player = player_ids.next().unwrap();
            let next_player_id = player_ids.next();

            info!(
                "Accepting blinds from players {} (sm) and {} (lg)",
                small_blind_player, big_blind_player
            );
            accept_blinds(state, small_blind_player, big_blind_player);

            next_player_id
        }
        None => state.players.keys().cloned().next(),
    };

    match next_player_id
        .as_ref()
        .and_then(|id| state.players.get_mut(id))
    {
        Some(next_player) => {
            let mut expires = state::dt::Instant::default();
            expires.add_seconds(state::PLAYER_TURN_TIMEOUT_SECONDS);
            next_player.ttl = Some(expires);
        }
        None => {
            info!("Round complete, awaiting next round");
        }
    }

    state.round.players_turn = next_player_id;
}

fn get_next_players_turn(
    state: &state::State,
    current_player_id: &state::PlayerId,
) -> Option<state::PlayerId> {
    let target_stake = state
        .round
        .raises
        .last()
        .map(|(_, stake)| *stake)
        .unwrap_or(0);

    let next_player = state
        .players
        .iter()
        .enumerate()
        .skip_while(|(_, (id, _))| id != current_player_id)
        .skip(1)
        .filter(|(idx, (_, player))| {
            !player.folded
                && (state.round.cards_on_table.len() > 0
                    || player.stake < target_stake
                    || *idx == 1)
        })
        .next()
        .map(|(_, (id, _))| id.clone());

    next_player.or_else(|| {
        state
            .players
            .iter()
            .skip_while(|(_, player)| player.folded)
            .next()
            .filter(|(_, player)| player.stake < target_stake)
            .map(|(id, _)| id.clone())
    })
}

fn validate_player_stake(
    state: &mut state::State,
    stake: u64,
    player_id: &state::PlayerId,
    action: &models::PlayAction,
) -> Result<u64, String> {
    let last_raise = state.round.raises.last().map(|(_, s)| *s).unwrap_or(0);
    let player_stake = state.players.get(player_id).map(|p| p.stake).unwrap_or(0);
    let stake = match action {
        models::PlayAction::Check
            if !state.round.raises.is_empty() && player_stake != last_raise =>
        {
            return Err("Cannot check after a raise".to_string());
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
            stake
        }
        models::PlayAction::Call => {
            let call = call_amount(state).ok_or("No bets to call".to_string())?;
            call - player_stake
        }
        models::PlayAction::Fold => unreachable!("Cannot handle fold action here"),
    };
    Ok(stake)
}

fn complete_round(state: &mut state::State) {
    match state.round.cards_on_table.len() {
        0 => {
            place_cards_on_table(state, 3);
            next_turn(state, None);
            state.round.raises.clear();
        }
        3 | 4 => {
            place_cards_on_table(state, 1);
            next_turn(state, None);
            state.round.raises.clear();
        }
        5 => {
            complete_game(state);
            reset_players(state);
            rotate_dealer(state);
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

    #[derive(Clone, PartialEq, PartialOrd)]
    struct PlayerStake {
        id: state::PlayerId,
        stake: u64,
    }

    let mut stakes: Vec<_> = state
        .players
        .values()
        .filter(|p| !p.folded)
        .map(|p| PlayerStake {
            id: p.id.clone(),
            stake: p.stake,
        })
        .collect();
    stakes.sort_by_key(|s| s.stake);

    let mut deduped_stakes = stakes.iter().map(|s| s.stake).collect::<Vec<_>>();
    deduped_stakes.dedup();

    let mut pots = vec![];

    deduped_stakes.insert(0, 0);
    for stake in deduped_stakes.windows(2) {
        let (rel_stake, abs_stake) = (stake[1] - stake[0], stake[1]);

        let winnable_players: Vec<_> = stakes
            .iter()
            .filter(|s| s.stake >= abs_stake)
            .map(|s| s.id.clone())
            .collect();

        let pot = winnable_players.len() as u64 * rel_stake;
        pots.push((pot, winnable_players));
    }

    // TODO: TEST! the stake values players that folded should still be included in the winnable pot
    for (_, player) in state.players.iter().filter(|(_, p)| p.folded) {
        let mut pot = pots
            .iter_mut()
            .skip_while(|(pot, players)| (*pot / players.len() as u64) < player.stake);

        if let Some((pot, _)) = pot.next() {
            println!(
                "Player {} folded, adding {} stake to pot of {}",
                player.id, player.stake, pot
            );
            *pot += player.stake;
        }
    }

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
            "Player {} has score {} (cards {:?})",
            player.id,
            score.strength(),
            score.cards()
        );
    }

    for (pot, players) in &pots {
        let winning_hand = scores
            .iter()
            .filter(|(player, _)| players.contains(&player.id))
            .map(|(_, score)| score.clone())
            .max()
            .unwrap();

        let mut winning_players: Vec<_> = scores
            .iter_mut()
            .filter(|(player, score)| score == &winning_hand && players.contains(&player.id))
            .map(|(player, _)| &mut **player)
            .collect();

        let winners_count = winning_players.len() as u64;
        let payout = pot / winners_count;
        for winner in winning_players.iter_mut() {
            winner.balance += payout;
        }

        let winners = winning_players
            .iter()
            .map(|p| p.id.clone().to_string())
            .collect::<Vec<_>>();

        println!(
            "Paid out pot to winners. Pot: {}, Winner(s): {}",
            pot,
            winners.join(", "),
        );
    }

    let best_hand = scores.iter().map(|(_, score)| score.clone()).max().unwrap();
    info!(
        "Game complete, pot: {} ({} splits) (rank {:?}) cards: {:?}",
        round.pot,
        pots.len() - 1,
        best_hand.strength(),
        best_hand.cards()
    );

    round.pot = 0;
    state.round.players_turn = None;
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
        .max_by_key(|(_, score)| score.clone())?;

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
        winning_hand: winning_hand.strength().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_pays_outright_winner_from_pot() {
        use models::PlayAction as P;
        use state::BIG_BLIND;

        let mut state = state::State::default();
        let state = &mut state;
        state.round.deck = cards::Deck::ordered();

        let player_1 = add_new_player(state, "player_1").unwrap();
        let player_2 = add_new_player(state, "player_2").unwrap();
        let player_3 = add_new_player(state, "player_3").unwrap();
        let player_4 = add_new_player(state, "player_4").unwrap();
        let player_5 = add_new_player(state, "player_5").unwrap();

        assert_eq!(state.players.len(), 5);
        assert_eq!(state.status, state::GameStatus::Joining);
        let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();
        assert_eq!(starting_balance, state::STARTING_BALANCE);

        start_game(state).unwrap();

        assert_eq!(cards_on_table(state).len(), 0);

        accept_player_stake(state, &player_3, BIG_BLIND, P::Call).expect("R1-P3");
        accept_player_stake(state, &player_4, BIG_BLIND, P::Call).expect("R1-P4");
        accept_player_stake(state, &player_5, BIG_BLIND, P::Call).expect("R1-P5");
        accept_player_stake(state, &player_1, BIG_BLIND, P::Call).expect("R1-P1");
        accept_player_stake(state, &player_2, 0, P::Check).expect("R1-P2");

        assert_eq!(cards_on_table(state).len(), 3);

        accept_player_stake(state, &player_1, 500, P::Raise).expect("R2-P1");
        accept_player_stake(state, &player_2, 0, P::Call).expect("R2-P2");
        accept_player_stake(state, &player_3, 0, P::Call).expect("R2-P3");
        fold_player(state, &player_4).expect("R2-P4");
        fold_player(state, &player_5).expect("R2-P4");

        assert_eq!(cards_on_table(state).len(), 4);

        accept_player_stake(state, &player_1, 0, P::Check).unwrap();
        accept_player_stake(state, &player_2, 0, P::Check).unwrap();
        accept_player_stake(state, &player_3, 0, P::Check).unwrap();

        let pot_before_payout = state.round.pot;
        let winner_balance_before_payout = state.players.get(&player_1).unwrap().balance;

        assert_eq!(pot_before_payout, (BIG_BLIND * 5) + (500 * 3));
        assert_eq!(cards_on_table(state).len(), 5);

        accept_player_stake(state, &player_1, 0, P::Check).unwrap();
        accept_player_stake(state, &player_2, 0, P::Check).unwrap();
        accept_player_stake(state, &player_3, 0, P::Check).unwrap();

        assert_eq!(cards_on_table(state).len(), 5);
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        // wins remaining 4 players blinds and remaining 2 players 500 bets
        let winner = state.players.get(&player_1).unwrap();
        let expected_balance = state::STARTING_BALANCE + BIG_BLIND * 4 + 500 * 2;
        assert_eq!(
            winner_balance_before_payout + pot_before_payout,
            expected_balance
        );
        assert_eq!(winner.balance, expected_balance);
    }
}
