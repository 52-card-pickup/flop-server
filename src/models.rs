use serde::{Deserialize, Serialize};

use crate::cards::{CardSuite, CardValue};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinRequest {
    pub(crate) name: String,
    pub(crate) room_code: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JoinResponse {
    pub(crate) id: String,
    pub(crate) room_code: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NewRoomRequest {
    pub(crate) name: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumeRequest {
    pub(crate) room_code: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResumeResponse {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NewRoomResponse {
    pub(crate) id: String,
    pub(crate) room_code: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CloseRoomRequest {
    pub(crate) room_code: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeekRoomRequest {
    pub(crate) room_code: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PeekRoomResponse {
    pub(crate) state: GamePhase,
    pub(crate) players_count: usize,
    pub(crate) can_resume: bool,
    pub(crate) resume_player_name: Option<String>,
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
#[serde(rename_all = "camelCase")]
pub(crate) struct PairRequest {
    pub(crate) room_code: String,
    pub(crate) screen_code: String,
}

#[derive(Debug, Deserialize, Clone, schemars::JsonSchema)]
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
    pub(crate) players_count: usize,
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
    pub(crate) room_code: Option<String>,
    pub(crate) pair_screen_code: Option<String>,
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
    pub(crate) emoji: Option<String>,
    pub(crate) photo: Option<String>,
    pub(crate) color_hue: u16,
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

pub mod headers {
    pub(crate) struct RoomCodeHeader(pub(crate) String);

    static ROOM_CODE_HEADER_NAME: std::sync::OnceLock<axum::http::HeaderName> =
        std::sync::OnceLock::new();

    impl Into<String> for RoomCodeHeader {
        fn into(self) -> String {
            self.0
        }
    }

    impl headers::Header for RoomCodeHeader {
        fn name() -> &'static axum::http::HeaderName {
            ROOM_CODE_HEADER_NAME.get_or_init(|| axum::http::HeaderName::from_static("room-code"))
        }

        fn decode<'i, I>(values: &mut I) -> Result<Self, headers::Error>
        where
            Self: Sized,
            I: Iterator<Item = &'i axum::http::HeaderValue>,
        {
            let value = values
                .next()
                .ok_or_else(|| headers::Error::invalid())?
                .to_str()
                .map_err(|_| headers::Error::invalid())?;

            Ok(Self(value.to_string()))
        }

        fn encode<E: Extend<axum::http::HeaderValue>>(&self, values: &mut E) {
            match axum::http::HeaderValue::from_str(&self.0) {
                Ok(value) => values.extend(std::iter::once(value)),
                Err(_) => values.extend(std::iter::once(axum::http::HeaderValue::from_static(""))),
            }
        }
    }
}
