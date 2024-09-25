use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
};

use aide::{axum::ApiRouter, openapi::OpenApi, transform::TransformOpenApi};
use axum::Extension;
use tower_http::cors::CorsLayer;
use tracing::info;

mod cards;
mod doc_routes;
mod game;
mod models;
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
    let state = state::SharedState::default();
    game::spawn_game_worker(state.clone());

    // build our application with a route
    let app = ApiRouter::new()
        .nest_api_service("/api/v1", routes::api_routes(state.clone()))
        .nest_api_service("/docs", doc_routes::docs_routes(state.clone()))
        .finish_api_with(&mut api, api_docs)
        .layer(Extension(Arc::new(api)))
        .layer(CorsLayer::permissive());

    // run our app with hyper, listening globally - by default on port 5000
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), api_port());
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();

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

fn api_port() -> u16 {
    std::env::var("PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(5000)
}

fn docs_url(listener: std::net::SocketAddr) -> String {
    match listener {
        std::net::SocketAddr::V4(addr) if addr.ip().is_unspecified() => {
            format!("http://localhost:{}/docs", addr.port())
        }
        addr => format!("http://{}/docs", addr),
    }
}
