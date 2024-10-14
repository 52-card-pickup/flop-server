use std::sync::Arc;

use aide::{axum::ApiRouter, openapi::OpenApi, transform::TransformOpenApi};
use axum::{middleware, Extension};
use tower_http::{cors::CorsLayer, trace::TraceLayer};

pub mod app_metrics;
pub mod cards;
pub mod game;
pub mod models;
pub mod state;

mod doc_routes;
mod routes;

pub fn create_application(state: state::SharedState) -> axum::Router {
    let mut api = OpenApi::default();
    ApiRouter::new()
        .nest_api_service("/api/v1", routes::api_routes(state.clone()))
        .route_layer(middleware::from_fn(layer::add_anonymous_player_id))
        .route_layer(middleware::from_fn(layer::track_router_metrics))
        .route("/health", axum::routing::get(|| async { "ok" }))
        .nest_api_service("/docs", doc_routes::docs_routes(state.clone()))
        .nest_api_service("/metrics", metric_routes())
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

fn metric_routes() -> axum::Router {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        autometrics::prometheus_exporter::init();
    });

    axum::Router::new().route(
        "/",
        axum::routing::get(|| async { autometrics::prometheus_exporter::encode_http_response() }),
    )
}

pub mod layer {
    use std::time::Instant;

    use axum::{
        extract::{self, FromRequestParts, MatchedPath},
        http::{Request, Response, StatusCode},
        middleware::Next,
        response::IntoResponse,
        Extension,
    };
    use axum_extra::extract::{cookie::Cookie, CookieJar};

    use crate::app_metrics::{metrics_labels, Metrics};
    use inner::SetApidCookie;

    #[derive(Clone)]
    pub struct Apid(pub String);

    pub mod inner {
        #[derive(Clone)]
        pub struct SetApidCookie(pub uuid::Uuid);
    }

    pub async fn track_router_metrics(req: extract::Request, next: Next) -> impl IntoResponse {
        let start = Instant::now();
        let path = if let Some(matched_path) = req.extensions().get::<MatchedPath>() {
            matched_path.as_str().to_owned()
        } else {
            req.uri().path().to_owned()
        };
        let method = req.method().clone();

        let response = next.run(req).await;

        let latency_ms = start.elapsed().as_secs_f64() * 1000.0;

        let labels =
            metrics_labels::http_requests(&method.to_string(), &path, response.status().as_u16());

        Metrics::c_http_requests_total_incr(labels.clone());
        Metrics::h_http_requests_duration_ms(labels, latency_ms);

        response
    }

    pub async fn add_anonymous_player_id(
        mut req: extract::Request,
        next: Next,
    ) -> Result<impl IntoResponse, StatusCode> {
        let cookies = CookieJar::from_headers(req.headers());

        let apid_cookie = cookies
            .get("apid")
            .filter(|cookie| uuid::Uuid::try_parse(cookie.value_trimmed()).is_ok());

        let (apid, created_apid) = match apid_cookie {
            Some(cookie) => {
                let apid = cookie.value_trimmed().to_string();
                (apid, None)
            }
            None => {
                let uuid = uuid::Uuid::new_v4();
                let apid = uuid.to_string();
                (apid, Some(uuid))
            }
        };

        req.extensions_mut().insert(Apid(apid));

        let mut response = next.run(req).await;

        if let Some(apid) = created_apid {
            let cookie = Cookie::build(("apid", apid.to_string()))
                .path("/")
                // .secure(true)
                .http_only(true);

            response
                .headers_mut()
                .insert("Set-Cookie", cookie.to_string().parse().unwrap());
        }

        Ok(response)
    }
}
