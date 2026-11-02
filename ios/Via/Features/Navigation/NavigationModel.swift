import Foundation
import Observation

nonisolated struct NavigationNotice: Equatable, Sendable {
    let id: Int
    let alert: NavigationAlert
}

@Observable
final class NavigationModel {
    static let rerouteCooldown: TimeInterval = 60

    private(set) var tracker: NavigationTracker
    private(set) var notice: NavigationNotice?
    private(set) var isRerouting = false
    private(set) var rerouteFailed = false
    private(set) var now: Date
    var isMinimized = false

    @ObservationIgnored let destination: Place
    @ObservationIgnored let modes: Set<PlanningMode>
    @ObservationIgnored private let planner: any JourneyPlanner
    @ObservationIgnored private let location: any LocationTracking
    @ObservationIgnored private let clock: () -> Date
    @ObservationIgnored private var lastRerouteAt: Date?
    @ObservationIgnored private var noticeCount = 0
    @ObservationIgnored private var tasks: [Task<Void, Never>] = []

    init(
        option: JourneyOption,
        destination: Place,
        modes: Set<PlanningMode>,
        planner: any JourneyPlanner,
        location: any LocationTracking,
        clock: @escaping () -> Date = Date.init
    ) {
        let now = clock()
        self.now = now
        tracker = NavigationTracker(route: NavigationRoute(option: option), startedAt: now)
        self.destination = destination
        self.modes = modes
        self.planner = planner
        self.location = location
        self.clock = clock
    }

    var option: JourneyOption { tracker.route.option }
    var currentLeg: NavigationLeg { tracker.currentLeg }
    var isFinished: Bool { tracker.isFinished }
    var estimatedArrival: Date { tracker.estimatedArrival(now: now) }

    func start() {
        guard tasks.isEmpty else { return }
        location.beginNavigation()
        let fixes = location.fixes()
        tasks = [
            Task { [weak self] in
                for await fix in fixes {
                    guard let self else { return }
                    await handle(fix)
                }
            },
            Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(10))
                    guard let self else { return }
                    await tick()
                }
            },
        ]
    }

    func stop() {
        tasks.forEach { $0.cancel() }
        tasks = []
        location.endNavigation()
    }

    func dismissNotice() {
        notice = nil
    }

    func handle(_ fix: LocationFix) async {
        now = fix.timestamp
        let update = tracker.update(fix)
        await apply(update, at: fix.coordinate)
    }

    func tick() async {
        now = clock()
        let update = tracker.tick(now: now)
        await apply(update, at: tracker.lastFix?.coordinate)
    }

    private func apply(_ update: NavigationUpdate, at coordinate: Coordinate?) async {
        if let alert = update.alerts.last {
            noticeCount += 1
            notice = NavigationNotice(id: noticeCount, alert: alert)
        }
        if tracker.isFinished {
            stop()
            return
        }
        if update.reroute != nil, let coordinate {
            await reroute(from: coordinate)
        }
    }

    private func reroute(from coordinate: Coordinate) async {
        let now = clock()
        if isRerouting { return }
        if let lastRerouteAt, now.timeIntervalSince(lastRerouteAt) < Self.rerouteCooldown { return }
        lastRerouteAt = now
        isRerouting = true
        defer { isRerouting = false }
        let request = PlanningRequest(
            origin: Place(id: "current-location", name: String(localized: "My Location"), coordinate: coordinate),
            destination: destination,
            modes: modes,
            departure: .now
        )
        do {
            let plan = try await planner.plan(request)
            let key = option.matchingKey
            guard let next = plan.options.first(where: { $0.matchingKey == key }) ?? plan.options.first else {
                rerouteFailed = true
                return
            }
            rerouteFailed = false
            tracker = NavigationTracker(route: NavigationRoute(option: next), startedAt: now)
        } catch {
            rerouteFailed = true
        }
    }
}
