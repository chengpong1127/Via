use std::sync::Arc;

use crate::support::build_test_router;
use axum::{
    body::Body,
    http::{Request, StatusCode, header::CACHE_CONTROL},
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use via_server::{api::PlanningRequest, app::openapi_document, street::FakeStreetRouter};

#[test]
fn generated_openapi_covers_the_http_contract() {
    let openapi = serde_json::to_value(openapi_document()).unwrap();
    assert_eq!(openapi["openapi"], "3.1.0");
    assert_eq!(openapi["info"]["title"], "Via API");
    assert_eq!(openapi["info"]["version"], "1.0");

    for path in [
        "/health/live",
        "/health/ready",
        "/metrics",
        "/health/street-routing",
        "/v1/journey-plans",
    ] {
        assert!(openapi["paths"][path].is_object(), "missing route {path}");
    }

    assert_eq!(
        openapi["paths"]["/health/live"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/LivenessResponse"
    );
    assert!(openapi["paths"]["/health/ready"]["get"]["responses"]["200"].is_object());
    assert!(openapi["paths"]["/health/ready"]["get"]["responses"]["503"].is_object());
    assert!(openapi["paths"]["/health/street-routing"]["get"]["responses"]["200"].is_object());
    assert!(openapi["paths"]["/health/street-routing"]["get"]["responses"]["503"].is_object());
    assert!(openapi["paths"]["/metrics"]["get"]["responses"]["200"].is_object());

    let journey = &openapi["paths"]["/v1/journey-plans"]["post"];
    assert_eq!(
        journey["requestBody"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/PlanningRequest"
    );
    for status in ["200", "400", "408", "500", "503"] {
        assert!(
            journey["responses"][status].is_object(),
            "missing journey status {status}"
        );
    }
    assert_eq!(
        journey["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/PlanningOutcome"
    );
    for status in ["400", "408", "500", "503"] {
        assert_eq!(
            journey["responses"][status]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ErrorEnvelope"
        );
    }

    for (path, method) in [
        ("/health/live", "get"),
        ("/health/ready", "get"),
        ("/metrics", "get"),
        ("/health/street-routing", "get"),
        ("/v1/journey-plans", "post"),
    ] {
        assert!(
            openapi["paths"][path][method].get("security").is_none(),
            "public endpoint {method} {path} must not require authentication"
        );
    }

    for schema in [
        "PlanningRequest",
        "SelectedPlace",
        "Coordinate",
        "DepartureSetting",
        "PlanningOutcome",
        "JourneyOptionsResult",
        "EmptyResult",
        "ErrorEnvelope",
        "ReadinessResponse",
        "HealthResponse",
    ] {
        assert!(
            openapi["components"]["schemas"][schema].is_object(),
            "missing schema {schema}"
        );
    }

    let health_properties = &openapi["components"]["schemas"]["HealthResponse"]["properties"];
    assert!(health_properties["healthy"].is_object());
    assert!(health_properties["provider"].is_object());
    for removed in ["graph_revision", "pinned_graph_revision", "capabilities"] {
        assert!(
            health_properties[removed].is_null(),
            "health leaked {removed}"
        );
    }

    let planning_properties = &openapi["components"]["schemas"]["PlanningRequest"]["properties"];
    assert!(planning_properties["selected_modes"].is_object());
    assert!(planning_properties["departure_setting"].is_object());
    assert_eq!(planning_properties["selected_modes"]["minItems"], 1);
    assert!(planning_properties["selected_modes"]["maxItems"].is_null());
    assert_eq!(planning_properties["selected_modes"]["uniqueItems"], true);
    assert_eq!(
        planning_properties["selected_modes"]["items"]["enum"],
        serde_json::json!(["bus", "metro", "tra", "thsr", "bike_share"])
    );
    assert_eq!(
        openapi["components"]["schemas"]["JourneyMode"]["enum"],
        serde_json::json!(["walking", "transit", "bicycle"])
    );
    assert_eq!(
        openapi["components"]["schemas"]["TransitMode"]["enum"],
        serde_json::json!(["bus", "metro", "rail", "high_speed_rail"])
    );
    assert!(openapi["components"]["schemas"]["DepartureSetting"]["oneOf"].is_array());
    assert!(openapi["components"]["schemas"]["LegDetails"].is_object());
    assert!(openapi["components"]["schemas"]["WarningScope"]["properties"]["type"].is_object());

    for internal_schema in [
        "AppState",
        "PlanningError",
        "NormalizedLiveStatus",
        "LiveStatusValue",
    ] {
        assert!(
            openapi["components"]["schemas"]
                .get(internal_schema)
                .is_none(),
            "internal type {internal_schema} leaked into the public document"
        );
    }

    let request: PlanningRequest =
        serde_json::from_str(include_str!("../../fixtures/api/planning-request.json")).unwrap();
    request.validate().unwrap();
    for fixture in [
        include_str!("../../fixtures/api/walking-result.json"),
        include_str!("../../fixtures/api/transit-result.json"),
        include_str!("../../fixtures/api/bike-share-result.json"),
    ] {
        let _: Value = serde_json::from_str(fixture).unwrap();
    }
    let _: Value =
        serde_json::from_str(include_str!("../../fixtures/api/empty-result.json")).unwrap();
    let _: Value = serde_json::from_str(include_str!(
        "../../fixtures/api/invalid-request-error.json"
    ))
    .unwrap();
}

#[tokio::test]
async fn openapi_endpoint_serves_the_generated_document_without_caching() {
    let app = build_test_router(Arc::new(FakeStreetRouter::builder().build()))
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    let served: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(served, serde_json::to_value(openapi_document()).unwrap());
}

#[test]
fn representative_payload_serialization_remains_compatible_with_generated_schemas() {
    let document = serde_json::to_value(openapi_document()).unwrap();
    let request_fixture: Value =
        serde_json::from_str(include_str!("../../fixtures/api/planning-request.json")).unwrap();
    let request: PlanningRequest = serde_json::from_value(request_fixture.clone()).unwrap();
    assert_eq!(serde_json::to_value(request).unwrap(), request_fixture);
    assert_schema_compatible(
        &document,
        "PlanningRequest",
        &request_fixture,
        "PlanningRequest",
    );

    for (schema, fixture) in [
        (
            "JourneyOptionsResult",
            include_str!("../../fixtures/api/walking-result.json"),
        ),
        (
            "JourneyOptionsResult",
            include_str!("../../fixtures/api/transit-result.json"),
        ),
        (
            "JourneyOptionsResult",
            include_str!("../../fixtures/api/bike-share-result.json"),
        ),
        (
            "EmptyResult",
            include_str!("../../fixtures/api/empty-result.json"),
        ),
        (
            "ErrorEnvelope",
            include_str!("../../fixtures/api/invalid-request-error.json"),
        ),
    ] {
        let payload: Value = serde_json::from_str(fixture).unwrap();
        assert_schema_compatible(&document, schema, &payload, schema);
    }
}

fn assert_schema_compatible(document: &Value, schema_name: &str, value: &Value, label: &str) {
    if let Err(error) = validate_schema(
        document,
        &document["components"]["schemas"][schema_name],
        value,
        "$".into(),
    ) {
        panic!("{label} fixture is incompatible with generated schema: {error}");
    }
}

fn validate_schema(
    document: &Value,
    schema: &Value,
    value: &Value,
    path: String,
) -> Result<(), String> {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| format!("{path}: unsupported schema reference {reference}"))?;
        let target = document
            .pointer(pointer)
            .ok_or_else(|| format!("{path}: missing schema reference {reference}"))?;
        return validate_schema(document, target, value, path);
    }

    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        if branches
            .iter()
            .any(|branch| validate_schema(document, branch, value, path.clone()).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path}: value did not match any oneOf branch"));
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        if branches
            .iter()
            .any(|branch| validate_schema(document, branch, value, path.clone()).is_ok())
        {
            return Ok(());
        }
        return Err(format!("{path}: value did not match any anyOf branch"));
    }
    if let Some(branches) = schema.get("allOf").and_then(Value::as_array) {
        for branch in branches {
            validate_schema(document, branch, value, path.clone())?;
        }
    }

    if let Some(constant) = schema.get("const")
        && constant != value
    {
        return Err(format!("{path}: expected const {constant}, got {value}"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.iter().any(|candidate| candidate == value)
    {
        return Err(format!("{path}: value {value} is not in enum {values:?}"));
    }

    if let Some(types) = schema.get("type") {
        let valid = types.as_array().map_or_else(
            || type_matches(types, value),
            |types| types.iter().any(|ty| type_matches(ty, value)),
        );
        if !valid {
            return Err(format!(
                "{path}: value {value} has incompatible type {types}"
            ));
        }
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let object = value
            .as_object()
            .ok_or_else(|| format!("{path}: required properties need an object"))?;
        for property in required.iter().filter_map(Value::as_str) {
            if !object.contains_key(property) {
                return Err(format!("{path}: missing required property {property}"));
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(Value::as_object)
        && let Some(object) = value.as_object()
    {
        let additional = schema.get("additionalProperties");
        for (property, property_value) in object {
            if let Some(property_schema) = properties.get(property) {
                validate_schema(
                    document,
                    property_schema,
                    property_value,
                    format!("{path}.{property}"),
                )?;
            } else if additional == Some(&Value::Bool(false)) {
                return Err(format!("{path}: unexpected property {property}"));
            } else if let Some(additional_schema) = additional.filter(|value| !value.is_boolean()) {
                validate_schema(
                    document,
                    additional_schema,
                    property_value,
                    format!("{path}.{property}"),
                )?;
            }
        }
    }

    if let Some(items) = schema.get("items") {
        if let Some(array) = value.as_array() {
            for (index, item) in array.iter().enumerate() {
                validate_schema(document, items, item, format!("{path}[{index}]"))?;
            }
        }
    }

    Ok(())
}

fn type_matches(schema_type: &Value, value: &Value) -> bool {
    match schema_type.as_str() {
        Some("array") => value.is_array(),
        Some("boolean") => value.is_boolean(),
        Some("integer") => value.as_i64().is_some() || value.as_u64().is_some(),
        Some("number") => value.is_number(),
        Some("null") => value.is_null(),
        Some("object") => value.is_object(),
        Some("string") => value.is_string(),
        _ => true,
    }
}
