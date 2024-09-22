use std::sync::Arc;

use aide::{axum::ApiRouter, openapi::OpenApi, transform::TransformOpenApi};
use axum::Extension;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

pub mod cards;
pub mod game;
pub mod models;
pub mod state;

mod doc_routes;
mod routes;

pub fn create_application(state: state::SharedState) -> axum::Router {
    let mut api = OpenApi::default();
    ApiRouter::new()
        .route("/health", axum::routing::get(|| async { "ok" }))
        .nest_api_service("/api/v1", routes::api_routes(state.clone()))
        .nest_api_service("/docs", doc_routes::docs_routes(state.clone()))
        .finish_api_with(&mut api, api_docs)
        .layer(Extension(Arc::new(api)))
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
}

fn api_docs(api: TransformOpenApi) -> TransformOpenApi {
    api.title("flop: The Party Poker Game")
        .summary("API for poker game")
        .description(include_str!("../README.md"))
}
