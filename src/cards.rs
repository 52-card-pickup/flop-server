use std::{collections::BTreeMap, fmt::Display};

use rand::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Deck(Vec<Card>);

impl Deck {
    pub fn ordered() -> Self {
        let suites = vec![
            CardSuite::Hearts,
            CardSuite::Diamonds,
            CardSuite::Clubs,
            CardSuite::Spades,
        ];
        let values = vec![
            CardValue::Two,
            CardValue::Three,
            CardValue::Four,
            CardValue::Five,
            CardValue::Six,
            CardValue::Seven,
            CardValue::Eight,
            CardValue::Nine,
            CardValue::Ten,
            CardValue::Jack,
            CardValue::Queen,
            CardValue::King,
            CardValue::Ace,
        ];
        let mut deck = Vec::new();
        for suite in suites {
            for value in &values {
                deck.push(Card {
                    suite: suite.clone(),
                    value: value.clone(),
                });
            }
        }
        Deck(deck)
    }
    pub fn pop(&mut self) -> Card {
        self.0.pop().expect("deck is empty")
    }
    pub fn is_fresh(&self) -> bool {
        self.0.len() == 52
    }
}

impl Default for Deck {
    fn default() -> Self {
        let Deck(mut deck) = Self::ordered();
        let mut rng = rand::thread_rng();
        deck.shuffle(&mut rng);
        Self(deck)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Card {
    pub suite: CardSuite,
    pub value: CardValue,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HandStrength {
    HighCard,
    OnePair,
    TwoPair,
    ThreeOfAKind,
    Straight,
    Flush,
    FullHouse,
    FourOfAKind,
    StraightFlush,
    RoyalFlush,
}

impl Display for HandStrength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            HandStrength::HighCard => "High Card",
            HandStrength::OnePair => "Pair",
            HandStrength::TwoPair => "Two Pair",
            HandStrength::ThreeOfAKind => "Three of a Kind",
            HandStrength::Straight => "Straight",
            HandStrength::Flush => "Flush",
            HandStrength::FullHouse => "Full House",
            HandStrength::FourOfAKind => "Four of a Kind",
            HandStrength::StraightFlush => "Straight Flush",
            HandStrength::RoyalFlush => "Royal Flush",
        };
        write!(f, "{}", s)
    }
}

impl Card {
    pub fn evaluate_hand(player_cards: &(Self, Self), table_cards: &[Self]) -> EvaluatedHand {
        let mut all_cards = vec![player_cards.0, player_cards.1];
        all_cards.extend_from_slice(table_cards);
        all_cards.sort_by_key(|c| 14 - c.value as u64); // reverse sort, high cards first
        assert!(all_cards.len() >= 5, "not enough cards to evaluate hand");

        let by_suite: BTreeMap<_, Vec<_>> = all_cards.iter().fold(BTreeMap::new(), |mut acc, c| {
            acc.entry(c.suite).or_default().push(c);
            acc
        });

        let by_value: BTreeMap<_, Vec<_>> = all_cards.iter().fold(BTreeMap::new(), |mut acc, c| {
            acc.entry(c.value).or_default().push(c);
            acc
        });

        let deduped_values: Vec<_> = {
            let mut cards = all_cards.clone();
            cards.dedup_by_key(|c| c.value);
            cards
        };

        let mut with_high_low_ace: Vec<_> = deduped_values
            .iter()
            .map(|c| (c.value as u64 + 2, c.value))
            .chain(
                // handle the case where Ace is low
                deduped_values
                    .iter()
                    .filter(|c| c.value == CardValue::Ace)
                    .map(|c| (1, c.value)),
            )
            .collect();
        with_high_low_ace.sort_by_key(|(v, _)| 14 - v);

        // check for royal flush
        // example: [Ace, King, Queen, Jack, Ten] of the same suite
        for (_, cards) in by_suite.iter().filter(|(_, cards)| cards.len() >= 5) {
            let cards = cards.iter().map(|c| c.value).collect::<Vec<_>>();
            let royal_flush_cards = [
                CardValue::Ace,
                CardValue::King,
                CardValue::Queen,
                CardValue::Jack,
                CardValue::Ten,
            ];
            if cards[..5] == royal_flush_cards {
                return EvaluatedHand(HandStrength::RoyalFlush, royal_flush_cards);
            }
        }

        // check for straight flush
        // example: [8, 7, 6, 5, 4] of the same suite
        for (_, cards) in by_suite.iter().filter(|(_, cards)| cards.len() >= 5) {
            let cards = cards.iter().map(|c| c.value).collect::<Vec<_>>();
            for w in cards.windows(5) {
                if (w[0] as u64) - (w[4] as u64) == 4 {
                    return EvaluatedHand(
                        HandStrength::StraightFlush,
                        [w[0], w[1], w[2], w[3], w[4]],
                    );
                }
            }
        }

        // check for four of a kind
        // example: [King, King, King, King, 2]
        for (value, _) in by_value.iter().filter(|(_, cards)| cards.len() == 4) {
            let other = all_cards
                .iter()
                .find(|v| v.value != *value)
                .expect("four of a kind should have a card of a different value");
            let (value, other) = (*value, other.value);
            return EvaluatedHand(
                HandStrength::FourOfAKind,
                [value, value, value, value, other],
            );
        }

        // check for full house
        // example: [King, King, King, 2, 2]
        for (value, _) in by_value.iter().filter(|(_, cards)| cards.len() == 3) {
            for (other, _) in by_value
                .iter()
                .filter(|(other_value, cards)| cards.len() >= 2 && *other_value != value)
            {
                let (value, other) = (*value, *other);
                return EvaluatedHand(HandStrength::FullHouse, [value, value, value, other, other]);
            }
        }

        // check for flush
        // example: [King, 10, 8, 7, 2] of the same suite
        for (_, cards) in by_suite.iter().filter(|(_, cards)| cards.len() >= 5) {
            let cards = cards.iter().map(|c| c.value).collect::<Vec<_>>();
            return EvaluatedHand(
                HandStrength::Flush,
                [cards[0], cards[1], cards[2], cards[3], cards[4]],
            );
        }

        // check for straight
        // example: [8, 7, 6, 5, 4] (or [5, 4, 3, 2, Ace] for the wheel straight)
        for w in with_high_low_ace.windows(5) {
            let card1_value = w[0].0;
            let card5_value = w[4].0;
            if (card1_value - card5_value) == 4 {
                return EvaluatedHand(
                    HandStrength::Straight,
                    [w[0].1, w[1].1, w[2].1, w[3].1, w[4].1],
                );
            }
        }

        // check for three of a kind
        // example: [King, King, King, 7, 2]
        for (value, _) in by_value.iter().filter(|(_, cards)| cards.len() == 3) {
            let cards = all_cards
                .iter()
                .filter(|c| c.value != *value)
                .map(|c| c.value)
                .collect::<Vec<_>>();
            return EvaluatedHand(
                HandStrength::ThreeOfAKind,
                [*value, *value, *value, cards[0], cards[1]],
            );
        }

        // check for two pair
        // example: [King, King, 7, 7, 2]
        for (value_1, _) in by_value.iter().filter(|(_, cards)| cards.len() == 2) {
            for (value_2, _) in by_value
                .iter()
                .filter(|(value, cards)| cards.len() == 2 && value_1 != *value)
            {
                let other = all_cards
                    .iter()
                    .find(|c| c.value != *value_1 && c.value != *value_2)
                    .expect("two pair should have a card of a different value");
                let (value_1, value_2, other) = (*value_1, *value_2, other.value);
                return EvaluatedHand(
                    HandStrength::TwoPair,
                    [value_1, value_1, value_2, value_2, other],
                );
            }
        }

        // check for one pair
        // example: [King, King, 10, 7, 2]
        for (value, _) in by_value.iter().filter(|(_, cards)| cards.len() == 2) {
            let cards = all_cards
                .iter()
                .filter(|c| c.value != *value)
                .map(|c| c.value)
                .collect::<Vec<_>>();
            return EvaluatedHand(
                HandStrength::OnePair,
                [*value, *value, cards[0], cards[1], cards[2]],
            );
        }

        // fallback to high card
        // example: [King, 10, 8, 7, 2]
        EvaluatedHand(
            HandStrength::HighCard,
            [
                deduped_values[0].value,
                deduped_values[1].value,
                deduped_values[2].value,
                deduped_values[3].value,
                deduped_values[4].value,
            ],
        )
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum CardSuite {
    Hearts,
    Diamonds,
    Clubs,
    Spades,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema,
)]
pub enum CardValue {
    #[serde(rename = "2")]
    Two,
    #[serde(rename = "3")]
    Three,
    #[serde(rename = "4")]
    Four,
    #[serde(rename = "5")]
    Five,
    #[serde(rename = "6")]
    Six,
    #[serde(rename = "7")]
    Seven,
    #[serde(rename = "8")]
    Eight,
    #[serde(rename = "9")]
    Nine,
    #[serde(rename = "10")]
    Ten,
    #[serde(rename = "jack")]
    Jack,
    #[serde(rename = "queen")]
    Queen,
    #[serde(rename = "king")]
    King,
    #[serde(rename = "ace")]
    Ace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Ord)]
pub struct EvaluatedHand(HandStrength, [CardValue; 5]);

impl EvaluatedHand {
    pub fn strength(&self) -> HandStrength {
        self.0
    }
    pub fn cards(&self) -> &[CardValue; 5] {
        &self.1
    }
}

impl PartialOrd for EvaluatedHand {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        let (self_strength, self_hand) = (self.0 as u8, &self.1);
        let (other_strength, other_hand) = (other.0 as u8, &other.1);

        match self_strength.partial_cmp(&other_strength) {
            Some(std::cmp::Ordering::Equal) => self_hand
                .iter()
                .zip(other_hand.iter())
                .find_map(|(self_card_value, other_card_value)| {
                    self_card_value
                        .partial_cmp(other_card_value)
                        .filter(|x| !matches!(x, std::cmp::Ordering::Equal))
                })
                .or(Some(std::cmp::Ordering::Equal)),
            x => x,
        }
    }
}

#[cfg(test)]
mod tests {
    use helpers::{cards_1p, cards_2p};

    use super::*;

    #[test]
    fn cards_evaluate_hand_royal_flush() {
        let (player_cards, table_cards) = cards_1p("Ah Kh", "Qh Jh 10h 9h 8h");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::RoyalFlush);
    }

    #[test]
    fn cards_evaluate_hand_straight_flush() {
        let (player_cards, table_cards) = cards_1p("8h 7h", "6h 5h 4h 3c 2c");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::StraightFlush);
    }

    #[test]
    fn cards_evaluate_hand_four_of_a_kind() {
        let (player_cards, table_cards) = cards_1p("Kh Kd", "Kc Ks 2h 3c 4d");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::FourOfAKind);
    }

    #[test]
    fn cards_evaluate_hand_full_house() {
        let (player_cards, table_cards) = cards_1p("Kh Kd", "Kc 2h 2c 2s 3d");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::FullHouse);
    }

    #[test]
    fn cards_evaluate_hand_flush() {
        let (player_cards, table_cards) = cards_1p("Kh 10h", "8h 7h 2h 3c 4d");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::Flush);
    }

    #[test]
    fn cards_evaluate_hand_straight() {
        let (player_cards, table_cards) = cards_1p("8h 7d", "6h 5h 4c Kc Jd");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::Straight);
    }

    #[test]
    fn cards_evaluate_hand_straight_wheel() {
        let (player_cards, table_cards) = cards_1p("5h 4d", "3h 2h Ac Kc Jd");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::Straight);
    }

    #[test]
    fn cards_evaluate_hand_three_of_a_kind() {
        let (player_cards, table_cards) = cards_1p("Kh Kd", "Kc 7h 2c 3s 4d");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::ThreeOfAKind);
    }

    #[test]
    fn cards_evaluate_hand_two_pair() {
        let (player_cards, table_cards) = cards_1p("Kh Kd", "7c 7h 2c 2s 3d");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::TwoPair);
    }

    #[test]
    fn cards_evaluate_hand_one_pair() {
        let (player_cards, table_cards) = cards_1p("Kh 10c", "Kd 7h 2c 3s 4d");
        let EvaluatedHand(score, _) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::OnePair);
    }

    #[test]
    fn cards_evaluate_hand_compare_players() {
        let (player_1_cards, player_2_cards, table_cards) =
            cards_2p("Ad Kd", "Qc Jc", "Qh Kh Ah 7h 6s");
        let player_1_score = Card::evaluate_hand(&player_1_cards, &table_cards);
        let player_2_score = Card::evaluate_hand(&player_2_cards, &table_cards);
        assert_eq!(player_1_score.0, HandStrength::TwoPair);
        assert_eq!(player_2_score.0, HandStrength::OnePair);

        assert!(player_1_score > player_2_score);

        let (player_1_cards, player_2_cards, table_cards) =
            cards_2p("Ad Ah", "Ac As", "Qs Ks Ah 7s 6s");
        let player_1_score = Card::evaluate_hand(&player_1_cards, &table_cards);
        let player_2_score = Card::evaluate_hand(&player_2_cards, &table_cards);
        assert_eq!(player_1_score.0, HandStrength::ThreeOfAKind);
        assert_eq!(player_2_score.0, HandStrength::Flush);

        assert!(player_1_score < player_2_score);
    }

    mod helpers {
        use super::*;

        pub fn parse_shorthand(s: &str) -> Card {
            assert!(s.len() == 2 || s.len() == 3, "invalid card shorthand");
            let suite_start = s.len() - 1;

            let suite = match &s[suite_start..] {
                "h" => CardSuite::Hearts,
                "d" => CardSuite::Diamonds,
                "c" => CardSuite::Clubs,
                "s" => CardSuite::Spades,
                _ => panic!("invalid suite"),
            };
            let value = match &s[0..suite_start] {
                "2" => CardValue::Two,
                "3" => CardValue::Three,
                "4" => CardValue::Four,
                "5" => CardValue::Five,
                "6" => CardValue::Six,
                "7" => CardValue::Seven,
                "8" => CardValue::Eight,
                "9" => CardValue::Nine,
                "10" => CardValue::Ten,
                "J" => CardValue::Jack,
                "K" => CardValue::King,
                "Q" => CardValue::Queen,
                "A" => CardValue::Ace,
                _ => panic!("invalid value"),
            };
            Card { suite, value }
        }

        pub fn cards_1p(player: &str, table: &str) -> ((Card, Card), Vec<Card>) {
            let player = player
                .split_once(" ")
                .map(|(a, b)| (parse_shorthand(a), parse_shorthand(b)))
                .unwrap();
            let table = table
                .split_whitespace()
                .map(parse_shorthand)
                .collect::<Vec<_>>();

            (player, table)
        }

        pub fn cards_2p(
            player_1: &str,
            player_2: &str,
            table: &str,
        ) -> ((Card, Card), (Card, Card), Vec<Card>) {
            let player_1 = player_1
                .split_once(" ")
                .map(|(a, b)| (parse_shorthand(a), parse_shorthand(b)))
                .unwrap();
            let player_2 = player_2
                .split_once(" ")
                .map(|(a, b)| (parse_shorthand(a), parse_shorthand(b)))
                .unwrap();
            let table = table
                .split_whitespace()
                .map(parse_shorthand)
                .collect::<Vec<_>>();

            (player_1, player_2, table)
        }
    }
}
