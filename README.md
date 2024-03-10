# `flop-server` The Party Poker Game :spades: :hearts: :clubs: :diamonds:

Hello poker enthusiasts, welcome to `flop`! 
Play poker with your friends on your mobile devices, while watch the game unfold on the big screen. Enjoy the thrill of the game like never before!
This is the bedrock for our mobile game `flop-littlescreen` and the companion big screen application `flop-bigscreen`. 

## Architecture :construction:

Our poker server is built on the robust, performance-driven Rust language utilizing Tokio for the async network programming magic. We're leveraging libraries like `Axum` for RESTful goodness; `Aide` for setting up some OpenAPI docs; and `Tracing` for useful logs.

## Routes :world_map:

The server handles several routes: 

- GET `/api/v1/room` : View the game room state - for the big screen app
- GET `/api/v1/player/:player_id` : View the player state - for the mobile app
- POST `/api/v1/room/close` : Close the game room
- POST `/api/v1/room/reset` : Reset the game room
- POST `/api/v1/join` : Join the game room
- POST `/api/v1/play` : Play you turn in a round

Documentation for these routes is available via the OpenAPI spec at `/docs`.