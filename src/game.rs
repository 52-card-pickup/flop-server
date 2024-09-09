use std::{
    collections::{hash_map, HashMap},
    hash::Hash,
};

use crate::{
    cards, models,
    state::{self, TickerEvent},
};

use tracing::info;

pub(crate) fn spawn_game_worker(state: state::SharedState) {
    async fn run_tasks(state: &state::RoomState) {
        let now = state::dt::Instant::default();

        let (last_update, current_player, status, ticker_expired) = {
            let state = state.read().await;
            let last_update = state.last_update.as_u64();
            let players_turn = state.round.players_turn.clone();
            let current_player = players_turn.and_then(|id| state.players.get(&id)).cloned();
            let ticker_expired = state.ticker.has_expired_items(now);

            (last_update, current_player, state.status, ticker_expired)
        };

        let now_ms: u64 = now.into();
        let idle_ms = match status {
            state::GameStatus::Joining => Some(state::GAME_IDLE_TIMEOUT_SECONDS * 1000),
            state::GameStatus::Complete => Some(state::GAME_IDLE_TIMEOUT_SECONDS * 1000 * 4),
            state::GameStatus::Playing => None,
        };

        if idle_ms.map_or(false, |idle_ms| now_ms - last_update > idle_ms) {
            if let Ok("true") = std::env::var("KILL_ON_IDLE").as_deref() {
                info!("KILL_ON_IDLE is set, exiting...");
                // TODO: graceful shutdown
                std::process::exit(0);
            }

            let mut state = state.write().await;
            if !state.round.deck.is_fresh() || state.status == state::GameStatus::Complete {
                info!("Game idle timeout, resetting game");
                *state = state::State::default();
                state.last_update.set_now();
            }
        };

        if let Some(player) = current_player {
            let expired = player.ttl.map(|ttl| ttl < now).unwrap_or(false);
            if expired {
                info!("Player {} turn expired", player.id);
                let mut state = state.write().await;

                _ = fold_player(&mut state, &player.id).map_err(|e| {
                    info!(
                        "Player {} turn expired, but could not fold: {}",
                        player.id, e
                    )
                });

                // TODO: notify player, soft kick
                if let Some(player) = state.players.remove(&player.id) {
                    info!("Player {} removed from game", player.id);
                    state
                        .ticker
                        .emit(TickerEvent::PlayerTurnTimeout(player.name));
                }
                if state.players.len() < 2 {
                    info!("Not enough players, pausing game until more players join");
                    state.status = state::GameStatus::Joining;
                    state.round = state::Round::default();
                    for player in state.players.values_mut() {
                        player.ttl = None;
                    }
                }
                state.last_update.set_now();
            }
        }

        if ticker_expired {
            let mut state = state.write().await;
            state.ticker.clear_expired_items(now);
        }
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            for state in state.iter().await {
                run_tasks(&state).await;
            }
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
    state.round.completed = None;
    reset_players(state);
    next_turn(state, None);
    if state.status == state::GameStatus::Complete {
        state.round.deck = cards::Deck::default();
        for player in state.players.values_mut() {
            let card_1 = state.round.deck.pop();
            let card_2 = state.round.deck.pop();
            player.cards = (card_1, card_2);
        }
    }

    state.status = state::GameStatus::Playing;
    state.ticker.emit(TickerEvent::GameStarted);

    Ok(())
}

pub(crate) fn add_new_player(
    state: &mut state::State,
    player_name: &str,
    player_id: state::PlayerId,
) -> Result<state::PlayerId, String> {
    if state.status == state::GameStatus::Playing {
        return Err("Game already started".to_string());
    }
    if state.players.len() >= state::MAX_PLAYERS {
        return Err("Room is full".to_string());
    }
    let name = player_name.replace(char::is_whitespace, " ");

    let funds_token = state::token::Token::default();
    let card_1 = state.round.deck.pop();
    let card_2 = state.round.deck.pop();
    let player = state::Player {
        name: name.trim().to_owned(),
        id: player_id.clone(),
        funds_token,
        balance: state::STARTING_BALANCE,
        stake: 0,
        folded: false,
        photo: None,
        ttl: None,
        cards: (card_1, card_2),
    };
    state.players.insert(player_id.clone(), player);
    state
        .ticker
        .emit(TickerEvent::PlayerJoined(player_id.clone()));
    Ok(player_id)
}

pub(crate) fn accept_player_bet(
    state: &mut state::State,
    player_id: &state::PlayerId,
    action: state::BetAction,
) -> Result<(), String> {
    if state.status != state::GameStatus::Playing {
        return Err("Game not started".to_string());
    }
    if state.round.players_turn.as_ref() != Some(player_id) {
        return Err("Not your turn".to_string());
    }

    let action = validate_bet_action(state, player_id, &action)?;
    let player_stake_in_round = player_stake_in_round(state, player_id);
    let min_raise_to = min_raise_to(state);
    let call = call_amount(state).unwrap_or(0);

    let player = state
        .players
        .get_mut(&player_id)
        .ok_or("Player not found".to_string())?;

    let (new_balance, pot_addition) = match action {
        state::BetAction::Check => {
            let call = call - player_stake_in_round;
            if call > 0 {
                return Err("Cannot check, must call".to_string());
            }
            (player.balance, 0)
        }
        state::BetAction::Call => {
            let call = call - player_stake_in_round;
            let call = call.min(player.balance);
            state.round.calls.push((player_id.clone(), call));
            let new_balance = player
                .balance
                .checked_sub(call)
                .expect("Not enough balance to cover call amount");
            (new_balance, call)
        }
        state::BetAction::RaiseTo(raise_to) => {
            if raise_to < min_raise_to {
                return Err(format!("Raise must be at least {}", min_raise_to));
            }
            state.round.raises.push((player_id.clone(), raise_to));
            let pot_addition = raise_to - player_stake_in_round;
            let new_balance = player
                .balance
                .checked_sub(pot_addition)
                .ok_or("Not enough balance".to_string())?;
            (new_balance, pot_addition)
        }
    };

    player.balance = new_balance;
    player.stake += pot_addition;
    state.round.pot += pot_addition;

    state
        .ticker
        .emit(TickerEvent::PlayerBet(player_id.clone(), action));

    next_turn(state, Some(player_id));

    if state.round.players_turn.is_none() {
        complete_round(state);
    }

    Ok(())
}

pub fn player_stake_in_round(state: &state::State, player_id: &state::PlayerId) -> u64 {
    // check if player was last to raise, if so, return raise amount
    if let Some((id, stake)) = state.round.raises.last() {
        if id == player_id {
            return *stake;
        }
    }

    let max_raise = state
        .round
        .raises
        .iter()
        .filter(|(id, _)| id == player_id)
        .map(|(_, stake)| *stake)
        .max()
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
        .ticker
        .emit(TickerEvent::SmallBlindPosted(small_blind_player.id.clone()));

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

    state
        .ticker
        .emit(TickerEvent::BigBlindPosted(big_blind_player.id.clone()));
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
            let mut player_ids = state
                .players
                .iter()
                .filter(|(_, p)| !p.folded && p.balance > 0)
                .map(|(id, _)| id.clone())
                .cycle();
            let small_blind_player = player_ids.next().expect("No players left");
            let big_blind_player = player_ids.next().expect("No players left");
            let next_player_id = player_ids.next();

            info!(
                "Accepting blinds from players {} (sm) and {} (lg)",
                small_blind_player, big_blind_player
            );
            accept_blinds(state, small_blind_player, big_blind_player);

            next_player_id
        }
        None => get_rounds_starting_player(state),
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

fn get_rounds_starting_player(state: &mut state::State) -> Option<state::PlayerId> {
    let players_in_round = &mut state
        .players
        .iter()
        .filter(|(_, p)| !p.folded && p.balance > 0);

    let starting_player = players_in_round.next();

    // if no other players left, the game is complete
    let next_playable_player = players_in_round.next();
    if let None = next_playable_player {
        return None;
    }

    starting_player.map(|(id, _)| id.clone())
}

fn get_next_players_turn(
    state: &state::State,
    current_player_id: &state::PlayerId,
) -> Option<state::PlayerId> {
    let call_amount = call_amount(state).unwrap_or(0);
    let first_round = state.round.cards_on_table.len() < 3;

    // if call amount > 0, check if all players have reached equal
    // stakes in the current round. If so, end round.
    if call_amount > 0 && (!first_round || state.round.raises.len() > 2) {
        let all_players_have_called = state
            .players
            .iter()
            .filter(|(_, player)| !player.folded && player.balance > 0)
            .all(|(_, player)| player_stake_in_round(state, &player.id) == call_amount);

        if all_players_have_called {
            return None;
        }
    }

    // if first round, check if player with big blind has checked on the big blind stake.
    if first_round {
        let is_big_blind_first_round =
            current_player_id == state.players.keys().nth(1).expect("No players left");
        let current_player_stake_is_call_amount =
            player_stake_in_round(state, current_player_id) == state::BIG_BLIND;
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
        .filter(|(_, (_, player))| !player.folded && player.balance > 0)
        .next()
        .map(|(_, (id, _))| id.clone());

    next_player.or_else(|| {
        state
            .players
            .iter()
            .filter(|(_, player)| !player.folded && player.balance > 0)
            .next()
            .filter(|(_, player)| player_stake_in_round(state, &player.id) != call_amount)
            .map(|(id, _)| id.clone())
    })
}

fn validate_bet_action(
    state: &state::State,
    player_id: &state::PlayerId,
    action: &state::BetAction,
) -> Result<state::BetAction, String> {
    let last_raise = state.round.raises.last().map(|(_, s)| *s).unwrap_or(0);
    let player_stake_in_round = player_stake_in_round(state, player_id);
    let stake = match action {
        state::BetAction::Check
            if !state.round.raises.is_empty() && player_stake_in_round != last_raise =>
        {
            return Err("Cannot check after a raise".to_string());
        }
        state::BetAction::RaiseTo(raise_to) if *raise_to == 0 => {
            return Err("Stake cannot be 0 for raise".to_string())
        }
        state::BetAction::Check => state::BetAction::Check,
        state::BetAction::RaiseTo(raise_to) => {
            let call_amount = call_amount(state).unwrap_or(0);
            let min_raise_to = min_raise_to(state);
            let min_raise = call_amount.max(min_raise_to);
            if *raise_to < min_raise {
                return Err(format!("Raise must be at least {}", min_raise));
            }
            state::BetAction::RaiseTo(*raise_to)
        }
        state::BetAction::Call => {
            let call = call_amount(state).ok_or("No bets to call".to_string())?;
            if player_stake_in_round >= call {
                return Err("Cannot call, already called".to_string());
            }
            state::BetAction::Call
        }
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
            if state.round.players_turn.is_none() {
                complete_round(state);
            }
        }
        3 | 4 => {
            place_cards_on_table(state, 1);
            next_turn(state, None);
            state.round.raises.clear();
            state.round.calls.clear();
            if state.round.players_turn.is_none() {
                complete_round(state);
            }
        }
        5 => {
            payout_game_winners(state);
            state.round.raises.clear();
            state.round.calls.clear();
            state.status = state::GameStatus::Complete;
            state.ticker.emit(TickerEvent::RoundComplete);

            rotate_dealer(state);
        }
        _ => unreachable!(),
    }
}

fn place_cards_on_table(state: &mut state::State, count: usize) {
    for _ in 0..count {
        let next_card = state.round.deck.pop();
        state.round.cards_on_table.push(next_card);
    }
    state.ticker.emit(TickerEvent::CardsDealtToTable(count));
}

fn rotate_dealer(state: &mut state::State) {
    if let Some(old_dealer) = state.players.pop_first() {
        state.players.insert(old_dealer.0, old_dealer.1);

        let mut player_ids = state.players.keys();
        let dealer = player_ids.next().cloned().expect("No players left");
        state.ticker.emit(TickerEvent::DealerRotated(dealer));
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

    match stakes.len() {
        1 => {
            let winner_stake = stakes.first().unwrap();
            match state.players.get_mut(&winner_stake.id) {
                Some(player) => {
                    player.balance += round.pot;
                    let winner = state::RoundWinner {
                        player_id: winner_stake.id.clone(),
                        hand: None,
                        winnings: round.pot,
                        total_pot_winnings: round.pot,
                    };
                    round.completed = Some(state::CompletedRound {
                        winners: vec![winner],
                        best_hand: None,
                        hide_cards: false,
                    });
                    state
                        .ticker
                        .emit(TickerEvent::PaidPot(winner_stake.id.clone(), round.pot));
                    info!(
                        "Player {} is the only player left, whole pot is won, pot: {}",
                        player.id, round.pot
                    );
                }
                _ => {
                    info!("No players left, pot is lost");
                    round.completed = Some(state::CompletedRound {
                        winners: vec![],
                        best_hand: None,
                        hide_cards: true,
                    });
                    return;
                }
            }
            return;
        }
        0 => {
            info!("No players left, pot is lost");
            round.completed = Some(state::CompletedRound {
                winners: vec![],
                best_hand: None,
                hide_cards: true,
            });
            return;
        }
        _ => {}
    }

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
            info!(
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
    let mut winners = vec![];
    let mut winner_hands = vec![];

    for (pot, pot_players) in &pots {
        let winning_hand = scores
            .iter()
            .filter(|(player, _)| pot_players.contains(&player.id))
            .map(|(_, score)| score.clone())
            .max()
            .expect("No winning hand found for pot");

        let mut winning_players: Vec<_> = scores
            .iter_mut()
            .filter(|(player, score)| !(score < &winning_hand) && pot_players.contains(&player.id))
            .map(|(player, _)| &mut **player)
            .collect();

        let winners_count = winning_players.len() as u64;
        let payout = if winners_count > 0 {
            pot / winners_count
        } else {
            continue;
        }; // TODO: handle odd pot sizes
        match &winning_players[..] {
            [] => unreachable!(),
            [winner] => {
                state.ticker.emit(TickerEvent::Winner(
                    winner.id.clone(),
                    winning_hand.strength(),
                ));
            }
            winners => {
                state.ticker.emit(TickerEvent::SplitPotWinners(
                    winners.iter().map(|p| p.id.clone()).collect(),
                    winning_hand.strength(),
                ));
            }
        }

        for winner in winning_players.iter_mut() {
            winners.push(state::RoundWinner {
                player_id: winner.id.clone(),
                hand: Some(winning_hand.strength()),
                winnings: payout,
                total_pot_winnings: *pot,
            });
            let hand = cards::Card::evaluate_hand(&winner.cards, &round.cards_on_table);
            winner_hands.push((winner.id.clone(), hand));
            winner.balance += payout;
            state
                .ticker
                .emit(TickerEvent::PaidPot(winner.id.clone(), payout));
        }

        let winner_ids: Vec<_> = winning_players.iter().map(|p| p.id.to_string()).collect();

        info!(
            "Paid out pot to winners. Pot: {}, Winner(s): {}",
            pot,
            winner_ids.join(", "),
        );
    }

    let pot_splits = pots.len().saturating_sub(1);
    let (_, best_hand) = winner_hands
        .iter()
        .max_by_key(|(_, score)| score)
        .expect("No winning hands found, but there should be at least one winner");
    let best_hand_players: Vec<_> = winners
        .iter()
        .map(|winner| winner.player_id.clone())
        .collect();

    info!(
        "Game complete, pot: {} ({} splits) (rank {:?}) cards: {:?}",
        round.pot,
        pot_splits,
        best_hand.strength(),
        best_hand.cards()
    );

    round.completed = Some(state::CompletedRound {
        winners,
        best_hand: Some((best_hand_players, best_hand.strength())),
        hide_cards: false,
    });
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
) -> Option<(
    (cards::CardSuite, cards::CardValue),
    (cards::CardSuite, cards::CardValue),
)> {
    let player = state.players.get(player_id)?;
    let cards = player.cards.clone();
    let cards = (
        (cards.0.suite.clone(), cards.0.value.clone()),
        (cards.1.suite.clone(), cards.1.value.clone()),
    );
    Some(cards)
}

pub(crate) fn is_player_turn(state: &state::State, player_id: &state::PlayerId) -> bool {
    state.status == state::GameStatus::Playing
        && state.round.players_turn.as_ref() == Some(&player_id)
}

pub(crate) fn game_phase(state: &state::State) -> models::GamePhase {
    match state.status {
        state::GameStatus::Joining => models::GamePhase::Waiting,
        state::GameStatus::Playing => models::GamePhase::Playing,
        state::GameStatus::Complete => models::GamePhase::Complete,
    }
}

pub(crate) fn ticker(state: &state::State) -> Option<String> {
    fn ticker_header(state: &state::State, now: state::dt::Instant) -> Option<String> {
        match state.ticker.len() {
            0 => None,
            _ => Some(format!(
                "\x00{}\x00{}\x00",
                now.as_u64(),
                state.ticker.len()
            )),
        }
    }
    fn ticker_item(
        state: &state::State,
        item: &state::ticker::TickerItem,
        now: state::dt::Instant,
    ) -> String {
        let start_offset_ms = (item.start.as_u64() as i64) - (now.as_u64() as i64);
        let duration = item.end.as_u64().saturating_sub(item.start.as_u64());
        format!(
            "{}|{}|{}\x00{}",
            item.seq_index,
            start_offset_ms,
            duration,
            item.payload.format(state)
        )
    }
    let now = state::dt::Instant::default();
    let header = ticker_header(state, now)?;
    let items: Vec<_> = state
        .ticker
        .iter()
        .map(|item| ticker_item(state, item, now))
        .collect();
    Some(format!("{}\n{}", header, items.join("\n")))
}

pub(crate) fn completed_game(state: &state::State) -> Option<models::CompletedGame> {
    if state.status != state::GameStatus::Complete {
        return None;
    }

    let completed_round = state.round.completed.as_ref()?;
    let winners = {
        let mut winners: HashMap<state::PlayerId, u64> = HashMap::new();
        for winner in &completed_round.winners {
            let winnings = winners.entry(winner.player_id.clone()).or_default();
            *winnings += winner.winnings;
        }
        winners
    };

    let max_winner = completed_round
        .best_hand
        .as_ref()
        .map(|(players, _)| {
            players
                .iter()
                .map(|id| (id, winners.get(id).unwrap_or(&0)))
                .max_by_key(|(_, winnings)| **winnings)
        })
        .unwrap_or_else(|| winners.iter().max_by_key(|(_, winnings)| **winnings));

    let winner_name = max_winner
        .and_then(|(id, _)| state.players.get(id))
        .map(|p| p.name.clone());
    let winning_hand = completed_round
        .best_hand
        .as_ref()
        .map(|(_, hand)| hand.to_string());

    Some(models::CompletedGame {
        winner_name,
        winning_hand,
        player_cards: state
            .players
            .iter()
            .map(|(_, p)| {
                (!p.folded && !completed_round.hide_cards).then(|| {
                    (
                        (p.cards.0.suite.clone(), p.cards.0.value.clone()),
                        (p.cards.1.suite.clone(), p.cards.1.value.clone()),
                    )
                })
            })
            .collect(),
    })
}

pub(crate) fn room_players(state: &state::State) -> Vec<models::GameClientPlayer> {
    let current_player_id = state.round.players_turn.as_ref();
    let players = state
        .players
        .iter()
        .map(|(_, p)| models::GameClientPlayer {
            name: p.name.clone(),
            balance: p.balance,
            folded: p.folded,
            photo: player_photo_url(p),
            color_hue: player_color_hue(p),
            turn_expires_dt: p.ttl.map(|dt| dt.into()).filter(|_| {
                current_player_id == Some(&p.id) && state.status == state::GameStatus::Playing
            }),
        })
        .collect();
    players
}

fn player_photo_url(p: &state::Player) -> Option<String> {
    let state::PlayerPhoto(_, token) = p.photo.as_ref()?;
    Some(format!("player/photo/{}", token))
}

fn player_color_hue(p: &state::Player) -> u16 {
    let mut hasher = hash_map::DefaultHasher::default();
    p.id.hash(&mut hasher);
    let degrees = std::hash::Hasher::finish(&hasher) % 360;
    degrees as u16
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

    state
        .ticker
        .emit(TickerEvent::PlayerFolded(player_id.clone()));

    let mut remaining_players: Vec<_> = state.players.values_mut().filter(|p| !p.folded).collect();
    match remaining_players.as_mut_slice() {
        [only_player_left] => {
            info!(
                "All players but one have folded, paying out pot to {} and completing game",
                only_player_left.id
            );
            let pot = state.round.pot;
            only_player_left.balance += pot;
            state.round.pot = 0;

            state
                .ticker
                .emit(TickerEvent::PaidPot(only_player_left.id.clone(), pot));

            rotate_dealer(state);
            state.status = state::GameStatus::Complete;
            state.round.raises.clear();
            state.round.calls.clear();
            state.round.completed = Some(state::CompletedRound {
                winners: vec![],
                best_hand: None,
                hide_cards: true,
            });
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

pub(crate) fn transfer_funds(
    state: &mut state::State,
    player_id: &state::PlayerId,
    payload: &models::TransferRequest,
) -> Result<(), ()> {
    let player_balance = state.players.get(&player_id).ok_or(())?.balance;
    let remaining = player_balance.checked_sub(payload.amount).ok_or_else(|| {
        info!(
            "Player {} failed to transfer: insufficient funds",
            player_id
        );
        ()
    })?;
    let destination_id = {
        let destination_id = state
            .players
            .iter()
            .find_map(|(id, p)| {
                if p.funds_token.as_ref() == &payload.to {
                    Some(id.clone())
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                info!(
                    "Player {} failed to transfer: destination not found",
                    player_id
                );
                ()
            })?;

        let destination = state.players.get_mut(&destination_id).ok_or_else(|| {
            info!(
                "Player {} failed to transfer: destination not found (destination_id: {})",
                player_id, destination_id
            );
            ()
        })?;
        destination.balance += payload.amount;
        destination.id.clone()
    };
    {
        let player = state
            .players
            .get_mut(&player_id)
            .expect("Player must exist");
        player.balance = remaining;
    }
    state
        .ticker
        .emit(state::TickerEvent::PlayerTransferredBalance(
            player_id.clone(),
            destination_id,
            payload.amount,
        ));

    Ok(())
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
        .max(state::BIG_BLIND);

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
    use state::BetAction as P;

    #[test]
    fn two_player_game_deals_correct_cards_to_table() {
        let (state, _) = fixtures::start_two_player_game(GameFixture::Round1);
        assert_eq!(cards_on_table(&state).len(), 0);

        let (state, _) = fixtures::start_two_player_game(GameFixture::Round2);
        assert_eq!(cards_on_table(&state).len(), 3);

        let (state, _) = fixtures::start_two_player_game(GameFixture::Round3);
        assert_eq!(cards_on_table(&state).len(), 4);

        let (state, _) = fixtures::start_two_player_game(GameFixture::Round4);
        assert_eq!(cards_on_table(&state).len(), 5);
    }

    #[test]
    fn two_player_game_redeals_players_cards_after_round() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Complete);
        let player_1_cards = cards_in_hand(&state, &player_1).unwrap();
        let player_2_cards = cards_in_hand(&state, &player_2).unwrap();

        start_game(&mut state).unwrap();
        let new_player_1_cards = cards_in_hand(&state, &player_1).unwrap();
        let new_player_2_cards = cards_in_hand(&state, &player_2).unwrap();

        assert_ne!(
            (player_1_cards, player_2_cards),
            (new_player_1_cards, new_player_2_cards)
        );
    }

    #[test]
    fn game_pays_outright_winner_from_pot() {
        let mut state = state::State::default();
        let state = &mut state;
        state.round.deck = cards::Deck::ordered();

        let player_1 = fixtures::add_player(state, "player_1").unwrap();
        let player_2 = fixtures::add_player(state, "player_2").unwrap();
        let player_3 = fixtures::add_player(state, "player_3").unwrap();
        let player_4 = fixtures::add_player(state, "player_4").unwrap();
        let player_5 = fixtures::add_player(state, "player_5").unwrap();

        assert_eq!(state.players.len(), 5);
        assert_eq!(state.status, state::GameStatus::Joining);
        let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();
        assert_eq!(starting_balance, state::STARTING_BALANCE);

        start_game(state).unwrap();

        assert_eq!(cards_on_table(state).len(), 0);

        accept_player_bet(state, &player_3, P::Call).expect("R1-P3");
        accept_player_bet(state, &player_4, P::Call).expect("R1-P4");
        accept_player_bet(state, &player_5, P::Call).expect("R1-P5");
        accept_player_bet(state, &player_1, P::Call).expect("R1-P1");
        accept_player_bet(state, &player_2, P::Check).expect("R1-P2");
        assert_eq!(state.round.pot, 100);
        assert_eq!(cards_on_table(state).len(), 3);

        accept_player_bet(state, &player_1, P::RaiseTo(500)).expect("R2-P1");
        assert_eq!(state.round.pot, 600);
        accept_player_bet(state, &player_2, P::Call).expect("R2-P2");
        assert_eq!(state.round.pot, 1100);
        accept_player_bet(state, &player_3, P::Call).expect("R2-P3");
        assert_eq!(state.round.pot, 1600);
        fold_player(state, &player_4).expect("R2-P4");
        fold_player(state, &player_5).expect("R2-P4");

        assert_eq!(cards_on_table(state).len(), 4);

        accept_player_bet(state, &player_1, P::Check).unwrap();
        accept_player_bet(state, &player_2, P::Check).unwrap();
        accept_player_bet(state, &player_3, P::Check).unwrap();

        let pot_before_payout = state.round.pot;
        let winner_balance_before_payout = state.players.get(&player_1).unwrap().balance;

        assert_eq!(pot_before_payout, (BIG_BLIND * 5) + (500 * 3));
        assert_eq!(cards_on_table(state).len(), 5);

        accept_player_bet(state, &player_1, P::Check).unwrap();
        accept_player_bet(state, &player_2, P::Check).unwrap();
        accept_player_bet(state, &player_3, P::Check).unwrap();

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
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
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
    fn two_player_game_can_transfer_funds() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        let player_1_balance = state.players.get(&player_1).unwrap().balance;
        let player_2_balance = state.players.get(&player_2).unwrap().balance;

        let player_2_token = &state.players.get(&player_2).unwrap().funds_token;

        let transfer_request = models::TransferRequest {
            to: player_2_token.to_string(),
            amount: 100,
        };

        transfer_funds(&mut state, &player_1, &transfer_request).unwrap();

        let player_1_balance_after_transfer = state.players.get(&player_1).unwrap().balance;
        let player_2_balance_after_transfer = state.players.get(&player_2).unwrap().balance;

        assert_eq!(player_1_balance - 100, player_1_balance_after_transfer);
        assert_eq!(player_2_balance + 100, player_2_balance_after_transfer);
    }

    #[test]
    fn two_player_game_reraising_minimum_works() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round4);
        assert_eq!(state.round.pot, 40);
        accept_player_bet(&mut state, &player_1, P::RaiseTo(BIG_BLIND)).unwrap();
        assert_eq!(state.status, state::GameStatus::Playing);
        assert_eq!(state.round.pot, 60);
        assert_eq!(state.players.get(&player_1).unwrap().stake, BIG_BLIND * 2);
        assert_eq!(state.players.get(&player_2).unwrap().stake, BIG_BLIND);

        accept_player_bet(&mut state, &player_2, P::RaiseTo(BIG_BLIND * 2)).unwrap();
        assert_eq!(state.round.pot, 100);
        assert_eq!(state.players.get(&player_1).unwrap().stake, BIG_BLIND * 2);
        assert_eq!(state.players.get(&player_2).unwrap().stake, BIG_BLIND * 3);

        accept_player_bet(&mut state, &player_1, P::RaiseTo(BIG_BLIND * 3)).unwrap();
        assert_eq!(state.players.get(&player_1).unwrap().stake, BIG_BLIND * 4);
        assert_eq!(state.players.get(&player_2).unwrap().stake, BIG_BLIND * 3);

        assert_eq!(state.status, state::GameStatus::Playing);

        assert_eq!(state.round.pot, 140);

        accept_player_bet(&mut state, &player_2, P::Call).unwrap();
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

        let completed = completed_game(&state).unwrap();
        assert_eq!(completed.winning_hand, None);
    }

    #[test]
    fn three_player_game_check_until_river_then_raise_on_last_player() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(state.round.pot, 30);

        accept_player_bet(&mut state, &player_3, P::Call).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);
        assert_eq!(state.round.pot, 60);

        accept_player_bet(&mut state, &player_1, P::Check).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();
        accept_player_bet(&mut state, &player_3, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 4);
        assert_eq!(state.round.pot, 60);

        accept_player_bet(&mut state, &player_1, P::Check).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();
        accept_player_bet(&mut state, &player_3, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 5);
        assert_eq!(state.round.pot, 60);

        accept_player_bet(&mut state, &player_1, P::RaiseTo(100)).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_1), 100);

        accept_player_bet(&mut state, &player_2, P::Call).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_2), 100);

        accept_player_bet(&mut state, &player_3, P::RaiseTo(200)).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_3), 200);
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
        assert_eq!(player_stake_in_round(&state, &player_1), 200);

        accept_player_bet(&mut state, &player_2, P::Call).unwrap();
        assert_eq!(state.status, state::GameStatus::Complete);
    }

    #[test]
    fn two_player_game_ends_in_big_win_next_game_accepts_call_to_all_in() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round4);

        assert_eq!(cards_on_table(&state).len(), 5);

        // game 1, round 4
        accept_player_bet(&mut state, &player_1, P::RaiseTo(500)).unwrap();
        accept_player_bet(&mut state, &player_2, P::Call).unwrap();
        assert_eq!(state.status, state::GameStatus::Complete);

        let player_1_balance = {
            let loser = state.players.get(&player_1).unwrap();
            let winner = state.players.get(&player_2).unwrap();
            assert!(winner.balance > loser.balance);
            loser.balance
        };

        // game 2, round 1
        start_game(&mut state).unwrap();
        accept_player_bet(&mut state, &player_2, P::RaiseTo(player_1_balance)).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
    }

    #[test]
    fn two_player_game_ends_in_big_win_next_game_accepts_call_to_above_all_in() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round4);

        assert_eq!(cards_on_table(&state).len(), 5);

        // game 1, round 4
        accept_player_bet(&mut state, &player_1, P::RaiseTo(500)).unwrap();
        accept_player_bet(&mut state, &player_2, P::Call).unwrap();
        assert_eq!(state.status, state::GameStatus::Complete);

        let player_1_balance = {
            let loser = state.players.get(&player_1).unwrap();
            let winner = state.players.get(&player_2).unwrap();
            assert!(winner.balance > loser.balance);
            loser.balance
        };

        // game 2, round 1
        start_game(&mut state).unwrap();
        fixtures::deal_biased_deck(&mut state, &player_1, &player_2, true);
        assert_eq!(state.status, state::GameStatus::Playing);

        accept_player_bet(&mut state, &player_2, P::RaiseTo(player_1_balance + 100)).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();

        assert_eq!(cards_on_table(&state).len(), 5);
        assert_eq!(state.status, state::GameStatus::Complete);

        let loser = state.players.get(&player_1).unwrap();
        let winner = state.players.get(&player_2).unwrap();
        assert!(winner.balance > loser.balance);
        assert_eq!(loser.balance, 0);
    }

    #[test]
    fn two_player_game_raising_round_one() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);

        assert_eq!(cards_on_table(&state).len(), 0);

        accept_player_bet(&mut state, &player_1, P::RaiseTo(BIG_BLIND * 2)).unwrap();

        accept_player_bet(&mut state, &player_2, P::RaiseTo(BIG_BLIND * 3)).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);
    }

    #[test]
    fn two_player_game_raising_with_intermittent_calls_checking_balances() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);

        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(
            state.players.get(&player_1).unwrap().balance,
            STARTING_BALANCE - SMALL_BLIND
        );
        assert_eq!(
            state.players.get(&player_2).unwrap().balance,
            STARTING_BALANCE - BIG_BLIND
        );

        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
        assert_eq!(
            state.players.get(&player_1).unwrap().balance,
            STARTING_BALANCE - BIG_BLIND
        );
        accept_player_bet(&mut state, &player_2, P::RaiseTo(BIG_BLIND * 2)).unwrap();
        assert_eq!(
            state.players.get(&player_2).unwrap().balance,
            STARTING_BALANCE - 40
        );
    }

    #[test]
    fn three_player_game_folded_players_dont_have_turns_in_further_rounds() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        assert_eq!(cards_on_table(&state).len(), 0);

        accept_player_bet(&mut state, &player_3, P::Call).unwrap();
        fold_player(&mut state, &player_1).expect("R2-P1");
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);

        // ensure player 1 does not take a turn given they have folded
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();
    }

    #[test]
    fn three_player_game_raise_someone_over_all_in_completes() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        let player_1 = state.players.get_mut(&player_1).unwrap();
        player_1.balance = 100;

        let player_1 = player_1.id.clone();

        assert_eq!(cards_on_table(&state).len(), 0);

        accept_player_bet(&mut state, &player_3, P::Call).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);

        accept_player_bet(&mut state, &player_1, P::Check).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();
        accept_player_bet(&mut state, &player_3, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 4);

        accept_player_bet(&mut state, &player_1, P::Check).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();
        accept_player_bet(&mut state, &player_3, P::Check).unwrap();

        assert_eq!(cards_on_table(&state).len(), 5);

        accept_player_bet(&mut state, &player_1, P::Check).unwrap();
        accept_player_bet(&mut state, &player_2, P::Check).unwrap();
        accept_player_bet(&mut state, &player_3, P::RaiseTo(200)).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
        accept_player_bet(&mut state, &player_2, P::Call).unwrap();

        assert_eq!(state.status, state::GameStatus::Complete);
    }

    mod fixtures {
        use super::*;

        #[derive(PartialEq)]
        pub enum GameFixture {
            Round1,
            Round2,
            Round3,
            Round4,
            Complete,
        }

        pub fn start_two_player_game(
            game_fixture: GameFixture,
        ) -> (state::State, (state::PlayerId, state::PlayerId)) {
            let mut state = state::State::default();

            let player_1 = add_player(&mut state, "player_1").unwrap();
            let player_2 = add_player(&mut state, "player_2").unwrap();

            assert_eq!(state.players.len(), 2);
            assert_eq!(state.status, state::GameStatus::Joining);
            let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();

            assert_eq!(starting_balance, STARTING_BALANCE);

            start_game(&mut state).unwrap();
            deal_biased_deck(&mut state, &player_1, &player_2, true);
            progress_two_player_game(&mut state, game_fixture);

            (state, (player_1, player_2))
        }

        pub fn progress_two_player_game(state: &mut state::State, game_fixture: GameFixture) {
            assert!(state.status == state::GameStatus::Playing);
            assert_eq!(cards_on_table(&state).len(), 0);
            if game_fixture == GameFixture::Round1 {
                return;
            }

            let (first_player, second_player) = {
                let active_player = state.round.players_turn.as_ref().unwrap();
                let mut players = state
                    .players
                    .keys()
                    .cycle()
                    .skip_while(|p| *p != active_player)
                    .cloned();
                (players.next().unwrap(), players.next().unwrap())
            };

            // assert pot balance on start is 30:
            assert_eq!(state.round.pot, 30);
            accept_player_bet(state, &first_player, P::Call).unwrap();
            assert_eq!(state.round.pot, 40);
            accept_player_bet(state, &second_player, P::Check).unwrap();
            assert_eq!(state.round.pot, 40);
            assert_eq!(cards_on_table(&state).len(), 3);
            if game_fixture == GameFixture::Round2 {
                return;
            }

            accept_player_bet(state, &first_player, P::Check).unwrap();
            accept_player_bet(state, &second_player, P::Check).unwrap();
            assert_eq!(cards_on_table(&state).len(), 4);
            if game_fixture == GameFixture::Round3 {
                return;
            }

            accept_player_bet(state, &first_player, P::Check).unwrap();
            accept_player_bet(state, &second_player, P::Check).unwrap();

            assert_eq!(cards_on_table(&state).len(), 5);
            if game_fixture == GameFixture::Round4 {
                return;
            }

            accept_player_bet(state, &first_player, P::Check).unwrap();
            accept_player_bet(state, &second_player, P::Check).unwrap();

            assert_eq!(state.status, state::GameStatus::Complete);
            if game_fixture == GameFixture::Complete {
                return;
            }

            unreachable!();
        }

        pub fn start_three_player_game() -> (
            state::State,
            (state::PlayerId, state::PlayerId, state::PlayerId),
        ) {
            let mut state = state::State::default();
            state.round.deck = cards::Deck::ordered();

            let player_1 = add_player(&mut state, "player_1").unwrap();
            let player_2 = add_player(&mut state, "player_2").unwrap();
            let player_3 = add_player(&mut state, "player_3").unwrap();

            assert_eq!(state.players.len(), 3);
            assert_eq!(state.status, state::GameStatus::Joining);
            let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();
            assert_eq!(starting_balance, STARTING_BALANCE);

            start_game(&mut state).unwrap();

            (state, (player_1, player_2, player_3))
        }

        pub fn deal_biased_deck(
            state: &mut state::State,
            player_1: &state::PlayerId,
            player_2: &state::PlayerId,
            player_1_loses: bool,
        ) {
            let mut deck = cards::Deck::ordered();
            let (loser, winner) = if player_1_loses {
                (player_1, player_2)
            } else {
                (player_2, player_1)
            };

            // higher value cards first
            let winner = state.players.get_mut(winner).unwrap();
            winner.cards = (deck.pop(), deck.pop());
            // then lower value cards
            let loser = state.players.get_mut(loser).unwrap();
            loser.cards = (deck.pop(), deck.pop());

            // set the round deck
            state.round.deck = cards::Deck::ordered();
        }

        pub fn add_player(
            state: &mut state::State,
            player_name: &str,
        ) -> Result<state::PlayerId, String> {
            let player_id = state::PlayerId::default();
            super::add_new_player(state, player_name, player_id)
        }
    }
}
