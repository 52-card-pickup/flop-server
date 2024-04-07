use std::sync::{Arc, RwLock};

use aide::{openapi::OpenApi, transform::TransformOpenApi};
use axum::Extension;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::info;

mod cards;
mod doc_routes;
mod game;
mod models;
mod room_routes;
mod routes;
mod state;

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    // initialize aide
    aide::gen::on_error(|error| {
        println!("{error}");
    });
    aide::gen::extract_schemas(true);
    let mut api = OpenApi::default();

    // initialize state
    let v1_state = state::State::default();
    let v1_state: state::SharedState = Arc::new(RwLock::new(v1_state));
    game::spawn_game_worker(v1_state.clone());

    // build our application with a route
    let (router, states) = room_routes::multi_room_router(room_count(), routes::api_routes);
    let app = router
        .nest_api_service("/api/v2/rooms", room_routes::room_routes(states))
        .nest_api_service("/api/v1", routes::api_routes(v1_state.clone()))
        .nest_api_service("/docs", doc_routes::docs_routes())
        .finish_api_with(&mut api, api_docs)
        .layer(Extension(Arc::new(api)))
        .layer(CorsLayer::permissive());

    // run our app with hyper, listening globally on port 5000
    let addr = format!("0.0.0.0:{}", port());
    let listener = TcpListener::bind(&addr).await.unwrap();
    let docs_url = docs_url(listener.local_addr().unwrap());
    info!("listening on {}", listener.local_addr().unwrap());
    info!("Example docs are accessible at {}", docs_url);

    axum::serve(listener, app).await.unwrap();
}

fn api_docs(api: TransformOpenApi) -> TransformOpenApi {
    api.title("flop: The Party Poker Game")
        .summary("API for poker game")
        .description(include_str!("../README.md"))
}

fn docs_url(listener: std::net::SocketAddr) -> String {
    match listener {
        std::net::SocketAddr::V4(addr) if addr.ip().is_unspecified() => {
            format!("http://localhost:{}/docs", addr.port())
        }
        addr => format!("http://{}/docs", addr),
    }
}

fn port() -> u16 {
    env_or_default("PORT", 5000)
}

fn room_count() -> usize {
    env_or_default("ROOMS", 1)
}

fn env_or_default<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|val| val.parse().ok())
        .unwrap_or(default)
}
