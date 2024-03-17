use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use crate::cards::{Card, Deck};

pub use id::PlayerId;

pub type SharedState = Arc<RwLock<State>>;

pub const STARTING_BALANCE: u64 = 1000;
pub const SMALL_BLIND: u64 = 10;
pub const BIG_BLIND: u64 = 20;
pub const PLAYER_TURN_TIMEOUT_SECONDS: u64 = 30;

#[derive(Default)]
pub struct State {
    pub players: BTreeMap<PlayerId, Player>,
    pub round: Round,
    pub last_update: dt::SignalInstant,
    pub status: GameStatus,
}

#[derive(Default)]
pub struct Round {
    pub pot: u64,
    pub deck: Deck,
    pub cards_on_table: Vec<Card>,
    pub players_turn: Option<PlayerId>,
    pub dealer: Option<PlayerId>,
    pub raises: Vec<(PlayerId, u64)>,
}

#[derive(Clone)]
pub struct Player {
    pub name: String,
    pub id: PlayerId,
    pub balance: u64,
    pub stake: u64,
    pub folded: bool,
    pub ttl: Option<dt::Instant>,
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

pub mod dt {
    use std::time::{SystemTime, UNIX_EPOCH};

    pub use watch::SignalInstant;

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Instant(u64);

    impl Instant {
        pub fn set_now(&mut self) {
            self.0 = Self::now_ms();
        }

        pub fn add_seconds(&mut self, seconds: u64) {
            self.0 += seconds * 1000;
        }

        fn now_ms() -> u64 {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
        }
    }

    impl Into<u64> for Instant {
        fn into(self) -> u64 {
            self.0
        }
    }

    impl From<u64> for Instant {
        fn from(ms: u64) -> Self {
            Self(ms)
        }
    }

    impl Default for Instant {
        fn default() -> Self {
            Instant(Self::now_ms())
        }
    }

    pub mod watch {
        use std::sync::{Arc, Mutex};

        use tokio::sync::oneshot;

        use super::Instant;

        #[derive(Clone, Default)]
        pub struct SignalInstant(
            Instant,
            Arc<Mutex<Vec<tokio::sync::oneshot::Sender<Instant>>>>,
        );

        impl SignalInstant {
            pub fn as_u64(&self) -> u64 {
                self.0.into()
            }

            pub fn set_now(&mut self) {
                self.0.set_now();
                let senders = {
                    let mut senders = self.1.lock().unwrap();
                    senders.drain(..).collect::<Vec<_>>()
                };
                for sender in senders {
                    let _ = sender.send(self.0);
                }
            }

            pub fn wait_for(&self, wait_until: Instant) -> oneshot::Receiver<Instant> {
                let when = self.0;
                let (sender, receiver) = oneshot::channel();

                if when < wait_until {
                    sender.send(when).unwrap();
                } else {
                    let mut senders = self.1.lock().unwrap();
                    senders.push(sender);
                }
                receiver
            }
        }
    }
}
