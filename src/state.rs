use std::sync::Arc;

use crate::cards::{Card, Deck};

pub use id::PlayerId;
use serde::Deserialize;
use tokio::sync::RwLock;

use self::players::Players;

pub type SharedState = Arc<RwLock<State>>;

pub const STARTING_BALANCE: u64 = 1000;
pub const SMALL_BLIND: u64 = 10;
pub const BIG_BLIND: u64 = 20;
pub const PLAYER_TURN_TIMEOUT_SECONDS: u64 = 60;
pub const GAME_IDLE_TIMEOUT_SECONDS: u64 = 300;
pub const MAX_PLAYERS: usize = 8;

#[derive(Default)]
pub struct State {
    pub players: Players,
    pub round: Round,
    pub last_update: dt::SignalInstant,
    pub status: GameStatus,
    pub vote: Option<Vote>,
}

#[derive(Default)]
pub struct Vote {
    pub start_time: dt::SignalInstant,
    pub end_time: dt::SignalInstant,

    pub votes: Vec<(PlayerId, bool)>,
    pub motion: Motion,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
pub struct Motion {
    pub motion: MotionType,
    pub player_id: PlayerId,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub enum MotionType {
    KickPlayer,
    DoubleBlinds,
}

impl Default for MotionType {
    fn default() -> Self {
        MotionType::KickPlayer
    }
}

#[derive(Default)]
pub struct Round {
    pub pot: u64,
    pub deck: Deck,
    pub cards_on_table: Vec<Card>,
    pub players_turn: Option<PlayerId>,
    pub raises: Vec<(PlayerId, u64)>,
    pub calls: Vec<(PlayerId, u64)>,
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

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    #[default]
    Joining,
    Playing,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BetAction {
    Check,
    Call,
    RaiseTo(u64),
}

mod id {
    use serde::Deserialize;
    use std::{fmt::Display, str::FromStr};

    #[derive(
        Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Deserialize, schemars::JsonSchema,
    )]
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
        use std::future::Future;
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
                let mut senders = self.1.lock().unwrap();
                for sender in senders.drain(..) {
                    let _ = sender.send(self.0);
                }
            }

            pub fn add_ms(&mut self, ms: u64) {
                self.0 .0 += ms;
            }

            pub fn wait_for(&self, since: Instant) -> impl Future<Output = Option<Instant>> {
                let receiver = match self.try_wait_for(since) {
                    Some(receiver) => receiver,
                    None => {
                        let (sender, receiver) = oneshot::channel();
                        sender.send(self.0).unwrap();
                        receiver
                    }
                };
                async move { receiver.await.ok() }
            }

            pub fn try_wait_for(&self, since: Instant) -> Option<oneshot::Receiver<Instant>> {
                let when = self.0;
                if when > since {
                    return None;
                }

                let (sender, receiver) = oneshot::channel();
                let mut senders = self.1.lock().unwrap();
                senders.push(sender);
                Some(receiver)
            }
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn signal_instant_returns_instantly_if_changed() {
                let signal = SignalInstant::default();
                let now = signal.as_u64();
                let before = now - 1000;
                let receiver = signal.try_wait_for(Instant(before));

                assert!(receiver.is_none());
            }

            #[test]
            fn signal_instant_waits_for_change_if_not_changed() {
                let signal = SignalInstant::default();
                let now = signal.as_u64();
                let receiver = signal.try_wait_for(Instant(now));

                assert!(receiver.is_some());
            }
        }
    }
}

mod players {
    use std::collections::VecDeque;

    use super::{Player, PlayerId};

    #[derive(Default)]
    pub struct Players(VecDeque<(PlayerId, Player)>);

    impl Players {
        pub fn insert(&mut self, player_id: PlayerId, player: Player) {
            self.0.push_back((player_id, player));
        }

        pub fn get(&self, id: &PlayerId) -> Option<&Player> {
            self.0
                .iter()
                .find_map(|(pid, p)| if pid == id { Some(p) } else { None })
        }

        pub fn get_mut(&mut self, id: &PlayerId) -> Option<&mut Player> {
            self.0
                .iter_mut()
                .find_map(|(pid, p)| if pid == id { Some(p) } else { None })
        }

        pub fn remove(&mut self, id: &PlayerId) -> Option<Player> {
            let idx = self.0.iter().position(|(pid, _)| pid == id)?;
            self.0.remove(idx).map(|(_, p)| p)
        }

        pub fn pop_first(&mut self) -> Option<(PlayerId, Player)> {
            self.0.pop_front()
        }

        pub fn iter(&self) -> std::collections::vec_deque::Iter<(PlayerId, Player)> {
            self.0.iter()
        }

        pub fn keys(&self) -> std::vec::IntoIter<&PlayerId> {
            self.0
                .iter()
                .map(|(player_id, _)| player_id)
                .collect::<Vec<_>>()
                .into_iter()
        }

        pub fn values(&self) -> impl Iterator<Item = &Player> {
            self.0.iter().map(|(_, p)| p)
        }

        pub fn values_mut(&mut self) -> impl Iterator<Item = &mut Player> {
            self.0.iter_mut().map(|(_, p)| p)
        }

        pub fn len(&self) -> usize {
            self.0.len()
        }
    }
}
