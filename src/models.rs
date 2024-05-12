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
    RaiseTo,
    Fold,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayerSendRequest {
    pub(crate) message: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayerAccountsResponse {
    pub(crate) accounts: Vec<PlayerAccount>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlayerAccount {
    pub(crate) name: String,
    pub(crate) account_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransferRequest {
    pub(crate) amount: u64,
    pub(crate) to: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PollQuery {
    pub since: Option<u64>,
    pub timeout: Option<u64>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GamePlayerState {
    pub(crate) state: GamePhase,
    pub(crate) balance: u64,
    pub(crate) cards: ((CardSuite, CardValue), (CardSuite, CardValue)),
    pub(crate) your_turn: bool,
    pub(crate) call_amount: u64,
    pub(crate) min_raise_to: u64,
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
    pub(crate) ticker: Option<String>,
    pub(crate) last_update: u64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompletedGame {
    pub(crate) winner_name: Option<String>,
    pub(crate) winning_hand: Option<String>,
    pub(crate) player_cards: Vec<Option<((CardSuite, CardValue), (CardSuite, CardValue))>>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GameClientPlayer {
    pub(crate) name: String,
    pub(crate) balance: u64,
    pub(crate) folded: bool,
    pub(crate) photo: Option<String>,
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
