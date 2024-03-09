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
        let mut all_cards = vec![player_cards.0.clone(), player_cards.1.clone()];
        all_cards.extend_from_slice(table_cards);
        all_cards.sort_by_key(|c| c.value.clone() as u64);

        let by_suite: BTreeMap<_, Vec<_>> = all_cards.iter().fold(BTreeMap::new(), |mut acc, c| {
            acc.entry(c.suite.clone()).or_default().push(c);
            acc
        });

        let by_value: BTreeMap<_, Vec<_>> = all_cards.iter().fold(BTreeMap::new(), |mut acc, c| {
            acc.entry(c.value.clone()).or_default().push(c);
            acc
        });

        let deduped_values: BTreeSet<_> =
            all_cards.iter().map(|c| c.value.clone() as u64).collect();
        let deduped_values: Vec<_> = deduped_values.into_iter().collect();

        // check for royal flush
        // TODO: not sure this will work
        let royal_flush = by_suite.iter().find_map(|(_suite, cards)| {
            if cards.len() >= 5 {
                if cards
                    .iter()
                    .skip_while(|c| c.value != CardValue::Ten)
                    .take(5)
                    .map(|c| c.value.clone() as u64)
                    .eq(10..=14)
                {
                    Some(cards.iter().map(|c| c.value.clone() as u64).sum())
                } else {
                    None
                }
            } else {
                None
            }
        });

        // check for straight flush
        let straight_flush = by_suite.iter().find_map(|(_suite, cards)| {
            cards
                .iter()
                .map(|c| c.value.clone() as u64)
                .collect::<Vec<_>>()
                .windows(5)
                .find_map(|w| if w[4] - w[0] == 4 { Some(w[4]) } else { None })
        });

        // check for four of a kind
        let four_of_a_kind = by_value.iter().find_map(|(value, cards)| {
            if cards.len() == 4 {
                Some(value.clone() as u64)
            } else {
                None
            }
        });

        // check for full house
        let full_house = by_value.iter().find_map(|(value, cards)| {
            if cards.len() == 3 {
                let pair = by_value.iter().find_map(|(value, cards)| {
                    if cards.len() == 2 {
                        Some(value.clone() as u64)
                    } else {
                        None
                    }
                });
                pair.map(|pair| value.clone() as u64 + pair)
            } else {
                None
            }
        });

        // check for flush
        let flush = by_suite.iter().find_map(|(_suite, cards)| {
            if cards.len() >= 5 {
                Some(cards.iter().map(|c| c.value.clone() as u64).sum())
            } else {
                None
            }
        });

        // check for straight
        let straight =
            deduped_values
                .windows(5)
                .find_map(|w| if w[4] - w[0] == 4 { Some(w[4]) } else { None });

        // check for three of a kind
        let three_of_a_kind = by_value.iter().find_map(|(value, cards)| {
            if cards.len() == 3 {
                Some(value.clone() as u64)
            } else {
                None
            }
        });

        // check for two pair
        let two_pair = by_value.iter().find_map(|(value_1, cards)| {
            if cards.len() == 2 {
                if let Some(pair) = by_value.iter().find_map(|(value_2, cards)| {
                    if cards.len() == 2 && value_1 != value_2 {
                        Some(value_2.clone() as u64)
                    } else {
                        None
                    }
                }) {
                    Some(value_1.clone() as u64 + pair)
                } else {
                    None
                }
            } else {
                None
            }
        });

        // check for one pair
        let one_pair = by_value.iter().find_map(|(value, cards)| {
            if cards.len() == 2 {
                Some(value.clone() as u64)
            } else {
                None
            }
        });

        // check for high card
        // TODO: not sure this will work
        let high_card: u64 = deduped_values.iter().rev().take(5).sum();

        let score = None
            .or(royal_flush.map(|x| (HandStrength::RoyalFlush, x)))
            .or(straight_flush.map(|x| (HandStrength::StraightFlush, x)))
            .or(four_of_a_kind.map(|x| (HandStrength::FourOfAKind, x)))
            .or(full_house.map(|x| (HandStrength::FullHouse, x)))
            .or(flush.map(|x| (HandStrength::Flush, x)))
            .or(straight.map(|x| (HandStrength::Straight, x)))
            .or(three_of_a_kind.map(|x| (HandStrength::ThreeOfAKind, x)))
            .or(two_pair.map(|x| (HandStrength::TwoPair, x)))
            .or(one_pair.map(|x| (HandStrength::OnePair, x)))
            .unwrap_or((HandStrength::HighCard, high_card));

        score
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
