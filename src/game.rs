use crate::{
    cards, models,
    state::{self, BIG_BLIND},
};

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
    let player_stake_in_round = player_stake_in_round(state, player_id);
    let min_raise_to = min_raise_to(state);
    let call = call_amount(state).unwrap_or(0);

    let player = state
        .players
        .get_mut(&player_id)
        .ok_or("Player not found".to_string())?;

    let (new_balance, pot_addition) = match action {
        models::PlayAction::Fold => {
            player.folded = true;
            (player.balance, 0)
        }
        models::PlayAction::Check => {
            let call = call - player_stake_in_round;
            if call > 0 {
                return Err("Cannot check, must call".to_string());
            }
            (player.balance, 0)
        }
        models::PlayAction::Call => {
            let call = call - player_stake_in_round;
            state.round.calls.push((player_id.clone(), call));
            (
                player
                    .balance
                    .checked_sub(call)
                    .ok_or("Not enough balance".to_string())?,
                call,
            )
        }
        models::PlayAction::RaiseTo => {
            if stake < min_raise_to {
                return Err(format!("Raise must be at least {}", min_raise_to));
            }
            state.round.raises.push((player_id.clone(), stake));
            (
                player
                    .balance
                    .checked_sub(stake)
                    .ok_or("Not enough balance".to_string())?,
                stake - player_stake_in_round,
            )
        }
    };

    player.balance = new_balance;
    player.stake += pot_addition;
    state.round.pot += pot_addition;

    next_turn(state, Some(player_id));

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}

pub fn player_stake_in_round(state: &state::State, player_id: &state::PlayerId) -> u64 {
    let max_raise = state
        .round
        .raises
        .iter()
        .filter(|(id, _)| id == player_id)
        .max_by_key(|(_, stake)| *stake)
        .map(|(_, stake)| *stake)
        .unwrap_or(0);

    let sum_of_calls = state
        .round
        .calls
        .iter()
        .filter(|(id, _)| id == player_id)
        .map(|(_, stake)| *stake)
        .sum::<u64>();

    let player_stake_in_current_round = max_raise + sum_of_calls;
    player_stake_in_current_round
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

    state
        .round
        .raises
        .push((small_blind_player.id.clone(), small_blind_stake));

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
    state.round.players_turn = None;
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
    let call_amount = call_amount(state).unwrap_or(0);
    let first_round = state.round.cards_on_table.len() < 3;

    // if call amount > 0, check if all players have reached equal
    // stakes in the current round. If so, end round.
    if call_amount > 0 && (!first_round || first_round && state.round.raises.len() > 2) {
        let all_players_have_called = state
            .players
            .iter()
            .filter(|(_, player)| !player.folded)
            .all(|(_, player)| player_stake_in_round(state, &player.id) == call_amount);

        if all_players_have_called {
            return None;
        }
    }

    // if first round, check if player with big blind has checked on the big blind stake.
    if first_round {
        let is_big_blind_first_round =
            current_player_id == state.players.keys().nth(1).unwrap() && first_round;
        let current_player_stake_is_call_amount =
            player_stake_in_round(state, current_player_id) == BIG_BLIND;
        if is_big_blind_first_round && current_player_stake_is_call_amount {
            return None;
        }
    }

    let next_player = state
        .players
        .iter()
        .enumerate()
        .skip_while(|(_, (id, _))| id != current_player_id)
        .skip(1)
        .filter(|(idx, (_, player))| !player.folded)
        .next()
        .map(|(_, (id, _))| id.clone());

    next_player.or_else(|| {
        state
            .players
            .iter()
            .skip_while(|(_, player)| player.folded)
            .next()
            .filter(|(_, player)| player_stake_in_round(state, &player.id) != call_amount)
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
    let player_stake_in_round = player_stake_in_round(state, player_id);
    let stake = match action {
        models::PlayAction::Check
            if !state.round.raises.is_empty() && player_stake_in_round != last_raise =>
        {
            return Err("Cannot check after a raise".to_string());
        }
        models::PlayAction::RaiseTo if stake == 0 => {
            return Err("Stake cannot be 0 for raise".to_string())
        }
        models::PlayAction::Check => 0,
        models::PlayAction::RaiseTo => {
            let call_amount = call_amount(state).unwrap_or(0);
            let min_raise_to = min_raise_to(state);
            let min_raise = call_amount.max(min_raise_to);
            if stake < min_raise {
                return Err(format!("Raise must be at least {}", min_raise));
            }
            stake
        }
        models::PlayAction::Call => {
            let call = call_amount(state).ok_or("No bets to call".to_string())?;
            if player_stake_in_round >= call {
                return Err("Cannot call, already called".to_string());
            }
            0
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
            state.round.calls.clear();
        }
        3 | 4 => {
            place_cards_on_table(state, 1);
            next_turn(state, None);
            state.round.raises.clear();
            state.round.calls.clear();
        }
        5 => {
            payout_game_winners(state);
            reset_players(state);
            rotate_dealer(state);
            state.status = state::GameStatus::Complete;
            state.round.raises.clear();
            state.round.calls.clear();
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

fn payout_game_winners(state: &mut state::State) {
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
            folded: p.folded,
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

    let mut remaining_players: Vec<_> = state.players.values_mut().filter(|p| !p.folded).collect();
    match remaining_players.as_mut_slice() {
        [only_player_left] => {
            info!(
                "All players but one have folded, paying out pot to {} and completing game",
                only_player_left.id
            );
            only_player_left.balance += state.round.pot;
            state.round.pot = 0;

            reset_players(state);
            rotate_dealer(state);
            state.status = state::GameStatus::Complete;
            state.round.raises.clear();
            state.round.calls.clear();
            return Ok(());
        }
        _ => {}
    }

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

pub(crate) fn min_raise_to(state: &state::State) -> u64 {
    let raises: Vec<_> = [0_u64]
        .into_iter()
        .chain(state.round.raises.iter().map(|(_, s)| *s))
        .collect();

    let max_raise = raises.iter().max().unwrap_or(&0);

    let largest_raise_diff = raises
        .windows(2)
        .map(|w| w[1] - w[0])
        .max()
        .unwrap_or(0)
        .max(BIG_BLIND);

    let min_raise_to = max_raise + largest_raise_diff;
    min_raise_to
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
    use crate::{
        game::tests::fixtures::GameFixture,
        state::{BIG_BLIND, SMALL_BLIND, STARTING_BALANCE},
    };

    #[test]
    fn game_pays_outright_winner_from_pot() {
        use models::PlayAction as P;

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
        accept_player_stake(state, &player_1, SMALL_BLIND, P::Call).expect("R1-P1");
        accept_player_stake(state, &player_2, 0, P::Check).expect("R1-P2");
        assert_eq!(state.round.pot, 100);
        assert_eq!(cards_on_table(state).len(), 3);

        accept_player_stake(state, &player_1, 500, P::RaiseTo).expect("R2-P1");
        assert_eq!(state.round.pot, 600);
        accept_player_stake(state, &player_2, 500, P::Call).expect("R2-P2");
        assert_eq!(state.round.pot, 1100);
        accept_player_stake(state, &player_3, 500, P::Call).expect("R2-P3");
        assert_eq!(state.round.pot, 1600);
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
        let expected_balance = STARTING_BALANCE + BIG_BLIND * 4 + 500 * 2;
        assert_eq!(
            winner_balance_before_payout + pot_before_payout,
            expected_balance
        );
        assert_eq!(winner.balance, expected_balance);
    }

    #[test]
    fn two_player_game_pays_out_to_winner_after_others_fold_in_two_rounds() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);

        assert_eq!(cards_on_table(&state).len(), 0);

        fold_player(&mut state, &player_1).expect("R2-P1");
        assert_eq!(cards_on_table(&state).len(), 0);
        info!(
            "Player 2 stakes: {}",
            state.players.get(&player_2).unwrap().stake
        );
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        let winner = state.players.get(&player_2).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + SMALL_BLIND);

        start_game(&mut state).unwrap();
        assert_eq!(cards_on_table(&state).len(), 0);

        let player_2_data = state.players.get(&player_2).unwrap();
        assert_eq!(player_2_data.stake, SMALL_BLIND);

        fold_player(&mut state, &player_2).expect("R2-P2");

        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.status, state::GameStatus::Complete);

        let winner = state.players.get(&player_1).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE);
    }

    #[test]
    fn two_player_game_fold_on_small_blind() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        fold_player(&mut state, &player_1).expect("R2-P1");
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        let winner = state.players.get(&player_2).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + SMALL_BLIND);
    }

    #[test]
    fn two_player_game_fold_on_big_blind() {
        use models::PlayAction as P;

        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        accept_player_stake(&mut state, &player_1, BIG_BLIND, P::Call).unwrap();
        fold_player(&mut state, &player_2).expect("R2-P2");
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        let winner = state.players.get(&player_1).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + BIG_BLIND);
    }

    #[test]
    fn two_player_game_fold_on_raise() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        fold_player(&mut state, &player_1).expect("R2-P1");
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        let winner = state.players.get(&player_2).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + SMALL_BLIND);
    }

    #[test]
    fn two_player_game_reraising_minimum_works() {
        use models::PlayAction as P;

        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round4);
        assert_eq!(state.round.pot, 40);
        accept_player_stake(&mut state, &player_1, BIG_BLIND, P::RaiseTo).unwrap();
        assert_eq!(state.status, state::GameStatus::Playing);
        assert_eq!(state.round.pot, 60);
        assert_eq!(state.players.get(&player_1).unwrap().stake, BIG_BLIND * 2);
        assert_eq!(state.players.get(&player_2).unwrap().stake, BIG_BLIND);

        accept_player_stake(&mut state, &player_2, BIG_BLIND * 2, P::RaiseTo).unwrap();
        assert_eq!(state.round.pot, 100);
        assert_eq!(state.players.get(&player_1).unwrap().stake, BIG_BLIND * 2);
        assert_eq!(state.players.get(&player_2).unwrap().stake, BIG_BLIND * 3);

        accept_player_stake(&mut state, &player_1, BIG_BLIND * 3, P::RaiseTo).unwrap();
        assert_eq!(state.players.get(&player_1).unwrap().stake, BIG_BLIND * 4);
        assert_eq!(state.players.get(&player_2).unwrap().stake, BIG_BLIND * 3);

        assert_eq!(state.status, state::GameStatus::Playing);

        assert_eq!(state.round.pot, 140);

        accept_player_stake(&mut state, &player_2, BIG_BLIND * 3, P::Call).unwrap();
        assert_eq!(state.status, state::GameStatus::Complete);
    }

    #[test]
    fn three_player_game_fold_on_small_blind() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        fold_player(&mut state, &player_3).expect("R2-P3");
        fold_player(&mut state, &player_1).expect("R2-P1");
        assert_eq!(state.status, state::GameStatus::Complete);

        let winner = state.players.get(&player_2).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + SMALL_BLIND);
    }

    #[test]
    fn three_player_game_check_until_river_then_raise_on_last_player() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        accept_player_stake(&mut state, &player_3, BIG_BLIND, models::PlayAction::Call).unwrap();
        accept_player_stake(&mut state, &player_1, SMALL_BLIND, models::PlayAction::Call).unwrap();
        accept_player_stake(&mut state, &player_2, BIG_BLIND, models::PlayAction::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);
        assert_eq!(state.round.pot, 60);

        accept_player_stake(&mut state, &player_1, 0, models::PlayAction::Check).unwrap();
        accept_player_stake(&mut state, &player_2, 0, models::PlayAction::Check).unwrap();
        accept_player_stake(&mut state, &player_3, 0, models::PlayAction::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 4);
        assert_eq!(state.round.pot, 60);

        accept_player_stake(&mut state, &player_1, 0, models::PlayAction::Check).unwrap();
        accept_player_stake(&mut state, &player_2, 0, models::PlayAction::Check).unwrap();
        accept_player_stake(&mut state, &player_3, 0, models::PlayAction::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 5);
        assert_eq!(state.round.pot, 60);

        accept_player_stake(&mut state, &player_1, 100, models::PlayAction::RaiseTo).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_1), 100);

        accept_player_stake(&mut state, &player_2, 100, models::PlayAction::Call).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_2), 100);

        accept_player_stake(&mut state, &player_3, 200, models::PlayAction::RaiseTo).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_3), 200);
        accept_player_stake(&mut state, &player_1, 100, models::PlayAction::Call).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_1), 200);

        accept_player_stake(&mut state, &player_2, 200, models::PlayAction::Call).unwrap();
        assert_eq!(state.status, state::GameStatus::Complete);
    }

    #[test]
    fn two_player_game_raising_round_one() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);

        assert_eq!(cards_on_table(&state).len(), 0);

        accept_player_stake(
            &mut state,
            &player_1,
            BIG_BLIND * 2,
            models::PlayAction::RaiseTo,
        )
        .unwrap();

        accept_player_stake(
            &mut state,
            &player_2,
            BIG_BLIND * 3,
            models::PlayAction::RaiseTo,
        )
        .unwrap();
        accept_player_stake(&mut state, &player_1, 0, models::PlayAction::Call).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);
    }

    mod fixtures {
        use super::*;

        #[derive(PartialEq)]
        pub enum GameFixture {
            Round1,
            Round2,
            Round3,
            Round4,
        }

        pub fn start_two_player_game(
            game_fixture: GameFixture,
        ) -> (state::State, (state::PlayerId, state::PlayerId)) {
            let mut state = state::State::default();
            state.round.deck = cards::Deck::ordered();

            let player_1 = add_new_player(&mut state, "player_1").unwrap();
            let player_2 = add_new_player(&mut state, "player_2").unwrap();

            assert_eq!(state.players.len(), 2);
            assert_eq!(state.status, state::GameStatus::Joining);
            let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();

            assert_eq!(starting_balance, STARTING_BALANCE);
            start_game(&mut state).unwrap();

            if game_fixture == GameFixture::Round1 {
                return (state, (player_1, player_2));
            }

            // assert pot balance on start is 30:
            assert_eq!(state.round.pot, 30);
            accept_player_stake(&mut state, &player_1, SMALL_BLIND, models::PlayAction::Call)
                .unwrap();
            assert_eq!(state.round.pot, 40);
            accept_player_stake(&mut state, &player_2, BIG_BLIND, models::PlayAction::Check)
                .unwrap();
            assert_eq!(state.round.pot, 40);
            assert_eq!(cards_on_table(&state).len(), 3);
            if game_fixture == GameFixture::Round2 {
                return (state, (player_1, player_2));
            }

            accept_player_stake(&mut state, &player_1, 0, models::PlayAction::Check).unwrap();
            accept_player_stake(&mut state, &player_2, 0, models::PlayAction::Check).unwrap();
            assert_eq!(cards_on_table(&state).len(), 4);
            if game_fixture == GameFixture::Round2 {
                return (state, (player_1, player_2));
            }

            accept_player_stake(&mut state, &player_1, 0, models::PlayAction::Check).unwrap();
            accept_player_stake(&mut state, &player_2, 0, models::PlayAction::Check).unwrap();

            assert_eq!(cards_on_table(&state).len(), 5);
            if game_fixture == GameFixture::Round4 {
                return (state, (player_1, player_2));
            }

            unreachable!();
        }

        pub fn start_three_player_game() -> (
            state::State,
            (state::PlayerId, state::PlayerId, state::PlayerId),
        ) {
            let mut state = state::State::default();
            state.round.deck = cards::Deck::ordered();

            let player_1 = add_new_player(&mut state, "player_1").unwrap();
            let player_2 = add_new_player(&mut state, "player_2").unwrap();
            let player_3 = add_new_player(&mut state, "player_3").unwrap();

            assert_eq!(state.players.len(), 3);
            assert_eq!(state.status, state::GameStatus::Joining);
            let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();
            assert_eq!(starting_balance, STARTING_BALANCE);

            start_game(&mut state).unwrap();

            (state, (player_1, player_2, player_3))
        }
    }
}
