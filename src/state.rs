use std::{collections::HashMap, sync::Arc};

use crate::cards::{self, Card, Deck};

use axum::body::Bytes;
use dt::Instant;
use tokio::sync::RwLock;

pub use id::PlayerId;
pub use ticker::TickerEvent;

use self::players::Players;

pub type RoomState = Arc<RwLock<State>>;

#[derive(Default, Clone)]
pub struct SharedState {
    states: Arc<std::sync::RwLock<HashMap<room::RoomCode, RoomState>>>,
    registry: Arc<RwLock<room::RoomRegistry>>,
    default_config: Arc<std::sync::RwLock<Option<config::RoomConfig>>>,
}

impl SharedState {
    pub async fn get(&self, player_id: &PlayerId) -> Option<RoomState> {
        let registry = self.registry.read().await;
        let room_code = registry.get_room(&player_id)?;

        self.get_room(&room_code).await
    }

    pub async fn get_room(&self, room: &room::RoomCode) -> Option<RoomState> {
        let exisiting_room_state = {
            let rooms = self.states.read().unwrap();
            rooms.get(room).cloned()
        };

        let state = match exisiting_room_state {
            Some(state) => state,
            None => {
                let registry = self.registry.read().await;
                if !registry.room_exists(room) {
                    return None;
                }

                let mut rooms = self.states.write().unwrap();
                let state = Arc::new(RwLock::new(self.default_state()));
                rooms.insert(room.clone(), state.clone());
                state
            }
        };
        Some(state.clone())
    }

    pub async fn get_default_room(&self) -> Option<RoomState> {
        let room_code = self.get_default_room_code().await?;
        self.get_room(&room_code).await
    }

    pub async fn get_default_room_code(&self) -> Option<room::RoomCode> {
        let rooms = self.registry.read().await;
        Some(rooms.get_default_room().cloned()?)
    }

    pub async fn create_room(&self, player_id: &PlayerId) -> room::RoomCode {
        let mut rooms = self.registry.write().await;
        let code = rooms.create_room(player_id);
        let state = Arc::new(RwLock::new(self.default_state()));

        let mut inner = self.states.write().unwrap();
        inner.insert(code.clone(), state);

        code
    }

    pub async fn join_room(
        &self,
        player_id: &PlayerId,
        room_code: Option<&room::RoomCode>,
    ) -> Result<room::RoomCode, ()> {
        let mut rooms = self.registry.write().await;
        match room_code.cloned() {
            Some(code) => {
                rooms.insert_player(player_id, &code)?;
                Ok(code)
            }
            None => {
                let code = rooms.get_or_create_default_room(player_id);
                Ok(code)
            }
        }
    }

    pub async fn remove(&self, player_id: &PlayerId) {
        let mut registry = self.registry.write().await;
        if let Some(room_code) = registry.remove_room(player_id) {
            let mut rooms = self.states.write().unwrap();
            rooms.remove(&room_code);
        }
    }

    pub async fn iter(&self) -> impl Iterator<Item = RoomState> {
        let rooms = self.states.read().unwrap();
        rooms.values().cloned().collect::<Vec<_>>().into_iter()
    }

    pub async fn iter_key_values(&self) -> impl Iterator<Item = (room::RoomCode, RoomState)> {
        let rooms = self.states.read().unwrap();
        rooms
            .iter()
            .map(|(code, state)| (code.clone(), state.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub async fn cleanup(&self) {
        let mut rooms = self.states.write().unwrap().clone();
        let mut to_remove = Vec::new();

        for (room_code, state) in rooms.iter() {
            let state = state.read().await;

            if state.disposed {
                continue;
            }

            let now = Instant::default().as_u64();
            let last_update = state.last_update.as_u64();
            let room_expires_at = last_update + GAME_IDLE_TIMEOUT_SECONDS * 1000;

            if room_expires_at < now {
                to_remove.push(room_code.clone());
            }
        }

        if to_remove.is_empty() {
            return;
        }

        let mut registry = self.registry.write().await;
        for room_code in to_remove {
            if let Some(state) = rooms.remove(&room_code) {
                let mut state = state.write().await;
                state.disposed = true;

                for player_id in state.players.keys() {
                    registry.remove_room(player_id);
                }
            }
        }
    }

    pub fn set_default_config(&self, config: config::RoomConfig) {
        let mut default_config = self.default_config.write().unwrap();
        *default_config = Some(config);
    }

    fn default_state(&self) -> State {
        match self.default_config.read() {
            Ok(config) => {
                let config = config.as_ref().cloned().unwrap_or_default();
                let mut state = State::default();
                state.config = config;
                state
            }
            Err(_) => State::default(),
        }
    }
}

pub mod room {
    use std::{
        collections::{HashMap, HashSet},
        str::FromStr,
    };

    use rand::Rng;
    use tracing::info;

    use crate::state::{PlayerId, ROOM_CODE_LENGTH};

    #[derive(Default)]
    pub struct RoomRegistry {
        player_rooms: HashMap<PlayerId, RoomCode>,
        rooms: HashSet<RoomCode>,
        default: Option<RoomCode>,
    }

    impl RoomRegistry {
        pub fn create_room(&mut self, player_id: &PlayerId) -> RoomCode {
            let room = RoomCode::default();
            self.rooms.insert(room.clone());
            self.player_rooms.insert(player_id.clone(), room.clone());
            room
        }

        pub fn get_or_create_default_room(&mut self, player_id: &PlayerId) -> RoomCode {
            match self.default.clone() {
                Some(room) => {
                    self.insert_player(player_id, &room).unwrap();
                    room
                }
                None => {
                    let room = self.create_room(player_id);
                    info!("Created default room: {:?}", &room);
                    self.default = Some(room.clone());
                    room
                }
            }
        }

        pub fn insert_player(&mut self, player_id: &PlayerId, room: &RoomCode) -> Result<(), ()> {
            if !self.rooms.contains(&room) {
                return Err(());
            }
            self.player_rooms.insert(player_id.clone(), room.clone());
            Ok(())
        }

        pub fn get_room(&self, player_id: &PlayerId) -> Option<&RoomCode> {
            self.player_rooms.get(player_id)
        }

        pub fn get_default_room(&self) -> Option<&RoomCode> {
            self.default.as_ref()
        }

        pub fn remove_room(&mut self, player_id: &PlayerId) -> Option<RoomCode> {
            let code = self.player_rooms.remove(player_id)?;
            if self.player_rooms.values().any(|c| c == &code) {
                return None;
            }

            self.rooms.remove(&code);

            if self.default.as_ref() == Some(&code) {
                self.default = None;
            }
            Some(code)
        }

        pub fn room_exists(&self, room: &RoomCode) -> bool {
            self.rooms.contains(room)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct RoomCode(String);

    impl ToString for RoomCode {
        #[inline]
        fn to_string(&self) -> String {
            <String as ToString>::to_string(&self.0)
        }
    }

    impl FromStr for RoomCode {
        type Err = ();

        fn from_str(s: &str) -> Result<Self, Self::Err> {
            let char_count = s.chars().count();
            if char_count != ROOM_CODE_LENGTH {
                return Err(());
            }
            if !s.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(());
            }

            Ok(Self(s.to_ascii_uppercase()))
        }
    }

    impl Default for RoomCode {
        fn default() -> Self {
            let mut rng = rand::thread_rng();
            let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ".as_bytes();
            let code: String = (0..ROOM_CODE_LENGTH)
                .map(|_| {
                    let idx = rng.gen_range(0..alphabet.len());
                    alphabet[idx] as char
                })
                .collect();

            Self(code)
        }
    }
}

pub const STARTING_BALANCE: u64 = 1000;
pub const SMALL_BLIND: u64 = 10;
pub const BIG_BLIND: u64 = SMALL_BLIND * 2;
pub const PLAYER_EMOJI_TIMEOUT_SECONDS: u64 = 5;
pub const TICKER_ITEM_TIMEOUT_SECONDS: u64 = 10;
pub const TICKER_ITEM_GAP_MILLISECONDS: u64 = 500;
pub const PLAYER_TURN_TIMEOUT_SECONDS: u64 = 60;
pub const GAME_IDLE_TIMEOUT_SECONDS: u64 = 300;
pub const ROOM_CODE_LENGTH: usize = 4;
pub const MAX_PLAYERS: usize = 10;

#[derive(Debug, Default)]
pub struct State {
    pub players: Players,
    pub round: Round,
    pub last_update: dt::SignalInstant,
    pub ticker: ticker::Ticker,
    pub status: GameStatus,
    pub config: config::RoomConfig,
    pub disposed: bool,
}

#[derive(Debug, Default)]
pub struct Round {
    pub pot: u64,
    pub deck: Deck,
    pub cards_on_table: Vec<Card>,
    pub players_turn: Option<PlayerId>,
    pub raises: Vec<(PlayerId, u64)>,
    pub calls: Vec<(PlayerId, u64)>,
    pub completed: Option<CompletedRound>,
}

impl Into<RoomState> for State {
    fn into(self) -> RoomState {
        Arc::new(RwLock::new(self))
    }
}

#[derive(Clone)]
pub struct PlayerPhoto(pub Arc<Bytes>, pub token::Token);

impl std::fmt::Debug for PlayerPhoto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PlayerPhoto").field(&self.1).finish()
    }
}

#[derive(Debug, Clone)]
pub struct Player {
    pub name: String,
    pub id: PlayerId,
    pub emoji: Option<(ticker::emoji::TickerEmoji, dt::Instant)>,
    pub funds_token: token::Token,
    pub balance: u64,
    pub stake: u64,
    pub folded: bool,
    pub photo: Option<PlayerPhoto>,
    pub ttl: Option<dt::Instant>,
    pub apid: String,
    pub cards: (Card, Card),
}

#[derive(Debug, Clone)]
pub struct CompletedRound {
    pub winners: Vec<RoundWinner>,
    pub best_hand: Option<(Vec<PlayerId>, cards::HandStrength)>,
    pub hide_cards: bool,
}

#[derive(Debug, Clone)]
pub struct RoundWinner {
    pub player_id: PlayerId,
    pub hand: Option<cards::HandStrength>,
    pub winnings: u64,
    pub total_pot_winnings: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GameStatus {
    #[default]
    Joining,
    Playing,
    Complete,
    Idle,
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

    impl PlayerId {
        pub fn new_unchecked(player_id: &str) -> Self {
            Self(player_id.to_string())
        }
    }

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

    #[derive(Debug, Clone, PartialEq, Eq)]
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
        use std::fmt::Debug;
        use std::future::Future;
        use std::sync::{Arc, Mutex};

        use tokio::sync::oneshot;

        use super::Instant;

        #[derive(Clone, Default)]
        pub struct SignalInstant(Instant, Arc<Mutex<Vec<oneshot::Sender<Instant>>>>);

        impl Debug for SignalInstant {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

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
    static TICKER_DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    #[derive(Debug, Clone)]
    pub enum TickerEvent {
        GameStarted,
        PlayerJoined(PlayerId),
        PlayerTurnTimeout(String),
        PlayerLeft(String),
        PlayerResumed(PlayerId),
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
                match state
                    .players
                    .get(player_id)
                    .or_else(|| state.players.get_dormant(player_id))
                {
                    Some(player) => return format!("Player {} {}", player.name, action),
                    None => return format!("Previous player {}", player_id),
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
                Self::PlayerLeft(player_name) => {
                    format!("Player {} left the game", player_name)
                }
                Self::PlayerResumed(player_id) => {
                    format_player_action(state, player_id, "rejoined the game")
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

    #[derive(Debug)]
    pub struct TickerItem {
        pub seq_index: usize,
        pub start: Instant,
        pub end: Instant,
        pub payload: TickerEvent,
    }

    #[derive(Debug, Default)]
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

    pub(crate) fn is_disabled() -> bool {
        *TICKER_DISABLED.get_or_init(|| std::env::var("DISABLE_TICKER").is_ok())
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

    #[derive(Debug)]
    struct DormantPlayer(Player);

    #[derive(Default, Debug)]
    pub struct Players(VecDeque<(PlayerId, Player)>, Vec<DormantPlayer>);

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
            let player = self.0.remove(idx).map(|(_, p)| p)?;
            self.1.push(DormantPlayer(player.clone()));

            Some(player)
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

        pub fn promote_dormant(&mut self, apid: &str) -> Option<Player> {
            let player = self.peek_dormant(apid)?;
            let idx = self
                .1
                .iter()
                .position(|DormantPlayer(d)| d.id == player.id)?;
            let dormant = self.1.remove(idx);
            self.0.push_back((dormant.0.id.clone(), dormant.0.clone()));
            Some(dormant.0)
        }

        pub fn peek_dormant(&self, apid: &str) -> Option<&Player> {
            self.1.iter().rev().find_map(
                |DormantPlayer(d)| {
                    if d.apid == apid {
                        Some(d)
                    } else {
                        None
                    }
                },
            )
        }

        pub fn get_dormant(&self, player_id: &PlayerId) -> Option<&Player> {
            self.1.iter().find_map(
                |DormantPlayer(d)| {
                    if d.id == *player_id {
                        Some(d)
                    } else {
                        None
                    }
                },
            )
        }

        pub fn get_non_dormant(&self, apid: &str) -> Option<&Player> {
            self.0
                .iter()
                .find_map(|(_, p)| if p.apid == apid { Some(p) } else { None })
        }
    }
}

pub mod config {
    use super::*;

    #[derive(Debug, Clone)]
    pub struct RoomConfig {
        small_blind: u64,
        max_players: usize,
        starting_balance: u64,
        ticker_disabled: bool,
    }

    impl RoomConfig {
        pub fn small_blind(&self) -> u64 {
            self.small_blind
        }

        pub fn big_blind(&self) -> u64 {
            self.small_blind * 2
        }

        pub fn with_small_blind(mut self, small_blind: u64) -> Self {
            assert!(small_blind > 0);
            assert!(small_blind < self.starting_balance);
            self.small_blind = small_blind;
            self
        }

        pub fn max_players(&self) -> usize {
            self.max_players
        }

        pub fn with_max_players(mut self, max_players: usize) -> Self {
            assert!(max_players > 0);
            self.max_players = max_players.min(MAX_PLAYERS);
            self
        }

        pub fn starting_balance(&self) -> u64 {
            self.starting_balance
        }

        pub fn with_starting_balance(mut self, starting_balance: u64) -> Self {
            assert!(starting_balance > 0);
            assert!(starting_balance > self.small_blind);
            self.starting_balance = starting_balance;
            self
        }

        pub fn ticker_disabled(&self) -> bool {
            self.ticker_disabled
        }

        pub fn with_ticker_enabled(mut self) -> Self {
            self.ticker_disabled = false;
            self
        }

        pub fn with_ticker_disabled(mut self) -> Self {
            self.ticker_disabled = true;
            self
        }
    }

    impl Default for RoomConfig {
        fn default() -> Self {
            Self {
                small_blind: SMALL_BLIND,
                max_players: MAX_PLAYERS,
                starting_balance: STARTING_BALANCE,
                ticker_disabled: ticker::is_disabled(),
            }
        }
    }
}
