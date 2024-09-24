use std::sync::Arc;

use aide::{axum::ApiRouter, openapi::OpenApi, transform::TransformOpenApi};
use axum::{
    middleware::{map_request, map_response},
    Extension,
};
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
        .layer(map_response(layer::write_response_apid_cookie))
        .layer(map_request(layer::ensure_request_apid_cookie))
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

pub mod layer {
    use axum::{
        extract::FromRequestParts,
        http::{Request, Response, StatusCode},
        Extension,
    };
    use axum_extra::extract::{cookie::Cookie, CookieJar};

    use inner::SetApidCookie;

    #[derive(Clone)]
    pub struct Apid(pub String);

    pub mod inner {
        #[derive(Clone)]
        pub struct SetApidCookie(pub uuid::Uuid);
    }

    pub async fn ensure_request_apid_cookie<B>(
        request: Request<B>,
    ) -> Result<Request<B>, StatusCode> {
        let (mut parts, body) = request.into_parts();
        let cookies = CookieJar::from_request_parts(&mut parts, &())
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;

        let apid_cookie = cookies
            .get("apid")
            .filter(|cookie| uuid::Uuid::parse_str(cookie.value_trimmed()).is_ok());

        match apid_cookie {
            Some(cookie) => {
                let apid = cookie.value_trimmed().to_string();
                parts.extensions.insert(Apid(apid));
            }
            None => {
                let uuid = uuid::Uuid::new_v4();
                let apid = uuid.to_string();
                parts.extensions.insert(Apid(apid));
                parts.extensions.insert(SetApidCookie(uuid));
            }
        }

        Ok(Request::from_parts(parts, body))
    }

    pub async fn write_response_apid_cookie<B>(
        extension: Option<Extension<SetApidCookie>>,
        mut response: Response<B>,
    ) -> Response<B> {
        match extension {
            Some(Extension(SetApidCookie(apid))) => {
                let cookie = Cookie::build(("apid", apid.to_string()))
                    .path("/")
                    // .secure(true)
                    .http_only(true);

                response
                    .headers_mut()
                    .insert("Set-Cookie", cookie.to_string().parse().unwrap());

                response
            }
            None => response,
        }
    }
}
