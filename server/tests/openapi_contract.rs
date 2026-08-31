use serde_json::Value;
use via_server::api::PlanningRequest;

#[test]
fn openapi_and_shared_fixtures_are_valid_json() {
    let openapi: Value = serde_json::from_str(include_str!("../openapi.json")).unwrap();
    assert_eq!(openapi["openapi"], "3.1.0");
    assert!(openapi["paths"]["/v1/journey-plans"]["post"].is_object());

    let request: PlanningRequest =
        serde_json::from_str(include_str!("../fixtures/api/planning-request.json")).unwrap();
    request.validate().unwrap();
    for fixture in [
        include_str!("../fixtures/api/journey-options-result.json"),
        include_str!("../fixtures/api/empty-result.json"),
        include_str!("../fixtures/api/invalid-request-error.json"),
    ] {
        let _: Value = serde_json::from_str(fixture).unwrap();
    }
}
