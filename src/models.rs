use serde::{Deserialize, Serialize};

use crate::cards::{CardSuite, CardValue};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinRequest {
    pub(crate) name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinResponse {
    pub(crate) id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayRequest {
    pub(crate) player_id: String,
    pub(crate) stake: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GamePlayerState {
    pub(crate) state: GamePhase,
    pub(crate) balance: u64,
    pub(crate) cards: ((CardSuite, CardValue), (CardSuite, CardValue)),
    pub(crate) your_turn: bool,
    pub(crate) last_update: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameClientState {
    pub(crate) state: GamePhase,
    pub(crate) players: Vec<GameClientPlayer>,
    pub(crate) pot: u64,
    pub(crate) cards: Vec<(CardSuite, CardValue)>,
    pub(crate) last_update: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameClientPlayer {
    pub(crate) name: String,
    pub(crate) balance: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GamePhase {
    Offline,
    Idle,
    Waiting,
    Playing,
    Complete,
}
