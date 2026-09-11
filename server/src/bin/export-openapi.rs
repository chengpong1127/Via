fn main() {
    let document = via_server::app::openapi_document();
    serde_json::to_writer_pretty(std::io::stdout().lock(), &document)
        .expect("writing generated OpenAPI to stdout should succeed");
    println!();
}
