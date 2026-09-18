//! Process configuration shared by development tooling and the server.

/// Load the repository-local dotenv file when present. Existing process
/// environment values keep dotenvy's normal precedence.
pub fn load_dotenv() {
    dotenvy::dotenv().ok();
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../.env")).ok();
}
