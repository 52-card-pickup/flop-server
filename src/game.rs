use crate::{cards, models, state};

use tracing::info;

pub(crate) fn spawn_game_worker(state: state::SharedState) {
    async fn run_tasks(state: &state::SharedState) {
        let now = state::dt::Instant::default();

        let (last_update, current_player, status, ballot) = {
            let state = state.read().await;
            let last_update = state.last_update.as_u64();
            let players_turn = state.round.players_turn.clone();
            let current_player = players_turn.and_then(|id| state.players.get(&id)).cloned();
            let ballot = state.ballot.clone();

            (last_update, current_player, state.status, ballot)
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

        let now_ms: u64 = now.into();
        if now_ms - last_update > state::GAME_IDLE_TIMEOUT_SECONDS * 1000 {
            info!("Game idle timeout, resetting game");
            let mut state = state.write().await;
            *state = state::State::default();
            return;
        }

        if let Some(player) = current_player {
            let expired = player.ttl.map(|ttl| ttl < now).unwrap_or(false);
            if expired {
                info!("Player {} turn expired", player.id);
                let mut state = state.write().await;

                fold_player(&mut state, &player.id).unwrap();

                // TODO: notify player, soft kick
                state.players.remove(&player.id);
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

        if ballot.is_some() {
            let mut state = state.write().await;
            if let Some(vote) = state.ballot.as_ref() {
                if vote.end_time.as_u64() < state::dt::Instant::default().as_u64() {
                    end_ballot(&mut state).unwrap();
                }
            }
        }
    }

    tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
            run_tasks(&state).await;
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

pub(crate) fn accept_player_bet(
    state: &mut state::State,
    player_id: &state::PlayerId,
    action: state::BetAction,
) -> Result<(), String> {
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
            state.round.calls.push((player_id.clone(), call));
            let new_balance = player
                .balance
                .checked_sub(call)
                .ok_or("Not enough balance".to_string())?;
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
    let small_blind_stake = small_blind_player.balance.min(state.small_blind);
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

    let big_blind_stake = big_blind_player.balance.min(state.small_blind * 2);

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
    if call_amount > 0 && (!first_round || state.round.raises.len() > 2) {
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
        let is_big_blind_first_round = current_player_id == state.players.keys().nth(1).unwrap();
        let current_player_stake_is_call_amount =
            player_stake_in_round(state, current_player_id) == state.small_blind * 2;
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
        .filter(|(_, (_, player))| !player.folded)
        .next()
        .map(|(_, (id, _))| id.clone());

    next_player.or_else(|| {
        state
            .players
            .iter()
            .filter(|(_, player)| !player.folded)
            .next()
            .filter(|(_, player)| player_stake_in_round(state, &player.id) != call_amount)
            .map(|(id, _)| id.clone())
    })
}

fn validate_bet_action(
    state: &mut state::State,
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
            consume_action_queue(state);
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

    match stakes.len() {
        1 => {
            let winner = stakes.first().unwrap();
            let player = state.players.get_mut(&winner.id).unwrap();
            player.balance += round.pot;
            info!(
                "Player {} is the only player left, whole pot is won, pot: {}",
                player.id, round.pot
            );
            return;
        }
        0 => {
            info!("No players left, pot is lost");
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

        info!(
            "Paid out pot to winners. Pot: {}, Winner(s): {}",
            pot,
            winners.join(", "),
        );
    }

    let pot_splits = pots.len().saturating_sub(1);
    let best_hand = scores.iter().map(|(_, score)| score.clone()).max().unwrap();
    info!(
        "Game complete, pot: {} ({} splits) (rank {:?}) cards: {:?}",
        round.pot,
        pot_splits,
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
            consume_action_queue(state);
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
        .max(state.small_blind * 2);

    let min_raise_to = max_raise + largest_raise_diff;
    min_raise_to
}

pub(crate) fn turn_expires_dt(state: &state::State, player_id: &state::PlayerId) -> Option<u64> {
    state
        .players
        .get(player_id)
        .and_then(|p| p.ttl.map(|dt| dt.into()))
}

pub(crate) fn player_start_ballot(
    state: &mut state::State,
    action: state::ballot::BallotAction,
) -> Result<(), String> {
    if state.ballot.is_some() {
        return Err("Vote already in progress".to_string());
    }

    let mut vote = state::ballot::Ballot::default();
    vote.action = action;
    vote.end_time.add_seconds(60);
    state.ballot = Some(vote);

    Ok(())
}

pub(crate) fn player_cast_vote_in_ballot(
    state: &mut state::State,
    player_id: &state::PlayerId,
    vote: bool,
) -> Result<(), String> {
    let voted = vote;

    let vote = match &mut state.ballot {
        Some(vote) => vote,
        None => return Err("No vote in progress".to_string()),
    };

    if vote.end_time.as_u64() < state.last_update.as_u64() {
        return Err("Vote has expired".to_string());
    }

    let player_ids_in_cast_votes = vote.votes.iter().map(|(id, _)| id).collect::<Vec<_>>();
    let has_player_already_voted = player_ids_in_cast_votes.contains(&player_id);

    if has_player_already_voted {
        return Err("Player has already voted".to_string());
    }

    vote.votes.push((player_id.clone(), voted));
    if vote.votes.len() == state.players.len() {
        end_ballot(state)?;
    }
    Ok(())
}

pub(crate) fn end_ballot(state: &mut state::State) -> Result<(), String> {
    info!("Ending ballot");

    let vote = match &state.ballot {
        Some(vote) => vote,
        None => return Err("No vote in progress".to_string()),
    };

    if (vote.votes.len() as f64) / (state.players.len() as f64) < 0.5 {
        return Err("Not enough players voted".to_string());
    }

    let votes_for = vote.votes.iter().filter(|(_, vote)| *vote).count();
    let votes_against = vote.votes.iter().filter(|(_, vote)| !*vote).count();

    let majority_voted_for = votes_for > votes_against;

    match &vote.action {
        state::ballot::BallotAction::KickPlayer(player_id) => {
            if majority_voted_for {
                info!("Majority voted to kick player {}", player_id);
                state
                    .action_queue
                    .push(state::ballot::BallotAction::KickPlayer(player_id.clone()));
            }
        }
        state::ballot::BallotAction::DoubleBlinds => {
            if majority_voted_for {
                state
                    .action_queue
                    .push(state::ballot::BallotAction::DoubleBlinds);
            }
        }
    }

    state.ballot = None;

    Ok(())
}

fn double_blinds(state: &mut state::State) {
    state.small_blind *= 2;
}

pub(crate) fn ballot_details(
    state: &state::State,
    player: &state::Player,
) -> Option<models::BallotDetails> {
    let player_has_voted = state
        .ballot
        .as_ref()
        .map(|vote| vote.votes.iter().any(|(id, _)| id == &player.id))
        .unwrap_or(false);

    if player_has_voted {
        return None;
    }

    if let Some(vote) = &state.ballot {
        if vote.end_time.as_u64() < state::dt::Instant::default().as_u64() {
            return None;
        }
    }

    match &state.ballot {
        Some(vote) => match &vote.action {
            state::ballot::BallotAction::KickPlayer(player_id) => {
                if player.id == *player_id {
                    return None;
                }
                let player_to_kick = state.players.get(player_id).unwrap();
                let ballot_details = models::BallotDetails {
                    action: models::BallotAction::KickPlayer(player_to_kick.name.clone()),
                    expires_dt: vote.end_time.as_u64(),
                };
                return Some(ballot_details);
            }
            state::ballot::BallotAction::DoubleBlinds => {
                let ballot_details = models::BallotDetails {
                    action: models::BallotAction::DoubleBlinds,
                    expires_dt: vote.end_time.as_u64(),
                };
                return Some(ballot_details);
            }
        },
        None => None,
    }
}

pub(crate) fn start_vote_options(
    state: &state::State,
    player: &state::Player,
) -> Option<models::StartBallotChoices> {
    if state.ballot.is_some() {
        return None;
    }

    let mut kick_player_options = state
        .players
        .iter()
        .filter(|(id, _)| *id != player.id)
        .map(|player| models::KickPlayerOption {
            name: player.1.name.clone(),
            id: player.1.id.clone().to_string(),
        })
        .collect::<Vec<_>>();

    if state.players.len() <= 2 {
        kick_player_options.clear();
    }

    let are_blinds_less_than_half_of_max_player_balance =
        state.small_blind * 2 < state.players.values().map(|p| p.balance).max().unwrap() / 2;

    let start_vote_options = models::StartBallotChoices {
        kick_player: kick_player_options,
        double_blinds: are_blinds_less_than_half_of_max_player_balance,
    };

    Some(start_vote_options)
}

fn consume_action_queue(state: &mut state::State) {
    while let Some(action) = state.action_queue.pop() {
        match action {
            state::ballot::BallotAction::KickPlayer(player_id) => {
                info!("Kicking player {}", player_id);
                state.players.remove(&player_id);
                if state.players.len() < 2 {
                    info!("Not enough players, pausing game until more players join");
                    state.status = state::GameStatus::Joining;
                    state.round = state::Round::default();
                    for player in state.players.values_mut() {
                        player.ttl = None;
                    }
                }
            }
            state::ballot::BallotAction::DoubleBlinds => {
                info!("Doubling blinds to {}", state.small_blind * 2);
                double_blinds(state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{game::tests::fixtures::GameFixture, state::STARTING_BALANCE};
    use state::ballot::BallotAction as B;
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
    fn game_pays_outright_winner_from_pot() {
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

        assert_eq!(pot_before_payout, (state.small_blind * 2 * 5) + (500 * 3));
        assert_eq!(cards_on_table(state).len(), 5);

        accept_player_bet(state, &player_1, P::Check).unwrap();
        accept_player_bet(state, &player_2, P::Check).unwrap();
        accept_player_bet(state, &player_3, P::Check).unwrap();

        assert_eq!(cards_on_table(state).len(), 5);
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        // wins remaining 4 players blinds and remaining 2 players 500 bets
        let winner = state.players.get(&player_1).unwrap();
        let expected_balance = STARTING_BALANCE + state.small_blind * 2 * 4 + 500 * 2;
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
        assert_eq!(winner.balance, STARTING_BALANCE + state.small_blind);

        start_game(&mut state).unwrap();
        assert_eq!(cards_on_table(&state).len(), 0);

        let player_2_data = state.players.get(&player_2).unwrap();
        assert_eq!(player_2_data.stake, state.small_blind);

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
        assert_eq!(winner.balance, STARTING_BALANCE + state.small_blind);
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
        assert_eq!(winner.balance, STARTING_BALANCE + state.small_blind * 2);
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
        assert_eq!(winner.balance, STARTING_BALANCE + state.small_blind);
    }

    #[test]
    fn two_player_game_reraising_minimum_works() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round4);
        let big_blind = state.small_blind * 2;
        assert_eq!(state.round.pot, 40);
        accept_player_bet(&mut state, &player_1, P::RaiseTo(big_blind)).unwrap();
        assert_eq!(state.status, state::GameStatus::Playing);
        assert_eq!(state.round.pot, 60);
        assert_eq!(
            state.players.get(&player_1).unwrap().stake,
            state.small_blind * 2 * 2
        );
        assert_eq!(
            state.players.get(&player_2).unwrap().stake,
            state.small_blind * 2
        );

        accept_player_bet(&mut state, &player_2, P::RaiseTo(big_blind * 2)).unwrap();
        assert_eq!(state.round.pot, 100);
        assert_eq!(
            state.players.get(&player_1).unwrap().stake,
            state.small_blind * 2 * 2
        );
        assert_eq!(
            state.players.get(&player_2).unwrap().stake,
            state.small_blind * 2 * 3
        );

        accept_player_bet(&mut state, &player_1, P::RaiseTo(big_blind * 3)).unwrap();
        assert_eq!(
            state.players.get(&player_1).unwrap().stake,
            state.small_blind * 2 * 4
        );
        assert_eq!(
            state.players.get(&player_2).unwrap().stake,
            state.small_blind * 2 * 3
        );

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
        assert_eq!(winner.balance, STARTING_BALANCE + state.small_blind);
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
    fn two_player_game_raising_round_one() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        let big_blind = state.small_blind * 2;

        assert_eq!(cards_on_table(&state).len(), 0);

        accept_player_bet(&mut state, &player_1, P::RaiseTo(big_blind * 2)).unwrap();

        accept_player_bet(&mut state, &player_2, P::RaiseTo(big_blind * 3)).unwrap();
        accept_player_bet(&mut state, &player_1, P::Call).unwrap();

        assert_eq!(cards_on_table(&state).len(), 3);
    }

    #[test]
    fn two_player_game_raising_with_intermittent_calls_checking_balances() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        let big_blind = state.small_blind * 2;

        assert_eq!(cards_on_table(&state).len(), 0);
        assert_eq!(
            state.players.get(&player_1).unwrap().balance,
            STARTING_BALANCE - state.small_blind
        );
        assert_eq!(
            state.players.get(&player_2).unwrap().balance,
            STARTING_BALANCE - state.small_blind * 2
        );

        accept_player_bet(&mut state, &player_1, P::Call).unwrap();
        assert_eq!(
            state.players.get(&player_1).unwrap().balance,
            STARTING_BALANCE - state.small_blind * 2
        );
        accept_player_bet(&mut state, &player_2, P::RaiseTo(big_blind * 2)).unwrap();
        assert_eq!(
            state.players.get(&player_2).unwrap().balance,
            STARTING_BALANCE - 40
        );
    }

    #[test]
    fn two_player_vote_to_double_blinds() {
        let (mut state, (player_1, player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        assert_eq!(state.players.get(&player_1).unwrap().stake, 10);
        assert_eq!(state.players.get(&player_2).unwrap().stake, 20);

        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        assert_eq!(state.small_blind, 10);
        assert_eq!(state.small_blind * 2, 20);
        player_cast_vote_in_ballot(&mut state, &player_2, true).unwrap();
        consume_action_queue(&mut state);
        assert_eq!(state.small_blind, 20);
        assert_eq!(state.small_blind * 2, 40);

        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_2, false).unwrap();
        assert_eq!(state.small_blind, 20);
        assert_eq!(state.small_blind * 2, 40);

        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, false).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_2, false).unwrap();
        assert_eq!(state.small_blind, 20);
        assert_eq!(state.small_blind * 2, 40);

        fold_player(&mut state, &player_1).unwrap();

        start_game(&mut state).unwrap();
        assert_eq!(state.players.get(&player_1).unwrap().stake, 40);
        assert_eq!(state.players.get(&player_2).unwrap().stake, 20);
    }

    #[test]
    fn three_player_vote_to_double_blinds() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        assert_eq!(state.players.get(&player_1).unwrap().stake, 10);
        assert_eq!(state.players.get(&player_2).unwrap().stake, 20);
        assert_eq!(state.players.get(&player_3).unwrap().stake, 0);

        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_2, true).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_3, true).unwrap();
        consume_action_queue(&mut state);
        assert_eq!(state.small_blind, 20);
        assert_eq!(state.small_blind * 2, 40);

        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_2, false).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_3, true).unwrap();
        consume_action_queue(&mut state);
        assert_eq!(state.small_blind, 40);
        assert_eq!(state.small_blind * 2, 80);

        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, false).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_2, false).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_3, true).unwrap();
        consume_action_queue(&mut state);
        assert_eq!(state.small_blind, 40);
        assert_eq!(state.small_blind * 2, 80);

        fold_player(&mut state, &player_3).unwrap();
        fold_player(&mut state, &player_1).unwrap();

        start_game(&mut state).unwrap();
        assert_eq!(state.players.get(&player_1).unwrap().stake, 0);
        assert_eq!(state.players.get(&player_2).unwrap().stake, 40);
        assert_eq!(state.players.get(&player_3).unwrap().stake, 80);
    }

    #[test]
    fn two_player_prevent_vote_if_already_in_progress() {
        let (mut state, (_player_1, _player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        assert_eq!(
            player_start_ballot(&mut state, B::DoubleBlinds),
            Err("Vote already in progress".to_string())
        );
    }

    #[test]
    fn two_player_prevent_vote_if_vote_expired() {
        let (mut state, (player_1, _player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        let mut instant = state::dt::Instant::default();
        instant.sub_seconds(60);
        state.ballot.as_mut().unwrap().end_time = instant;
        assert_eq!(
            player_cast_vote_in_ballot(&mut state, &player_1, true),
            Err("Vote has expired".to_string())
        );
    }

    #[test]
    fn two_player_prevent_vote_if_player_already_voted() {
        let (mut state, (player_1, _player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        player_start_ballot(&mut state, B::DoubleBlinds).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        assert_eq!(
            player_cast_vote_in_ballot(&mut state, &player_1, true),
            Err("Player has already voted".to_string())
        );
    }

    #[test]
    fn two_player_no_vote_to_kick() {
        let (mut state, (player_1, _player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);

        let voting_options = start_vote_options(&state, state.players.get(&player_1).unwrap());
        assert_eq!(voting_options.unwrap().kick_player.len(), 0);
    }

    #[test]
    fn no_option_to_double_blinds_if_more_than_half_of_max_player_balance() {
        let (mut state, (player_1, _player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        state.small_blind = 320;
        let voting_options = start_vote_options(&state, state.players.get(&player_1).unwrap());
        assert_eq!(voting_options.unwrap().double_blinds, false);
    }

    #[test]
    fn three_player_no_vote_to_kick_self() {
        let (mut state, (player_1, _player_2, _player_3)) = fixtures::start_three_player_game();
        let voting_options = start_vote_options(&state, state.players.get(&player_1).unwrap());
        assert_eq!(
            voting_options
                .clone()
                .unwrap()
                .kick_player
                .iter()
                .any(|p| p.id == player_1.to_string()),
            false
        );
        assert_eq!(voting_options.unwrap().kick_player.len(), 2);
    }

    #[test]
    fn three_player_vote_to_kick() {
        let (mut state, (player_1, player_2, player_3)) = fixtures::start_three_player_game();
        player_start_ballot(&mut state, B::KickPlayer(player_2.clone())).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_3, true).unwrap();
        end_ballot(&mut state).unwrap();
        consume_action_queue(&mut state);
        assert_eq!(state.players.len(), 2);
    }

    #[test]
    fn two_player_player_cant_vote_twice() {
        let (mut state, (player_1, _player_2)) =
            fixtures::start_two_player_game(GameFixture::Round1);
        player_start_ballot(&mut state, B::KickPlayer(player_1.clone())).unwrap();
        player_cast_vote_in_ballot(&mut state, &player_1, true).unwrap();
        assert_eq!(
            player_cast_vote_in_ballot(&mut state, &player_1, true),
            Err("Player has already voted".to_string())
        );
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
            accept_player_bet(&mut state, &player_1, P::Call).unwrap();
            assert_eq!(state.round.pot, 40);
            accept_player_bet(&mut state, &player_2, P::Check).unwrap();
            assert_eq!(state.round.pot, 40);
            assert_eq!(cards_on_table(&state).len(), 3);
            if game_fixture == GameFixture::Round2 {
                return (state, (player_1, player_2));
            }

            accept_player_bet(&mut state, &player_1, P::Check).unwrap();
            accept_player_bet(&mut state, &player_2, P::Check).unwrap();
            assert_eq!(cards_on_table(&state).len(), 4);
            if game_fixture == GameFixture::Round3 {
                return (state, (player_1, player_2));
            }

            accept_player_bet(&mut state, &player_1, P::Check).unwrap();
            accept_player_bet(&mut state, &player_2, P::Check).unwrap();

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
