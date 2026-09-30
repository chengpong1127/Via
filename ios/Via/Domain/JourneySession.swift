import Foundation
import Observation

@MainActor
@Observable
final class JourneySession {
    enum PresentationState: Equatable {
        case editing
        case loading
        case results
        case refreshing
        case empty
        case invalidRequest(field: String)
        case locationPermissionDenied
        case locationRestricted
        case locationUnavailable
        case temporarilyUnavailable
        case unexpectedFailure
    }

    var origin: JourneyEndpoint = .currentLocation
    var destination: Place?
    var departureSetting: DepartureSetting = .departNow
    private(set) var state: PresentationState = .editing
    private(set) var result: JourneyResult?
    private(set) var selectedOptionID: String?
    private(set) var refreshMessage: String?

    var selectedModes: Set<PlanningMode> {
        didSet { persistSelectedModes() }
    }

    private let planner: any JourneyPlanning
    private let locationProvider: any LocationProviding
    private let defaults: UserDefaults
    private var lastRequest: PlanningRequest?

    init(
        planner: any JourneyPlanning,
        locationProvider: any LocationProviding,
        defaults: UserDefaults = .standard,
        origin: JourneyEndpoint = .currentLocation,
        destination: Place? = nil
    ) {
        self.planner = planner
        self.locationProvider = locationProvider
        self.defaults = defaults
        self.origin = origin
        self.destination = destination

        let stored = defaults.array(forKey: "selectedTransportModes") as? [String] ?? []
        let restored = Set(stored.compactMap(PlanningMode.init(rawValue:)))
            .intersection(Set(PlanningMode.selectable))
        selectedModes = restored.isEmpty ? Set(PlanningMode.selectable) : restored
    }

    var selectedOption: JourneyOption? {
        guard let selectedOptionID else { return result?.options.first }
        return result?.options.first(where: { $0.id == selectedOptionID })
    }

    var canSubmit: Bool {
        destination != nil && !selectedModes.isEmpty
    }

    func toggle(_ mode: PlanningMode) {
        guard PlanningMode.selectable.contains(mode) else { return }
        if selectedModes.contains(mode) {
            guard selectedModes.count > 1 else { return }
            selectedModes.remove(mode)
        } else {
            selectedModes.insert(mode)
        }
    }

    func select(_ option: JourneyOption) {
        guard result?.options.contains(where: { $0.id == option.id }) == true else { return }
        selectedOptionID = option.id
    }

    func submit(locale: String = Locale.current.identifier) async {
        guard destination != nil, !selectedModes.isEmpty else {
            state = .invalidRequest(field: missingField)
            return
        }

        state = .loading
        result = nil
        selectedOptionID = nil
        refreshMessage = nil
        lastRequest = nil

        do {
            let request = try await makeRequest(locale: locale)
            lastRequest = request
            apply(try await planner.plan(request), preservingMatchingKey: nil)
        } catch {
            applyInitialFailure(error)
        }
    }

    func refresh() async {
        guard let request = lastRequest, let currentResult = result else { return }
        let matchingKey = selectedOption?.matchingKey
        state = .refreshing
        refreshMessage = nil

        do {
            apply(try await planner.plan(request), preservingMatchingKey: matchingKey)
        } catch {
            result = currentResult
            state = currentResult.options.isEmpty ? .empty : .results
            refreshMessage = String(localized: "Refresh unavailable. Showing the previous result.")
        }
    }

    func returnToEditing() {
        state = .editing
        result = nil
        selectedOptionID = nil
        lastRequest = nil
        refreshMessage = nil
    }

    private var missingField: String {
        if destination == nil { return "destination" }
        return "selected_modes"
    }

    private func makeRequest(locale: String) async throws -> PlanningRequest {
        guard let destination, !selectedModes.isEmpty else {
            throw JourneyPlanningError.invalidRequest(field: missingField)
        }

        let origin = try await resolve(origin)
        return PlanningRequest(
            origin: origin,
            destination: destination,
            selectedModes: selectedModes,
            departureSetting: departureSetting,
            locale: LocaleNormalization.bcp47(locale)
        )
    }

    private func resolve(_ endpoint: JourneyEndpoint) async throws -> Place {
        switch endpoint {
        case .place(let place):
            return place
        case .currentLocation:
            switch locationProvider.authorizationStatus {
            case .notDetermined:
                locationProvider.requestWhenInUseAuthorization()
            case .authorizedWhenInUse, .authorizedAlways:
                break
            case .denied:
                throw LocationProvidingError.permissionDenied
            case .restricted:
                throw LocationProvidingError.restricted
            @unknown default:
                throw LocationProvidingError.unavailable
            }

            let coordinate = try await locationProvider.currentLocation()
            return Place(name: "Current Location", coordinate: coordinate)
        }
    }

    private func apply(_ newResult: JourneyResult, preservingMatchingKey matchingKey: String?) {
        result = newResult
        refreshMessage = nil

        if newResult.options.isEmpty {
            selectedOptionID = nil
            state = .empty
            return
        }

        selectedOptionID = matchingKey
            .flatMap { key in newResult.options.first(where: { $0.matchingKey == key })?.id }
            ?? newResult.options.first?.id
        state = .results
    }

    private func applyInitialFailure(_ error: Error) {
        result = nil
        selectedOptionID = nil
        switch error {
        case JourneyPlanningError.invalidRequest(let field):
            state = .invalidRequest(field: field)
        case LocationProvidingError.permissionDenied:
            state = .locationPermissionDenied
        case LocationProvidingError.restricted:
            state = .locationRestricted
        case LocationProvidingError.unavailable:
            state = .locationUnavailable
        case JourneyPlanningError.temporarilyUnavailable:
            state = .temporarilyUnavailable
        default:
            state = .unexpectedFailure
        }
    }

    private func persistSelectedModes() {
        let rawValues = PlanningMode.selectable
            .filter(selectedModes.contains)
            .map(\.rawValue)
        defaults.set(rawValues, forKey: "selectedTransportModes")
    }
}
