use utoipa::{
    openapi::schema::{ArrayItems, Object, Schema, Type},
    openapi::{InfoBuilder, OpenApi, RefOr},
};
use utoipa_axum::router::OpenApiRouter;

use crate::api::API_VERSION;

use super::AppState;

pub(super) fn documented_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(utoipa_axum::routes!(super::handlers::liveness))
        .routes(utoipa_axum::routes!(super::handlers::readiness))
        .routes(utoipa_axum::routes!(super::handlers::metrics))
        .routes(utoipa_axum::routes!(super::handlers::street_routing_health))
        .routes(utoipa_axum::routes!(super::handlers::journey_plan))
}

pub(super) fn openapi_document() -> OpenApi {
    configure_openapi(documented_router().into_openapi())
}

pub(super) fn configure_openapi(mut document: OpenApi) -> OpenApi {
    document.info = InfoBuilder::new()
        .title("Via API")
        .version(API_VERSION)
        .description(Some(
            "Provider-neutral Journey Planning and service health API.",
        ))
        .build();
    apply_schema_constraints(&mut document);
    document
}

fn apply_schema_constraints(document: &mut OpenApi) {
    set_schema_enum(
        document,
        "JourneyMode",
        ["walking", "transit", "bicycle"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "LivenessResponse",
        "alive",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ResultMetadata",
        "api_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ResultMetadata",
        "semantic_catalog_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ResultMetadata",
        "time_zone",
        ["Asia/Taipei"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "Warning",
        "required",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "Warning",
        "severity",
        ["information", "caution"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "SemanticObject",
        "required",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ErrorEnvelope",
        "api_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ErrorEnvelope",
        "semantic_catalog_version",
        ["1.0"].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "ErrorObject",
        "required",
        [true].map(serde_json::Value::from),
    );
    set_schema_property_enum(
        document,
        "Geometry",
        "encoding",
        ["polyline6"].map(serde_json::Value::from),
    );
    set_schema_property_unique_items(document, "PlanningRequest", "selected_modes");
    set_schema_property_items_enum(
        document,
        "PlanningRequest",
        "selected_modes",
        ["bus", "metro", "tra", "thsr", "bike_share"].map(serde_json::Value::from),
    );
}

fn set_schema_enum<I>(document: &mut OpenApi, schema_name: &str, values: I)
where
    I: IntoIterator<Item = serde_json::Value>,
{
    if let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
    {
        schema.enum_values = Some(values.into_iter().collect());
    }
}

fn set_schema_property_enum<I>(
    document: &mut OpenApi,
    schema_name: &str,
    property_name: &str,
    values: I,
) where
    I: IntoIterator<Item = serde_json::Value>,
{
    if let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
        && let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(property))) =
            schema.properties.get_mut(property_name)
    {
        property.enum_values = Some(values.into_iter().collect());
    }
}

fn set_schema_property_unique_items(
    document: &mut OpenApi,
    schema_name: &str,
    property_name: &str,
) {
    if let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
        && let Some(utoipa::openapi::RefOr::T(utoipa::openapi::Schema::Array(property))) =
            schema.properties.get_mut(property_name)
    {
        property.unique_items = true;
    }
}

fn set_schema_property_items_enum<I>(
    document: &mut OpenApi,
    schema_name: &str,
    property_name: &str,
    values: I,
) where
    I: IntoIterator<Item = serde_json::Value>,
{
    if let Some(RefOr::T(Schema::Object(schema))) = document
        .components
        .as_mut()
        .and_then(|components| components.schemas.get_mut(schema_name))
        && let Some(RefOr::T(Schema::Array(property))) = schema.properties.get_mut(property_name)
    {
        let mut item_schema = Object::with_type(Type::String);
        item_schema.enum_values = Some(values.into_iter().collect());
        property.items = ArrayItems::RefOrSchema(Box::new(RefOr::T(Schema::Object(item_schema))));
    }
}
