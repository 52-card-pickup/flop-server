use tracing::info;

use crate::{cards, models, state};

pub use state_ext::StateExt;

mod round;
mod state_ext;

pub(crate) fn spawn_game_worker(state: state::SharedState) {
    fn run_tasks(state: &state::SharedState) {
        let now = state::dt::Instant::default();

        let (last_update, current_player, status) = {
            let state = state.read().unwrap();
            let last_update: u64 = state.last_update.into();
            let players_turn = state.round.players_turn.clone();
            let current_player = players_turn.and_then(|id| state.players.get(&id)).cloned();

            (last_update, current_player, state.status)
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

            let mut state = state.write().unwrap();
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
    round::reset_players(state);
    round::next_turn(state);
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

    let stake = round::validate_player_stake(state, stake, player_id, &action)?;

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

    round::get_next_players_turn(state, player_id);

    if state.round.players_turn.is_none() {
        round::complete_round(state);
    }

    Ok(())
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

            round::reset_players(state);
            round::rotate_dealer(state);
            state.status = state::GameStatus::Complete;
            state.round.raises.clear();
            return Ok(());
        }
        _ => {}
    }

    round::get_next_players_turn(state, player_id);

    if state.round.players_turn.is_none() {
        round::complete_round(state);
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

#[cfg(test)]
mod tests {
    use tracing::info;

    use super::*;
    use crate::{
        cards, models,
        state::{self, BIG_BLIND, SMALL_BLIND, STARTING_BALANCE},
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

        assert_eq!(state.cards_on_table().len(), 0);

        accept_player_stake(state, &player_3, BIG_BLIND, P::Call).expect("R1-P3");
        accept_player_stake(state, &player_4, BIG_BLIND, P::Call).expect("R1-P4");
        accept_player_stake(state, &player_5, BIG_BLIND, P::Call).expect("R1-P5");
        accept_player_stake(state, &player_1, BIG_BLIND, P::Call).expect("R1-P1");
        accept_player_stake(state, &player_2, 0, P::Check).expect("R1-P2");

        assert_eq!(state.cards_on_table().len(), 3);

        accept_player_stake(state, &player_1, 500, P::Raise).expect("R2-P1");
        accept_player_stake(state, &player_2, 0, P::Call).expect("R2-P2");
        accept_player_stake(state, &player_3, 0, P::Call).expect("R2-P3");
        fold_player(state, &player_4).expect("R2-P4");
        fold_player(state, &player_5).expect("R2-P4");

        assert_eq!(state.cards_on_table().len(), 4);

        accept_player_stake(state, &player_1, 0, P::Check).unwrap();
        accept_player_stake(state, &player_2, 0, P::Check).unwrap();
        accept_player_stake(state, &player_3, 0, P::Check).unwrap();

        let pot_before_payout = state.round.pot;
        let winner_balance_before_payout = state.players.get(&player_1).unwrap().balance;

        assert_eq!(pot_before_payout, (BIG_BLIND * 5) + (500 * 3));
        assert_eq!(state.cards_on_table().len(), 5);

        accept_player_stake(state, &player_1, 0, P::Check).unwrap();
        accept_player_stake(state, &player_2, 0, P::Check).unwrap();
        accept_player_stake(state, &player_3, 0, P::Check).unwrap();

        assert_eq!(state.cards_on_table().len(), 5);
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
    fn game_pays_out_to_winner_after_others_fold() {
        use models::PlayAction as P;

        let (mut state, (player_1, player_2)) = fixtures::start_two_player_game();

        accept_player_stake(&mut state, &player_1, SMALL_BLIND, P::Call).expect("R1-P1");
        accept_player_stake(&mut state, &player_2, 0, P::Call).expect("R1-P2");

        assert_eq!(state.cards_on_table().len(), 3);

        fold_player(&mut state, &player_1).expect("R2-P1");
        info!(
            "Player 2 stakes: {}",
            state.players.get(&player_2).unwrap().stake
        );
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        let winner = state.players.get(&player_2).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + BIG_BLIND);
    }

    #[test]
    fn two_player_game_fold_on_small_blind() {
        let (mut state, (player_1, player_2)) = fixtures::start_two_player_game();
        assert_eq!(state.cards_on_table().len(), 0);
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

        let (mut state, (player_1, player_2)) = fixtures::start_two_player_game();
        assert_eq!(state.cards_on_table().len(), 0);
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
        use models::PlayAction as P;

        let (mut state, (player_1, player_2)) = fixtures::start_two_player_game();
        assert_eq!(state.cards_on_table().len(), 0);
        assert_eq!(state.round.pot, 30);

        accept_player_stake(&mut state, &player_1, BIG_BLIND, P::Call).unwrap();
        accept_player_stake(&mut state, &player_2, BIG_BLIND * 2, P::Raise).unwrap();
        fold_player(&mut state, &player_1).expect("R2-P1");
        assert_eq!(state.status, state::GameStatus::Complete);
        assert_eq!(state.round.pot, 0);

        let winner = state.players.get(&player_2).unwrap();
        assert_eq!(winner.balance, STARTING_BALANCE + BIG_BLIND);
    }

    mod fixtures {
        use super::*;

        pub fn start_two_player_game() -> (state::State, (state::PlayerId, state::PlayerId)) {
            let mut state = state::State::default();
            state.round.deck = cards::Deck::ordered();

            let player_1 = add_new_player(&mut state, "player_1").unwrap();
            let player_2 = add_new_player(&mut state, "player_2").unwrap();

            assert_eq!(state.players.len(), 2);
            assert_eq!(state.status, state::GameStatus::Joining);
            let starting_balance = state.players.iter().map(|(_, p)| p.balance).next().unwrap();
            assert_eq!(starting_balance, STARTING_BALANCE);

            start_game(&mut state).unwrap();

            (state, (player_1, player_2))
        }
    }
}
