//! Axum route boundary. Route registration is owned by the parent app module
//! so OpenAPI remains coupled to the actual handlers.

pub use super::{
    build_router, build_router_with_network, build_router_with_realtime,
    build_router_with_realtime_and_network, build_router_with_realtime_and_network_and_ttl,
    openapi_document,
};
