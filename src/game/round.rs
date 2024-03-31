use tracing::info;

use crate::{cards, models, state};

use super::state_ext::StateExt;

pub(crate) fn accept_blinds(
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

pub(crate) fn select_next_players_turn(
    state: &mut state::State,
    current_player_id: &state::PlayerId,
) {
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

    let next_player = next_player.or_else(|| {
        state
            .players
            .iter()
            .skip_while(|(_, player)| player.folded)
            .next()
            .filter(|(_, player)| player.stake < target_stake)
            .map(|(id, _)| id.clone())
    });

    set_next_player_turn(state, next_player.as_ref());
}

pub(crate) fn start_players_turn(state: &mut state::State) {
    if state.players.len() < 2 {
        info!("Not enough players, pausing game");
        state.round.players_turn = None;
        return;
    }

    if state.round.cards_on_table.is_empty() {
        let mut player_ids = state.players.keys().cloned().cycle();

        let small_blind_player = player_ids.next().unwrap();
        let big_blind_player = player_ids.next().unwrap();
        let next_player_id = player_ids.next();

        info!(
            "Accepting blinds from players {} (sm) and {} (lg)",
            small_blind_player, big_blind_player
        );
        accept_blinds(state, small_blind_player, big_blind_player);
        set_next_player_turn(state, next_player_id.as_ref());
        return;
    }

    let mut player_ids = state.players.iter();
    let next_player_id = player_ids.next().map(|(p, _)| p.clone());
    set_next_player_turn(state, next_player_id.as_ref());
}

pub(crate) fn complete_round(state: &mut state::State) {
    match state.round.cards_on_table.len() {
        0 => {
            place_cards_on_table(state, 3);
            start_players_turn(state);
            state.round.raises.clear();
        }
        3 | 4 => {
            place_cards_on_table(state, 1);
            start_players_turn(state);
            state.round.raises.clear();
        }
        5 => {
            payout_game_winners(state);
            reset_players(state);
            rotate_dealer(state);
            state.status = state::GameStatus::Complete;
            state.round.raises.clear();
        }
        _ => unreachable!(),
    }
}

pub(crate) fn place_cards_on_table(state: &mut state::State, count: usize) {
    for _ in 0..count {
        let next_card = state.round.deck.pop().unwrap();
        state.round.cards_on_table.push(next_card);
    }
}

pub(crate) fn rotate_dealer(state: &mut state::State) {
    if let Some(old_dealer) = state.players.pop_first() {
        state.players.insert(old_dealer.0, old_dealer.1);
    }
}

pub(crate) fn reset_players(state: &mut state::State) {
    for player in state.players.values_mut() {
        player.stake = 0;
        player.folded = false;
    }
    state.round.players_turn = None;
}

pub(crate) fn deal_fresh_deck(state: &mut state::State) {
    state.round.deck = cards::Deck::default();
    state.round.cards_on_table.clear();

    for player in state.players.values_mut() {
        let card_1 = state.round.deck.pop().unwrap();
        let card_2 = state.round.deck.pop().unwrap();
        player.cards = (card_1, card_2);
    }
}

pub(crate) fn payout_game_winners(state: &mut state::State) {
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

pub(crate) fn validate_player_stake(
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
            let call_amount = state.call_amount().unwrap_or(0);
            let min_raise_by = state.min_raise_by();
            let min_raise = call_amount + min_raise_by;
            if stake < min_raise {
                return Err(format!("Raise must be at least {}", min_raise));
            }
            stake
        }
        models::PlayAction::Call => {
            let call = state.call_amount().ok_or("No bets to call".to_string())?;
            call - player_stake
        }
        models::PlayAction::Fold => unreachable!("Cannot handle fold action here"),
    };
    Ok(stake)
}

fn set_next_player_turn(state: &mut state::State, player_id: Option<&state::PlayerId>) {
    let next_player = player_id.and_then(|p| state.players.get_mut(&p));

    if let Some(next_player) = next_player {
        let mut expires = state::dt::Instant::default();
        expires.add_seconds(state::PLAYER_TURN_TIMEOUT_SECONDS);
        next_player.ttl = Some(expires);
    }

    state.round.players_turn = player_id.cloned();
}
