use tracing::warn;

mod common;

use common::{client, fixtures, server};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_should_return_default_room_state() {
    let (server, handle) = server::new_mock_app_server();

    let big_screen = client::get_big_screen(&server, None).await;
    assert_eq!(big_screen.state, "idle");
    handle.abort().await;
}

#[tokio::test]
async fn it_should_start_game_and_play_2p_until_end() {
    let (server, handle) = server::new_mock_app_server();

    let game = fixtures::start_full_game(&server, 2).await;
    fixtures::play_rounds_until_winner(&server, &game).await;
    handle.abort().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_should_start_game_and_play_2p_until_end_over_http() {
    let (server, handle) = server::new_http_app_server();

    let game = fixtures::start_full_game(&server, 2).await;
    fixtures::play_rounds_until_winner(&server, &game).await;
    handle.abort().await;
}

#[tokio::test]
async fn it_should_start_game_and_play_3p_until_end() {
    let (server, handle) = server::new_mock_app_server();

    let game = fixtures::start_full_game(&server, 3).await;
    fixtures::play_rounds_until_winner(&server, &game).await;
    handle.abort().await;
}

#[tokio::test]
async fn it_should_remove_players_from_game_on_leave() {
    let (server, handle) = server::new_mock_app_server();

    //  start game with 3 players
    let mut game = fixtures::start_full_game(&server, 3).await;
    fixtures::play_rounds_until_winner(&server, &game).await;

    // player 1 leaves
    let leaving_player_id = game.player_ids.remove(0);
    client::leave_room(&server, &leaving_player_id).await;

    client::requests::get_little_screen(&server, &leaving_player_id)
        .expect_failure()
        .await
        .assert_status_not_found();

    client::start_game(&server, &game.room_code).await;
    fixtures::play_round(&server, &game).await;

    // player 3 leaves, only 1 player left
    let leaving_player_id = game.player_ids.remove(1);
    client::leave_room(&server, &leaving_player_id).await;

    client::requests::get_little_screen(&server, &leaving_player_id)
        .expect_failure()
        .await
        .assert_status_not_found();

    let status = client::get_big_screen(&server, Some(&game.room_code))
        .await
        .state;

    // the game should be stopped and wait for more players
    assert_eq!(status, "waiting");

    handle.abort().await;
}

#[tokio::test]
async fn it_should_not_show_card_of_rejoining_players() {
    let (server, handle) = server::new_mock_app_server();

    //  start game with 3 players
    let mut game = fixtures::start_full_game(&server, 3).await;
    fixtures::play_rounds_until_winner(&server, &game).await;

    // player 1 leaves
    let leaving_player_id = game.player_ids.remove(0);
    client::leave_room(&server, &leaving_player_id).await;

    // player 1 rejoins
    let rejoining_player_apid = game.player_apids.get(&leaving_player_id).unwrap();
    let rejoining_player =
        client::resume_session(&server, rejoining_player_apid, &game.room_code).await;
    assert_eq!(rejoining_player.player_id, leaving_player_id);

    let big_screen = client::get_big_screen(&server, Some(&game.room_code)).await;
    let rejoining_player_idx = big_screen
        .players
        .iter()
        .position(|p| p["name"] == "player1")
        .unwrap();

    let completed_game = big_screen.raw["completed"]
        .as_object()
        .expect("completed is not an object");

    let player_cards = completed_game["playerCards"]
        .as_array()
        .expect("player_cards is not an array");

    let rejoining_player_cards = &player_cards[rejoining_player_idx];
    assert_eq!(rejoining_player_cards, &serde_json::Value::Null);

    handle.abort().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_should_start_game_and_play_3p_until_end_over_http() {
    let (server, handle) = server::new_http_app_server();

    let game = fixtures::start_full_game(&server, 3).await;
    fixtures::play_rounds_until_winner(&server, &game).await;
    handle.abort().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_should_start_game_two_simultaneous_games_and_play_3p_until_end() {
    let (server, handle) = server::new_mock_app_server();

    let game1 = fixtures::start_full_game(&server, 3).await;
    let game2 = fixtures::start_full_game(&server, 3).await;

    assert_ne!(game1.room_code, game2.room_code);

    for i in 0..3 {
        warn!("Playing game #{}", i + 1);
        if i > 0 {
            client::start_game(&server, &game1.room_code).await;
            client::start_game(&server, &game2.room_code).await;
        }
        tokio::join!(
            fixtures::play_rounds_until_winner(&server, &game1),
            fixtures::play_rounds_until_winner(&server, &game2),
        );
    }

    handle.abort().await;
}

#[ignore = "performance test - can be moved to benchmarks"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_play_4p_game_many_times() {
    let (server, handle) = server::new_mock_app_server();
    let game = fixtures::start_full_game(&server, 4).await;

    for i in 0..1000 {
        warn!("Playing game #{}", i);
        if i % 50 == 0 {
            warn!("Getting room state...");
            let shared_state = handle.state();
            let state = shared_state
                .get_room(&game.room_code.parse().unwrap())
                .await
                .unwrap();
            let state = state.read().await;
            warn!("Dumping room state...");
            warn!("state.status: {:?}", state.status);
            warn!("state.disposed: {:?}", state.disposed);
            warn!("state.last_update: {:?}", state.last_update);
            warn!("state.players: {:#?}", state.players);
            warn!("state.round: {:#?}", state.round);
        }
        if i > 0 {
            client::start_game(&server, &game.room_code).await;
        }
        fixtures::play_rounds_until_winner(&server, &game).await;
    }

    for (i, player_id) in game.player_ids.iter().enumerate() {
        let little_screen = client::get_little_screen(&server, player_id).await;
        warn!("player{} has balance {}", i + 1, little_screen.balance);
    }

    handle.abort().await;
}
