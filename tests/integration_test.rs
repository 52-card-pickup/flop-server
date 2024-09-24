use tracing::warn;

mod common;

use common::{client, fixtures, server};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn it_should_create_room_and_wait_for_players_over_http() {
    let (server, handle) = server::new_mock_app_server();

    let big_screen = client::get_big_screen(&server, None).await;
    assert_eq!(big_screen.state, "waiting");
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
async fn it_should_remove_player_from_game() {
    let (server, handle) = server::new_mock_app_server();

    let mut game = fixtures::start_full_game(&server, 3).await;
    fixtures::play_rounds_until_winner(&server, &game).await;

    client::leave_room(&server, &game.player_ids[0]).await;

    let leaving_player_id = game.player_ids.remove(0);
    client::requests::get_little_screen(&server, &leaving_player_id)
        .expect_failure()
        .await
        .assert_status_not_found();

    client::start_game(&server, &game.room_code).await;
    fixtures::play_rounds_until_winner(&server, &game).await;

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
