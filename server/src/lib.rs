#[path = "api/mod.rs"]
pub mod api;
#[path = "app/mod.rs"]
pub mod app;
pub mod domain;
mod entity;
mod migration;
pub mod observability;
pub mod providers;
pub mod routing;
pub mod street;
