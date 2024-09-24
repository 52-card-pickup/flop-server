pub mod fixtures {
    use super::*;
    use axum_test::TestServer;
    use client::models::LittleScreen;
    use state::*;

    pub async fn start_full_game(server: &TestServer, num_players: usize) -> StartedGame {
        assert!(num_players >= 2);

        // Player 1 creates a room and joins.
        let player1 = client::create_room(server, "player1").await;
        let room_code = player1.room_code;

        // Other players join.
        let mut player_ids = vec![player1.player_id];
        for i in 2..=num_players {
            let player = client::join_room(server, &format!("player{}", i), &room_code).await;
            player_ids.push(player.player_id);
        }

        client::start_game(server, &room_code).await;

        StartedGame {
            room_code,
            player_ids,
        }
    }

    pub async fn play_rounds_until_winner(server: &TestServer, game: &StartedGame) {
        let mut round_index = 0;
        while !game_over(server, game).await {
            play_round(server, game).await;

            round_index += 1;
            assert!(round_index < 10, "Game did not end");
        }
    }

    pub async fn play_round(server: &TestServer, game: &StartedGame) {
        let max_iters = game.player_ids.len() * 2;
        let initial_cards_on_table = cards_on_table(server, game).await;

        for _ in 0..max_iters {
            let active_player = get_active_player(server, game).await;

            if active_player.call_amount > active_player.current_round_stake {
                client::player_call(server, &active_player.player_id).await;
            } else {
                client::player_check(server, &active_player.player_id).await;
            }

            if initial_cards_on_table != cards_on_table(server, game).await {
                return;
            }
            if game_over(server, game).await {
                return;
            }
        }

        panic!("Round did not end");
    }

    async fn get_active_player(server: &TestServer, game: &StartedGame) -> LittleScreen {
        for player_id in &game.player_ids {
            let little_screen = client::get_little_screen(server, player_id).await;
            if little_screen.your_turn {
                return little_screen;
            }
        }

        panic!("No active player found");
    }

    async fn cards_on_table(server: &TestServer, game: &StartedGame) -> usize {
        client::get_big_screen(server, Some(&game.room_code))
            .await
            .raw["cards"]
            .as_array()
            .unwrap()
            .len()
    }

    async fn game_over(server: &TestServer, game: &StartedGame) -> bool {
        let big_screen = client::get_big_screen(server, Some(&game.room_code)).await;
        big_screen.state == "complete" || big_screen.state == "waiting"
    }

    mod state {
        pub struct StartedGame {
            pub room_code: String,
            pub player_ids: Vec<String>,
        }
    }
}

pub mod server {
    use axum_test::{TestServer, TestServerConfig};
    use tracing::info;

    use flop_server::{game, state};

    const PORT: std::sync::OnceLock<Option<u16>> = std::sync::OnceLock::new();

    pub fn new_mock_app_server() -> (TestServer, WorkerHandle) {
        new_app_server(false)
    }

    pub fn new_http_app_server() -> (TestServer, WorkerHandle) {
        new_app_server(true)
    }

    pub fn new_app_server(use_http: bool) -> (TestServer, WorkerHandle) {
        _ = tracing_subscriber::fmt::try_init();

        info!("Starting test server");

        let state = state::SharedState::default();
        state.set_default_config(
            state::config::RoomConfig::default()
                .with_ticker_disabled()
                .with_starting_balance(10_000),
        );
        let handle = game::spawn_game_worker(state.clone());
        let app = flop_server::create_application(state.clone());

        info!("Test server initialized");

        let builder = TestServerConfig::builder().expect_success_by_default();

        let builder = match (use_http, PORT.get_or_init(maybe_api_port)) {
            (true, Some(port)) => builder.http_transport_with_ip_port(None, Some(*port)),
            (true, None) => builder.http_transport(),
            (false, _) => builder.mock_transport(),
        };

        let test_server = builder.build_server(app).unwrap();

        info!(
            "Test server started, listening on port {}",
            maybe_api_port().map_or_else(|| "<none>".to_string(), |port| port.to_string())
        );

        (test_server, WorkerHandle(handle, state))
    }

    fn maybe_api_port() -> Option<u16> {
        std::env::var("PORT")
            .ok()
            .and_then(|port| port.parse().ok())
    }

    pub struct WorkerHandle(tokio::task::JoinHandle<()>, state::SharedState);

    impl WorkerHandle {
        pub async fn abort(self) {
            self.0.abort();
            assert!(self.0.await.unwrap_err().is_cancelled());
        }

        pub fn state(&self) -> &state::SharedState {
            &self.1
        }
    }
}

pub mod client {
    use axum_test::TestServer;
    use models::*;
    use serde_json::json;

    type Json = serde_json::Value;

    pub async fn get_big_screen(server: &TestServer, room_code: Option<&str>) -> BigScreen {
        let request = match room_code {
            Some(room_code) => requests::get_big_screen_with_room_code(server, room_code),
            None => requests::get_big_screen(server),
        };
        let response = request.await.json::<Json>();

        BigScreen {
            raw: response.clone(),
            state: response["state"].as_str().unwrap().to_string(),
            players: response["players"].as_array().unwrap().to_vec(),
        }
    }

    pub async fn get_little_screen(server: &TestServer, player_id: &str) -> LittleScreen {
        let response = requests::get_little_screen(server, player_id)
            .await
            .json::<Json>();

        LittleScreen {
            raw: response.clone(),
            player_id: player_id.to_string(),
            your_turn: response["yourTurn"].as_bool().unwrap(),
            balance: response["balance"].as_u64().unwrap(),
            call_amount: response["callAmount"].as_u64().unwrap(),
            min_raise_to: response["minRaiseTo"].as_u64().unwrap(),
            current_round_stake: response["currentRoundStake"].as_u64().unwrap(),
        }
    }

    pub async fn leave_room(server: &TestServer, player_id: &str) {
        requests::leave_room(server, player_id).await;
    }

    pub async fn create_room(server: &TestServer, player_name: &str) -> CreatedRoom {
        let response = requests::create_room(server)
            .json(&json!({
                "name": player_name,
            }))
            .await
            .json::<Json>();

        CreatedRoom {
            raw: response.clone(),
            room_code: response["roomCode"].as_str().unwrap().to_string(),
            player_id: response["id"].as_str().unwrap().to_string(),
        }
    }

    pub async fn join_room(server: &TestServer, player_name: &str, room_code: &str) -> JoinedRoom {
        let response = requests::join_room(server)
            .json(&json!({
                "name": player_name,
                "roomCode": room_code,
            }))
            .await
            .json::<Json>();

        JoinedRoom {
            raw: response.clone(),
            player_id: response["id"].as_str().unwrap().to_string(),
        }
    }

    pub async fn start_game(server: &TestServer, room_code: &str) {
        requests::start_game(server)
            .json(&json!({
                "roomCode": room_code,
            }))
            .await;
    }

    pub async fn player_check(server: &TestServer, player_id: &str) {
        requests::play_turn(server)
            .json(&json!({
                "playerId": player_id,
                "stake": 0,
                "action": "check",
            }))
            .await;
    }

    pub async fn player_call(server: &TestServer, player_id: &str) {
        requests::play_turn(server)
            .json(&json!({
                "playerId": player_id,
                "stake": 0,
                "action": "call",
            }))
            .await;
    }

    pub mod requests {
        use axum_test::{TestRequest, TestServer};

        pub fn get_big_screen(server: &TestServer) -> TestRequest {
            server.get("/api/v1/room")
        }
        pub fn get_big_screen_with_room_code(server: &TestServer, room_code: &str) -> TestRequest {
            server
                .get("/api/v1/room")
                .add_header("room-code", room_code)
        }
        pub fn get_little_screen(server: &TestServer, player_id: &str) -> TestRequest {
            server.get(&format!("/api/v1/player/{}", player_id))
        }
        pub fn leave_room(server: &TestServer, player_id: &str) -> TestRequest {
            server.post(&format!("/api/v1/player/{}/leave", player_id))
        }
        pub fn create_room(server: &TestServer) -> TestRequest {
            server.post("/api/v1/new")
        }
        pub fn join_room(server: &TestServer) -> TestRequest {
            server.post("/api/v1/join")
        }
        pub fn start_game(server: &TestServer) -> TestRequest {
            server.post("/api/v1/room/close")
        }
        pub fn play_turn(server: &TestServer) -> TestRequest {
            server.post("/api/v1/play")
        }
    }

    pub mod models {
        use serde_json::Value;

        pub struct BigScreen {
            pub raw: Value,
            pub state: String,
            pub players: Vec<Value>,
        }
        pub struct LittleScreen {
            pub raw: Value,
            pub player_id: String,
            pub your_turn: bool,
            pub balance: u64,
            pub call_amount: u64,
            pub min_raise_to: u64,
            pub current_round_stake: u64,
        }
        pub struct CreatedRoom {
            pub raw: Value,
            pub room_code: String,
            pub player_id: String,
        }
        pub struct JoinedRoom {
            pub raw: Value,
            pub player_id: String,
        }
    }
}
