use crate::{cards, models, state};

pub trait StateExt {
    fn cards_on_table(&self) -> Vec<(cards::CardSuite, cards::CardValue)>;
    fn cards_in_hand(
        &self,
        player_id: &state::PlayerId,
    ) -> (
        (cards::CardSuite, cards::CardValue),
        (cards::CardSuite, cards::CardValue),
    );
    fn game_phase(&self) -> models::GamePhase;
    fn room_players(&self) -> Vec<models::GameClientPlayer>;
    fn call_amount(&self) -> Option<u64>;
    fn min_raise_by(&self) -> u64;
    fn turn_expires_dt(&self, player_id: &state::PlayerId) -> Option<u64>;
    fn completed_game(&self) -> Option<models::CompletedGame>;
}

impl StateExt for state::State {
    fn cards_on_table(&self) -> Vec<(cards::CardSuite, cards::CardValue)> {
        let cards = self
            .round
            .cards_on_table
            .iter()
            .map(|c| (c.suite.clone(), c.value.clone()))
            .collect();
        cards
    }

    fn cards_in_hand(
        &self,
        player_id: &state::PlayerId,
    ) -> (
        (cards::CardSuite, cards::CardValue),
        (cards::CardSuite, cards::CardValue),
    ) {
        let player = self.players.get(player_id).unwrap();
        let cards = player.cards.clone();
        let cards = (
            (cards.0.suite.clone(), cards.0.value.clone()),
            (cards.1.suite.clone(), cards.1.value.clone()),
        );
        cards
    }

    fn game_phase(&self) -> models::GamePhase {
        match self.status {
            state::GameStatus::Joining => models::GamePhase::Waiting,
            state::GameStatus::Playing => models::GamePhase::Playing,
            state::GameStatus::Complete => models::GamePhase::Complete,
        }
    }

    fn room_players(&self) -> Vec<models::GameClientPlayer> {
        let players = self
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

    fn call_amount(&self) -> Option<u64> {
        self.round.raises.last().map(|(_, last_stake)| *last_stake)
    }

    fn min_raise_by(&self) -> u64 {
        let raises: Vec<_> = [0_u64]
            .into_iter()
            .chain(self.round.raises.iter().map(|(_, s)| *s))
            .collect();

        let min_raise = raises
            .windows(2)
            .map(|w| w[1] - w[0])
            .last()
            .unwrap_or(state::BIG_BLIND);

        min_raise
    }

    fn turn_expires_dt(&self, player_id: &state::PlayerId) -> Option<u64> {
        self.players
            .get(player_id)
            .and_then(|p| p.ttl.map(|dt| dt.into()))
    }

    fn completed_game(&self) -> Option<models::CompletedGame> {
        if self.status != state::GameStatus::Complete {
            return None;
        }
        let (winner, winning_hand) = self
            .players
            .values()
            .map(|p| {
                (
                    p,
                    cards::Card::evaluate_hand(&p.cards, &self.round.cards_on_table),
                )
            })
            .max_by_key(|(_, score)| score.clone())?;

        let winner_idx = self.players.keys().position(|id| id == &winner.id).unwrap();

        let player_cards = self
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
}
