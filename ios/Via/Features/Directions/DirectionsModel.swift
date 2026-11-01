import Foundation
import Observation

enum DirectionsFailure: Error, Equatable {
    case location(LocationError)
    case planning(JourneyPlanningError)
    case sameEndpoints
}

@Observable
final class DirectionsModel {
    enum Phase {
        case idle
        case loading
        case loaded(JourneyPlan)
        case failed(DirectionsFailure)
    }

    var origin: JourneyEndpoint = .currentLocation
    var destination: JourneyEndpoint?
    var departure: DepartureSetting = .now
    var modes: Set<PlanningMode> {
        didSet { defaults.set(PlanningMode.allCases.filter(modes.contains).map(\.rawValue), forKey: Self.modesKey) }
    }
    var selectedOptionID: String?
    private(set) var phase: Phase = .idle
    private(set) var isRefreshing = false

    @ObservationIgnored private let planner: any JourneyPlanner
    @ObservationIgnored private let location: any LocationProviding
    @ObservationIgnored private let defaults: UserDefaults
    @ObservationIgnored private var generation = 0

    private static let modesKey = "selectedTransportModes"

    init(planner: any JourneyPlanner, location: any LocationProviding, defaults: UserDefaults = .standard) {
        self.planner = planner
        self.location = location
        self.defaults = defaults
        if let stored = defaults.stringArray(forKey: Self.modesKey) {
            modes = Set(stored.compactMap(PlanningMode.init(rawValue:)))
        } else {
            modes = Set(PlanningMode.allCases)
        }
    }

    var plan: JourneyPlan? {
        if case let .loaded(plan) = phase { plan } else { nil }
    }

    var options: [JourneyOption] { plan?.options ?? [] }

    var selectedOption: JourneyOption? {
        options.first { $0.id == selectedOptionID } ?? options.first
    }

    func toggle(_ mode: PlanningMode) {
        if modes.contains(mode) {
            modes.remove(mode)
        } else {
            modes.insert(mode)
        }
    }

    func swapEndpoints() {
        guard let destination else { return }
        self.destination = origin
        origin = destination
    }

    func reset() {
        generation += 1
        phase = .idle
        isRefreshing = false
        selectedOptionID = nil
    }

    func plan() async {
        guard let destination else { return }
        generation += 1
        let current = generation
        let keepKey = selectedOption?.matchingKey
        if plan != nil {
            isRefreshing = true
        } else {
            phase = .loading
        }

        let outcome: Result<JourneyPlan, DirectionsFailure>
        do {
            let request = PlanningRequest(
                origin: try await resolve(origin),
                destination: try await resolve(destination),
                modes: modes,
                departure: departure
            )
            guard request.origin.coordinate.distance(to: request.destination.coordinate) > 1 else {
                throw DirectionsFailure.sameEndpoints
            }
            outcome = .success(try await planner.plan(request))
        } catch let failure as DirectionsFailure {
            outcome = .failure(failure)
        } catch let error as LocationError {
            outcome = .failure(.location(error))
        } catch let error as JourneyPlanningError {
            outcome = .failure(.planning(error))
        } catch is CancellationError {
            return
        } catch {
            outcome = .failure(.planning(.unexpected))
        }

        guard current == generation else { return }
        isRefreshing = false
        switch outcome {
        case let .success(plan):
            selectedOptionID = keepKey.flatMap { key in plan.options.first { $0.matchingKey == key }?.id }
                ?? plan.options.first?.id
            phase = .loaded(plan)
        case let .failure(failure):
            selectedOptionID = nil
            phase = .failed(failure)
        }
    }

    private func resolve(_ endpoint: JourneyEndpoint) async throws -> Place {
        switch endpoint {
        case let .place(place):
            return place
        case .currentLocation:
            let coordinate = try await location.currentCoordinate()
            return Place(id: "current-location", name: String(localized: "My Location"), coordinate: coordinate)
        }
    }
}
