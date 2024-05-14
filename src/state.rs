use std::sync::Arc;

use crate::cards::{Card, Deck};

use axum::body::Bytes;
pub use id::PlayerId;
pub use ticker::TickerEvent;
use tokio::sync::RwLock;

use self::players::Players;

pub type SharedState = Arc<RwLock<State>>;

pub const STARTING_BALANCE: u64 = 1000;
pub const SMALL_BLIND: u64 = 10;
pub const BIG_BLIND: u64 = 20;
pub const TICKER_ITEM_TIMEOUT_SECONDS: u64 = 10;
pub const TICKER_ITEM_GAP_MILLISECONDS: u64 = 500;
pub const PLAYER_TURN_TIMEOUT_SECONDS: u64 = 60;
pub const GAME_IDLE_TIMEOUT_SECONDS: u64 = 300;
pub const MAX_PLAYERS: usize = 10;

#[derive(Default)]
pub struct State {
    pub players: Players,
    pub round: Round,
    pub last_update: dt::SignalInstant,
    pub ticker: ticker::Ticker,
    pub status: GameStatus,
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
    pub funds_token: String,
    pub balance: u64,
    pub stake: u64,
    pub folded: bool,
    pub photo: Option<(Arc<Bytes>, token::Token)>,
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

pub mod token {
    use std::fmt::Display;

    #[derive(Debug, Clone)]
    pub struct Token {
        pub value: String,
    }

    impl AsRef<str> for Token {
        #[inline]
        fn as_ref(&self) -> &str {
            <String as AsRef<str>>::as_ref(&self.value)
        }
    }

    impl Default for Token {
        fn default() -> Self {
            let guid = &uuid::Uuid::new_v4();
            let guid = guid.as_hyphenated().to_string();
            let (hash, _) = guid.split_once('-').expect("uuid should have hyphen");

            Self {
                value: hash.to_string(),
            }
        }
    }

    impl Display for Token {
        #[inline]
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            <String as Display>::fmt(&self.value, f)
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

        pub fn as_u64(&self) -> u64 {
            self.0.into()
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
        pub struct SignalInstant(Instant, Arc<Mutex<Vec<oneshot::Sender<Instant>>>>);

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

pub mod ticker {
    use std::borrow::Cow;

    use crate::cards;

    use super::{dt::Instant, BetAction, PlayerId};

    #[derive(Debug, Clone)]
    pub enum TickerEvent {
        GameStarted,
        PlayerJoined(PlayerId),
        PlayerTurnTimeout(String),
        PlayerFolded(PlayerId),
        PlayerBet(PlayerId, BetAction),
        DealerRotated(PlayerId),
        SmallBlindPosted(PlayerId),
        BigBlindPosted(PlayerId),
        CardsDealtToTable(usize),
        RoundComplete,
        Winner(PlayerId, cards::HandStrength),
        SplitPotWinners(Vec<PlayerId>, cards::HandStrength),
        PaidPot(PlayerId, u64),
        PlayerPhotoUploaded(PlayerId),
        PlayerSentEmoji(PlayerId, emoji::TickerEmoji),
        PlayerTransferredBalance(PlayerId, PlayerId, u64),
    }

    impl TickerEvent {
        pub fn format(&self, state: &super::State) -> String {
            fn format_player_action(
                state: &super::State,
                player_id: &PlayerId,
                action: &str,
            ) -> String {
                match state.players.get(player_id) {
                    Some(player) => return format!("Player {} {}", player.name, action),
                    None => return format!("Previous player {}", action),
                }
            }
            match self {
                Self::GameStarted => "Game started".to_string(),
                Self::PlayerJoined(player_id) => {
                    format_player_action(state, player_id, "joined the game")
                }
                Self::PlayerTurnTimeout(player_name) => {
                    format!("Player {} timed out", player_name)
                }
                Self::PlayerFolded(player_id) => format_player_action(state, player_id, "folded"),
                Self::PlayerBet(player_id, action) => {
                    let action: Cow<'static, str> = match action {
                        BetAction::Check => "checked".into(),
                        BetAction::Call => "called".into(),
                        BetAction::RaiseTo(amount) => format!("raised to £{}", amount).into(),
                    };
                    format_player_action(state, player_id, &action)
                }
                Self::DealerRotated(player_id) => {
                    format_player_action(state, player_id, "is the next dealer")
                }
                Self::SmallBlindPosted(player_id) => {
                    format_player_action(state, player_id, "posted the small blind")
                }
                Self::BigBlindPosted(player_id) => {
                    format_player_action(state, player_id, "posted the big blind")
                }
                Self::CardsDealtToTable(1) => "Dealt another card".to_string(),
                Self::CardsDealtToTable(count) => format!("Dealt {} cards to table", count),
                Self::RoundComplete => "Round complete".to_string(),
                Self::Winner(player_id, strength) => {
                    format_player_action(state, player_id, &format!("won with {}", strength))
                }
                Self::SplitPotWinners(players, strength) => {
                    let players = players
                        .iter()
                        .map(|player_id| {
                            state
                                .players
                                .get(player_id)
                                .map(|player| player.name.as_str())
                                .unwrap_or_default()
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("Players {} split pot with {:?}", players, strength)
                }
                Self::PaidPot(player_id, amount) => {
                    let player = state
                        .players
                        .get(player_id)
                        .map(|p| p.name.as_str())
                        .unwrap_or_default();
                    format!("Player {} won £{} from pot", player, amount)
                }
                Self::PlayerPhotoUploaded(player_id) => {
                    format_player_action(state, player_id, "added a photo")
                }
                Self::PlayerSentEmoji(player_id, emoji) => {
                    let player = state
                        .players
                        .get(player_id)
                        .map(|p| p.name.as_str())
                        .unwrap_or_default();
                    format!("Player {}: {}", player, emoji)
                }
                Self::PlayerTransferredBalance(from, to, amount) => {
                    let from = state
                        .players
                        .get(from)
                        .map(|p| p.name.as_str())
                        .unwrap_or_default();
                    let to = state
                        .players
                        .get(to)
                        .map(|p| p.name.as_str())
                        .unwrap_or_default();
                    format!("Player {} transferred £{} to {}", from, amount, to)
                }
            }
        }
    }

    pub mod emoji {
        #[derive(Debug, Clone, Copy)]
        pub struct TickerEmoji(char);

        impl std::fmt::Display for TickerEmoji {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                <char as std::fmt::Display>::fmt(&self.0, f)
            }
        }

        impl TickerEmoji {
            pub fn thumbs_up() -> Self {
                Self('👍')
            }

            pub fn thumbs_down() -> Self {
                Self('👎')
            }

            pub fn clapping() -> Self {
                Self('👏')
            }

            pub fn time() -> Self {
                Self('⏳')
            }

            pub fn thinking() -> Self {
                Self('🤔')
            }

            pub fn money() -> Self {
                Self('💰')
            }

            pub fn angry() -> Self {
                Self('😡')
            }
        }
    }

    pub struct TickerItem {
        pub seq_index: usize,
        pub start: Instant,
        pub end: Instant,
        pub payload: TickerEvent,
    }

    #[derive(Default)]
    pub struct Ticker {
        events: Vec<TickerItem>,
        counter: usize,
        last_event: Option<Instant>,
    }

    impl Ticker {
        pub fn emit(&mut self, event: TickerEvent) {
            self.emit_with_delay(event, 0);
        }

        pub fn emit_with_delay(&mut self, event: TickerEvent, delay: u64) {
            let instant = Instant::default().as_u64() + delay;
            let start = if let Some(last) = self.last_event {
                let gap = instant.saturating_sub(last.as_u64());
                let gap = gap.max(super::TICKER_ITEM_GAP_MILLISECONDS);
                last.as_u64() + gap
            } else {
                instant
            };
            let end = start + super::TICKER_ITEM_TIMEOUT_SECONDS * 1000;
            let (start, end): (Instant, Instant) = (start.into(), end.into());
            self.events.push(TickerItem {
                seq_index: self.counter,
                start,
                end,
                payload: event,
            });
            self.counter += 1;
            self.last_event = Some(start);
        }

        pub fn clear_expired_items(&mut self, now: Instant) {
            self.events.retain(|item| {
                let expired = item.end.as_u64() <= now.as_u64();
                !expired
            });
        }

        pub fn has_expired_items(&self, now: Instant) -> bool {
            self.events
                .iter()
                .any(|item| item.end.as_u64() <= now.as_u64())
        }

        pub fn len(&self) -> usize {
            self.events.len()
        }

        pub fn iter(&self) -> impl Iterator<Item = &TickerItem> {
            self.events.iter()
        }

        pub fn active_items(&self, now: Instant) -> impl Iterator<Item = &TickerItem> {
            self.events.iter().filter(move |item| {
                item.start.as_u64() <= now.as_u64() && item.end.as_u64() > now.as_u64()
            })
        }

        pub fn timeout_ms(&self) -> u64 {
            super::TICKER_ITEM_TIMEOUT_SECONDS * 1000
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn ticker_emits_events() {
            let mut ticker = Ticker::default();
            ticker.emit(TickerEvent::GameStarted);
            ticker.emit(TickerEvent::PlayerJoined(PlayerId::default()));

            assert_eq!(ticker.events.len(), 2);
        }

        #[test]
        fn ticker_clears_expired_items() {
            let mut ticker = Ticker::default();
            ticker.emit(TickerEvent::GameStarted);
            ticker.emit_with_delay(TickerEvent::PlayerJoined(PlayerId::default()), 240_000);

            assert_eq!(ticker.events.len(), 2);

            let soon = Instant::default().as_u64() + 120_000;
            ticker.clear_expired_items(Instant::from(soon));

            assert_eq!(ticker.events.len(), 1);
        }

        #[test]
        fn ticker_checks_for_expired_items() {
            let mut ticker = Ticker::default();
            ticker.emit(TickerEvent::GameStarted);
            ticker.emit_with_delay(TickerEvent::PlayerJoined(PlayerId::default()), 1000);

            let soon = Instant::default().as_u64() + 120_000;
            assert!(ticker.has_expired_items(Instant::from(soon)));
        }

        #[test]
        fn ticker_emit_delayed_events() {
            let mut ticker = Ticker::default();
            ticker.emit(TickerEvent::GameStarted);
            ticker.emit_with_delay(TickerEvent::PlayerJoined(PlayerId::default()), 1000);
            ticker.emit_with_delay(TickerEvent::PlayerJoined(PlayerId::default()), 3000);

            let now = Instant::default().as_u64();
            let active_items = ticker.active_items(Instant::from(now)).count();
            assert_eq!(active_items, 1);

            let soon = now + 2000;
            let active_items = ticker.active_items(Instant::from(soon)).count();
            assert_eq!(active_items, 2);

            let soon = now + 4000;
            let active_items = ticker.active_items(Instant::from(soon)).count();
            assert_eq!(active_items, 3);
        }

        #[test]
        fn ticker_emits_events_with_gap() {
            let mut ticker = Ticker::default();
            ticker.emit(TickerEvent::GameStarted);
            ticker.emit(TickerEvent::PlayerJoined(PlayerId::default()));
            ticker.emit(TickerEvent::PlayerJoined(PlayerId::default()));

            let now = Instant::default().as_u64();
            let active_items = ticker.active_items(Instant::from(now)).count();
            assert_eq!(active_items, 1);

            let soon = now + 2000;
            let active_items = ticker.active_items(Instant::from(soon)).count();
            assert_eq!(active_items, 3);
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
