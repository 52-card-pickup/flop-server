use std::collections::{BTreeMap, BTreeSet};

use rand::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Deck(Vec<Card>);

impl Deck {
    pub fn pop(&mut self) -> Option<Card> {
        self.0.pop()
    }
}

impl Default for Deck {
    fn default() -> Self {
        let suites = vec![
            CardSuite::Hearts,
            CardSuite::Diamonds,
            CardSuite::Clubs,
            CardSuite::Spades,
        ];
        let values = vec![
            CardValue::Ace,
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
        let mut rng = rand::thread_rng();
        deck.shuffle(&mut rng);
        Deck(deck)
    }
}

#[derive(Clone)]
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

impl Card {
    pub fn evaluate_hand(player_cards: &(Self, Self), table_cards: &[Self]) -> (HandStrength, u64) {
        todo!()
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardSuite {
    Hearts,
    Diamonds,
    Clubs,
    Spades,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CardValue {
    Ace,
    Two,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
    Jack,
    Queen,
    King,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cards_evaluate_hand_royal_flush() {
        let player_cards = (
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Ace,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::King,
            },
        );
        let table_cards = vec![
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Queen,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Jack,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Ten,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Nine,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Eight,
            },
        ];
        let (score, value) = Card::evaluate_hand(&player_cards, &table_cards);
        assert_eq!(score, HandStrength::RoyalFlush);
        assert_eq!(value, 60);
    }

    #[test]
    fn cards_evaluate_hand_compare_players() {
        let player_1_cards = (
            Card {
                suite: CardSuite::Diamonds,
                value: CardValue::Ace,
            },
            Card {
                suite: CardSuite::Diamonds,
                value: CardValue::King,
            },
        );
        let player_2_cards = (
            Card {
                suite: CardSuite::Clubs,
                value: CardValue::Queen,
            },
            Card {
                suite: CardSuite::Clubs,
                value: CardValue::Jack,
            },
        );
        let table_cards = vec![
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Queen,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::King,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Ace,
            },
            Card {
                suite: CardSuite::Hearts,
                value: CardValue::Seven,
            },
            Card {
                suite: CardSuite::Spades,
                value: CardValue::Six,
            },
        ];
        let (player_1_score, player_1_value) = Card::evaluate_hand(&player_1_cards, &table_cards);
        let (player_2_score, player_2_value) = Card::evaluate_hand(&player_2_cards, &table_cards);
        assert_eq!(player_1_score, HandStrength::TwoPair);
        assert_eq!(player_1_value, 12);
        assert_eq!(player_2_score, HandStrength::OnePair);
        assert_eq!(player_2_value, 11);

        assert!((player_1_score, player_1_value) > (player_2_score, player_2_value));
    }
}
