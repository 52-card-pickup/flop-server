use serde::{Deserialize, Serialize};

use crate::cards::{CardSuite, CardValue};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinRequest {
    pub(crate) name: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinResponse {
    pub(crate) id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayRequest {
    pub(crate) player_id: String,
    pub(crate) stake: u64,
    pub(crate) action: PlayAction,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) enum PlayAction {
    Check,
    Call,
    Raise,
    Fold,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GamePlayerState {
    pub(crate) state: GamePhase,
    pub(crate) balance: u64,
    pub(crate) cards: ((CardSuite, CardValue), (CardSuite, CardValue)),
    pub(crate) your_turn: bool,
    pub(crate) call_amount: u64,
    pub(crate) min_raise_by: u64,
    pub(crate) turn_expires_dt: Option<u64>,
    pub(crate) last_update: u64,
    pub(crate) current_round_stake: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameClientRoom {
    pub(crate) state: GamePhase,
    pub(crate) players: Vec<GameClientPlayer>,
    pub(crate) pot: u64,
    pub(crate) cards: Vec<(CardSuite, CardValue)>,
    pub(crate) completed: Option<CompletedGame>,
    pub(crate) last_update: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompletedGame {
    pub(crate) winner_idx: usize,
    pub(crate) winning_hand: String,
    pub(crate) player_cards: Vec<((CardSuite, CardValue), (CardSuite, CardValue))>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameClientPlayer {
    pub(crate) name: String,
    pub(crate) balance: u64,
    pub(crate) folded: bool,
    pub(crate) turn_expires_dt: Option<u64>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) enum GamePhase {
    Offline,
    Idle,
    Waiting,
    Playing,
    Complete,
}
