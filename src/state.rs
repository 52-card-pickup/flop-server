use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use crate::cards::{Card, Deck};

pub type SharedState = Arc<RwLock<State>>;

#[derive(Default, Clone)]
pub struct State {
    pub cards_on_table: Vec<Card>,
    pub players: BTreeMap<String, Player>,
    pub pot: u64,
    pub deck: Deck,
    pub players_turn: Option<String>,
    pub last_update: u64,
    pub status: GameStatus,
}

#[derive(Clone)]
pub struct Player {
    pub name: String,
    pub id: String,
    pub balance: u64,
    pub stake: u64,
    pub cards: (Card, Card),
}

#[derive(Default, Clone)]
pub enum GameStatus {
    #[default]
    Joining,
    Playing,
    Complete,
}
