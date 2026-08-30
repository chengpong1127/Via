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
        case temporarilyUnavailable
        case unexpectedFailure
    }

    var origin: SelectedPlace?
    var destination: SelectedPlace?
    var departureSetting: DepartureSetting = .departNow
    private(set) var state: PresentationState = .editing
    private(set) var result: JourneyResult?
    private(set) var selectedOptionID: String?
    private(set) var refreshMessage: String?

    var selectedModes: Set<TravelMode> {
        didSet { persistSelectedModes() }
    }

    private let planner: any JourneyPlanning
    private let defaults: UserDefaults
    private var lastRequest: PlanningRequest?

    init(
        planner: any JourneyPlanning,
        defaults: UserDefaults = .standard,
        origin: SelectedPlace? = nil,
        destination: SelectedPlace? = nil
    ) {
        self.planner = planner
        self.defaults = defaults
        self.origin = origin
        self.destination = destination

        let stored = defaults.array(forKey: "selectedTravelModes") as? [String] ?? []
        let restored = Set(stored.compactMap(TravelMode.init(rawValue:)))
        selectedModes = restored.isEmpty ? [.bus, .metro, .youbike] : restored
    }

    var selectedOption: JourneyOption? {
        guard let selectedOptionID else { return result?.options.first }
        return result?.options.first(where: { $0.id == selectedOptionID })
    }

    var canSubmit: Bool {
        origin != nil && destination != nil && !selectedModes.isEmpty
    }

    func toggle(_ mode: TravelMode) {
        guard TravelMode.selectable.contains(mode) else { return }
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
        guard let request = makeRequest(locale: locale) else {
            state = .invalidRequest(field: missingField)
            return
        }

        state = .loading
        result = nil
        selectedOptionID = nil
        refreshMessage = nil
        lastRequest = request

        do {
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
        if origin == nil { return "origin" }
        if destination == nil { return "destination" }
        return "selected_modes"
    }

    private func makeRequest(locale: String) -> PlanningRequest? {
        guard let origin, let destination, !selectedModes.isEmpty else { return nil }
        return PlanningRequest(
            origin: origin,
            destination: destination,
            selectedModes: selectedModes,
            departureSetting: departureSetting,
            locale: locale
        )
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
        case JourneyPlanningError.temporarilyUnavailable:
            state = .temporarilyUnavailable
        default:
            state = .unexpectedFailure
        }
    }

    private func persistSelectedModes() {
        let rawValues = TravelMode.selectable
            .filter(selectedModes.contains)
            .map(\.rawValue)
        defaults.set(rawValues, forKey: "selectedTravelModes")
    }
}
