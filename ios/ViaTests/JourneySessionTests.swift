import CoreLocation
import Foundation
import Testing
@testable import Via

@MainActor
struct JourneySessionTests {
    @Test("Submitting exposes server ordering and selects the first Journey Option")
    func submitSelectsFirstServerOption() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["first", "second"]))])
        let session = makeSession(planner: planner)

        await session.submit()

        #expect(session.state == .results)
        #expect(session.result?.options.map(\.id) == ["first", "second"])
        #expect(session.selectedOptionID == "first")
    }

    @Test("Refresh keeps the previous complete result visible when replacement fails")
    func refreshFailureKeepsResult() async {
        let initial = Self.result(ids: ["original"])
        let planner = PlannerStub(results: [.success(initial), .failure(.temporarilyUnavailable)])
        let session = makeSession(planner: planner)
        await session.submit()

        await session.refresh()

        #expect(session.state == .results)
        #expect(session.result?.options.map(\.id) == ["original"])
        #expect(session.refreshMessage != nil)
    }

    @Test("Refresh preserves selection only through the server matching key")
    func refreshUsesMatchingKey() async {
        let initial = Self.result(ids: ["a", "b"], matchingKeys: ["key-a", "key-b"])
        let replacement = Self.result(ids: ["new-b", "new-a"], matchingKeys: ["key-b", "key-a"])
        let planner = PlannerStub(results: [.success(initial), .success(replacement)])
        let session = makeSession(planner: planner)
        await session.submit()
        session.select(initial.options[1])

        await session.refresh()

        #expect(session.selectedOptionID == "new-b")
    }

    @Test("Current Location is the default origin")
    func currentLocationIsDefaultOrigin() {
        let session = JourneySession(
            planner: PlannerStub(results: []),
            locationProvider: LocationProviderStub(),
            defaults: UserDefaults(suiteName: UUID().uuidString)!,
            destination: Self.place("Destination")
        )

        #expect(session.origin == .currentLocation)
        #expect(session.canSubmit)
    }

    @Test("Current Location resolves before the planner receives a request")
    func currentLocationOriginResolvesForPlanning() async {
        let coordinate = Coordinate(latitude: 24.1477, longitude: 120.6736)
        let planner = PlannerStub(results: [.success(Self.result(ids: ["taichung"]))])
        let locationProvider = LocationProviderStub(
            status: .authorizedWhenInUse,
            result: .success(coordinate)
        )
        let session = makeSession(
            planner: planner,
            origin: .currentLocation,
            locationProvider: locationProvider
        )

        await session.submit()

        let requests = await planner.requests
        #expect(requests.first?.origin.coordinate == coordinate)
        #expect(session.state == .results)
        #expect(session.origin == .currentLocation)
        #expect(locationProvider.currentLocationCallCount == 1)
    }

    @Test("Not determined permission is requested only when planning from Current Location")
    func currentLocationRequestsPermissionWhenNeeded() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["authorized"]))])
        let locationProvider = LocationProviderStub(
            status: .notDetermined,
            authorizationAfterRequest: .authorizedWhenInUse,
            result: .success(Coordinate(latitude: 25.03, longitude: 121.56))
        )
        let session = makeSession(
            planner: planner,
            origin: .currentLocation,
            locationProvider: locationProvider
        )

        await session.submit()

        #expect(locationProvider.requestAuthorizationCallCount == 1)
        #expect(locationProvider.currentLocationCallCount == 1)
        #expect(session.state == .results)
    }

    @Test("Manual origin never asks the location provider for GPS")
    func manualOriginDoesNotResolveLocation() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["manual"]))])
        let locationProvider = LocationProviderStub(
            status: .authorizedWhenInUse,
            result: .success(Coordinate(latitude: 22.6273, longitude: 120.3014))
        )
        let session = makeSession(
            planner: planner,
            origin: .place(Self.place("Kaohsiung Station")),
            locationProvider: locationProvider
        )

        await session.submit()

        let requests = await planner.requests
        #expect(requests.first?.origin.name == "Kaohsiung Station")
        #expect(locationProvider.currentLocationCallCount == 0)
    }

    @Test("Denied Current Location prevents planning and exposes a location error")
    func deniedLocationPreventsPlanning() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["not-used"]))])
        let locationProvider = LocationProviderStub(status: .denied, result: .failure(.permissionDenied))
        let session = makeSession(
            planner: planner,
            origin: .currentLocation,
            locationProvider: locationProvider
        )

        await session.submit()

        #expect(session.state == .locationPermissionDenied)
        #expect(locationProvider.currentLocationCallCount == 0)
        let requests = await planner.requests
        #expect(requests.isEmpty)
    }

    @Test("Restricted Current Location remains distinct from denied permission")
    func restrictedLocationPreventsPlanning() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["not-used"]))])
        let locationProvider = LocationProviderStub(status: .restricted, result: .failure(.restricted))
        let session = makeSession(
            planner: planner,
            origin: .currentLocation,
            locationProvider: locationProvider
        )

        await session.submit()

        #expect(session.state == .locationRestricted)
        #expect(locationProvider.currentLocationCallCount == 0)
        let requests = await planner.requests
        #expect(requests.isEmpty)
    }

    @Test("A location retrieval failure does not send an invalid planning request")
    func locationRetrievalFailurePreventsPlanning() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["not-used"]))])
        let locationProvider = LocationProviderStub(status: .authorizedWhenInUse, result: .failure(.unavailable))
        let session = makeSession(
            planner: planner,
            origin: .currentLocation,
            locationProvider: locationProvider
        )

        await session.submit()

        #expect(session.state == .locationUnavailable)
        let requests = await planner.requests
        #expect(requests.isEmpty)
    }

    @Test("Returning to editing clears stale results")
    func returningToEditingClearsResults() async {
        let planner = PlannerStub(results: [.success(Self.result(ids: ["stale"]))])
        let session = makeSession(planner: planner)

        await session.submit()
        session.returnToEditing()

        #expect(session.state == .editing)
        #expect(session.result == nil)
        #expect(session.selectedOptionID == nil)
        #expect(session.refreshMessage == nil)
    }

    private func makeSession(
        planner: PlannerStub,
        origin: JourneyEndpoint = .place(Place(name: "Origin", coordinate: Coordinate(latitude: 25.04, longitude: 121.56))),
        destination: Place = Place(name: "Destination", coordinate: Coordinate(latitude: 25.04, longitude: 121.56)),
        locationProvider: LocationProviderStub? = nil
    ) -> JourneySession {
        let suite = UserDefaults(suiteName: UUID().uuidString)!
        return JourneySession(
            planner: planner,
            locationProvider: locationProvider ?? LocationProviderStub(),
            defaults: suite,
            origin: origin,
            destination: destination
        )
    }

    private static func place(_ name: String) -> Place {
        Place(name: name, coordinate: Coordinate(latitude: 25.04, longitude: 121.56))
    }

    private static func result(ids: [String], matchingKeys: [String]? = nil) -> JourneyResult {
        let keys = matchingKeys ?? ids.map { "key-\($0)" }
        let generatedAt = Date()
        return JourneyResult(
            generatedAt: generatedAt,
            options: zip(ids, keys).map { id, key in JourneyOption(
                id: id,
                matchingKey: key,
                label: .bestNow,
                departureTime: .now,
                arrivalTime: .now,
                durationSeconds: 600,
                walkingSeconds: 120,
                transferCount: 0,
                travelModes: [.walking, .transit],
                liveStatusSummary: LiveStatusSummary(state: .fresh, observedAt: .now),
                overview: [],
                legs: []
            ) }
        )
    }
}

private actor PlannerStub: JourneyPlanning {
    private var results: [Result<JourneyResult, JourneyPlanningError>]
    private(set) var requests: [PlanningRequest] = []

    init(results: [Result<JourneyResult, JourneyPlanningError>]) {
        self.results = results
    }

    func plan(_ request: PlanningRequest) async throws -> JourneyResult {
        requests.append(request)
        guard !results.isEmpty else { throw JourneyPlanningError.unexpectedFailure }
        return try results.removeFirst().get()
    }
}

@MainActor
private final class LocationProviderStub: LocationProviding {
    var authorizationStatus: CLAuthorizationStatus
    let authorizationAfterRequest: CLAuthorizationStatus?
    let result: Result<Coordinate, LocationProvidingError>
    private(set) var requestAuthorizationCallCount = 0
    private(set) var currentLocationCallCount = 0

    init(
        status: CLAuthorizationStatus = .authorizedWhenInUse,
        authorizationAfterRequest: CLAuthorizationStatus? = nil,
        result: Result<Coordinate, LocationProvidingError> = .success(Coordinate(latitude: 25.04, longitude: 121.56))
    ) {
        authorizationStatus = status
        self.authorizationAfterRequest = authorizationAfterRequest
        self.result = result
    }

    func requestWhenInUseAuthorization() {
        requestAuthorizationCallCount += 1
        if let authorizationAfterRequest {
            authorizationStatus = authorizationAfterRequest
        }
    }

    func currentLocation() async throws -> Coordinate {
        currentLocationCallCount += 1
        return try result.get()
    }
}
