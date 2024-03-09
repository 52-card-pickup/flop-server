use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use crate::cards::{Card, Deck};

pub use id::PlayerId;

pub type SharedState = Arc<RwLock<State>>;

#[derive(Default)]
pub struct State {
    pub cards_on_table: Vec<Card>,
    pub players: BTreeMap<PlayerId, Player>,
    pub pot: u64,
    pub deck: Deck,
    pub players_turn: Option<PlayerId>,
    pub last_update: u64,
    pub status: GameStatus,
}

#[derive(Clone)]
pub struct Player {
    pub name: String,
    pub id: PlayerId,
    pub balance: u64,
    pub stake: u64,
    pub cards: (Card, Card),
}

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    #[default]
    Joining,
    Playing,
    Complete,
}

mod id {
    use std::{fmt::Display, str::FromStr};

    #[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
    pub struct PlayerId(String);

    impl Display for PlayerId {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl FromStr for PlayerId {
        type Err = uuid::Error;

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let uuid = uuid::Uuid::parse_str(s)?;
            Ok(PlayerId(uuid.to_string()))
        }
    }

    impl Default for PlayerId {
        fn default() -> Self {
            PlayerId(uuid::Uuid::new_v4().to_string())
        }
    }
}
