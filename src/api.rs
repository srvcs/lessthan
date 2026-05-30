use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::{OpenApi, ToSchema};

use crate::client::{self, DepError};

pub const SERVICE: &str = "srvcs-lessthan";
pub const CONCERN: &str = "comparison: a < b";
pub const DEPENDS_ON: &[&str] = &["srvcs-isnumber"];

/// Dependency endpoints, injected as router state so tests can point them at
/// mock services.
#[derive(Clone)]
pub struct Deps {
    pub isnumber_url: String,
}

#[derive(Serialize, ToSchema)]
pub struct Info {
    pub service: &'static str,
    pub concern: &'static str,
    pub depends_on: Vec<&'static str>,
}

/// `GET /` — service identity (srvcs service standard).
#[utoipa::path(get, path = "/", responses((status = 200, body = Info)))]
pub async fn index() -> Json<Info> {
    Json(Info {
        service: SERVICE,
        concern: CONCERN,
        depends_on: DEPENDS_ON.to_vec(),
    })
}

#[derive(Deserialize, ToSchema)]
pub struct EvalRequest {
    #[schema(value_type = Object)]
    pub a: Value,
    #[schema(value_type = Object)]
    pub b: Value,
}

#[derive(Serialize, ToSchema)]
pub struct ComparisonResponse {
    #[schema(value_type = Object)]
    pub a: Value,
    #[schema(value_type = Object)]
    pub b: Value,
    pub result: bool,
}

/// Coerce a validated numeric JSON value to an integer, accepting whole floats
/// (`4.0`) but rejecting genuinely fractional ones (`4.5`).
fn as_integer(value: &Value) -> Option<i64> {
    value.as_i64().or_else(|| {
        value
            .as_f64()
            .filter(|f| f.fract() == 0.0)
            .map(|f| f as i64)
    })
}

/// The single concern: is `a` strictly less than `b`?
pub fn less_than(a: i64, b: i64) -> bool {
    a < b
}

fn ok(a: Value, b: Value, result: bool) -> Response {
    (
        StatusCode::OK,
        Json(json!({ "a": a, "b": b, "result": result })),
    )
        .into_response()
}

fn invalid(reason: &str) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({ "error": reason })),
    )
        .into_response()
}

fn degraded(dependency: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": "dependency unavailable", "dependency": dependency })),
    )
        .into_response()
}

/// Validate a single operand against `srvcs-isnumber` and coerce it to an
/// integer. Returns `Ok(Some(n))` when valid, `Ok(None)` when the dependency
/// rejects it or it is not an integer, and `Err(())` when the dependency is
/// unreachable (which the caller surfaces as a degraded `503`).
async fn validate_operand(isnumber_url: &str, value: &Value) -> Result<Option<i64>, ()> {
    match client::evaluate_dep(isnumber_url, value).await {
        Err(DepError::Unreachable) => Err(()),
        Ok((200, body)) => {
            let is_number = body.get("result").and_then(Value::as_bool).unwrap_or(false);
            if !is_number {
                return Ok(None);
            }
            Ok(as_integer(value))
        }
        Ok(_) => Err(()),
    }
}

/// `POST /` — is `a` less than `b`?
///
/// Input validation is delegated to `srvcs-isnumber` over HTTP (the single
/// source of truth for "is this a number"), once per operand. If that
/// dependency is unreachable, this service reports itself degraded rather than
/// guessing.
#[utoipa::path(
    post,
    path = "/",
    request_body = EvalRequest,
    responses(
        (status = 200, body = ComparisonResponse),
        (status = 422, description = "an operand is not an integer"),
        (status = 503, description = "a dependency is unavailable")
    )
)]
pub async fn evaluate(State(deps): State<Deps>, Json(req): Json<EvalRequest>) -> Response {
    // Validate both operands; comparison is defined on integers.
    let a = match validate_operand(&deps.isnumber_url, &req.a).await {
        Err(()) => return degraded("srvcs-isnumber"),
        Ok(None) => return invalid("a is not an integer"),
        Ok(Some(n)) => n,
    };
    let b = match validate_operand(&deps.isnumber_url, &req.b).await {
        Err(()) => return degraded("srvcs-isnumber"),
        Ok(None) => return invalid("b is not an integer"),
        Ok(Some(n)) => n,
    };

    ok(req.a, req.b, less_than(a, b))
}

#[derive(OpenApi)]
#[openapi(
    paths(index, evaluate),
    components(schemas(Info, EvalRequest, ComparisonResponse))
)]
pub struct ApiDoc;

/// Serve OpenAPI document
pub async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(ApiDoc::openapi())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_documents_routes() {
        let doc = ApiDoc::openapi();
        let root = doc.paths.paths.get("/").expect("path / present");
        assert!(root.get.is_some());
        assert!(root.post.is_some());
    }

    #[test]
    fn comparison_is_correct() {
        assert!(less_than(1, 2));
        assert!(less_than(-5, -4));
        assert!(less_than(i64::MIN, i64::MAX));
        assert!(!less_than(2, 1));
        assert!(!less_than(3, 3));
        assert!(!less_than(0, 0));
        assert!(!less_than(i64::MAX, i64::MIN));
    }

    #[test]
    fn whole_floats_are_integers_but_fractions_are_not() {
        assert_eq!(as_integer(&json!(4)), Some(4));
        assert_eq!(as_integer(&json!(4.0)), Some(4));
        assert_eq!(as_integer(&json!(-6.0)), Some(-6));
        assert_eq!(as_integer(&json!(4.5)), None);
    }
}
