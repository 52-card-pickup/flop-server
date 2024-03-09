# Flop: The Async Poker Game Server :spades: :hearts: :clubs: :diamonds:

Hello poker enthusiasts, welcome to Flop! We've whipped up a fantastic, super-responsive, and rust-y poker game server just for you. This is the bedrock for our mobile game "Flop" and the companion big screen application "Flop-Bigscreen". 

## Architecture :construction:

Our poker server is built on the robust, performance-driven Rust language utilizing Tokio for the async network programming magic. We're leveraging libraries like `Aide` for initializing state, routes, and the OpenAPI; `Axum` for RESTful goodness, and `Tracing` for useful logs. All dealt in around 500 lines of clean, uncomplicated code. 

Our server is threaded for massively parallel play, safely permitting mutable shared state with the `Arc` and `RwLock` abstractions. 

## Routes :world_map:

The server handles several routes: 

- `/api/v1/room` : View the game room status - ['big' players only :sunglasses:]
- `/api/v1/room/close` : Close the room 
- `/api/v1/room/reset` : Reset the game state