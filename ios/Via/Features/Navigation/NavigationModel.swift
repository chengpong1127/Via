import Foundation
import Observation

nonisolated struct NavigationNotice: Equatable, Sendable {
    let id: Int
    let alert: NavigationAlert
}

@Observable
final class NavigationModel {
    static let rerouteCooldown: TimeInterval = 60
    static let maneuverAnnouncementDistance = 30.0

    private(set) var tracker: NavigationTracker
    private(set) var notice: NavigationNotice?
    private(set) var isRerouting = false
    private(set) var rerouteFailed = false
    private(set) var now: Date

    @ObservationIgnored let destination: Place
    @ObservationIgnored let modes: Set<PlanningMode>
    @ObservationIgnored private let planner: any JourneyPlanner
    @ObservationIgnored private let location: any LocationTracking
    @ObservationIgnored private let clock: () -> Date
    @ObservationIgnored private let feedback: [any NavigationFeedback]
    @ObservationIgnored private var announcedManeuvers: Set<String> = []
    @ObservationIgnored private var isActive = false
    @ObservationIgnored private var lastRerouteAt: Date?
    @ObservationIgnored private var noticeCount = 0
    @ObservationIgnored private var tasks: [Task<Void, Never>] = []

    init(
        option: JourneyOption,
        destination: Place,
        modes: Set<PlanningMode>,
        planner: any JourneyPlanner,
        location: any LocationTracking,
        feedback: [any NavigationFeedback] = [],
        clock: @escaping () -> Date = Date.init
    ) {
        let now = clock()
        self.now = now
        tracker = NavigationTracker(route: NavigationRoute(option: option), startedAt: now)
        self.destination = destination
        self.modes = modes
        self.planner = planner
        self.location = location
        self.feedback = feedback
        self.clock = clock
    }

    var option: JourneyOption { tracker.route.option }
    var currentLeg: NavigationLeg { tracker.currentLeg }
    var isFinished: Bool { tracker.isFinished }
    var estimatedArrival: Date { isFinished ? now : tracker.estimatedArrival(now: now) }

    func start() {
        guard tasks.isEmpty, !isActive else { return }
        isActive = true
        location.beginNavigation()
        let status = status
        feedback.forEach { $0.start(destination: destination.name, status: status) }
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
        guard isActive else { return }
        isActive = false
        location.endNavigation()
        let status = status
        feedback.forEach { $0.stop(status) }
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
        update.alerts.forEach { send(.alert($0)) }
        if tracker.isFinished {
            stop()
            return
        }
        announceManeuverIfNear()
        if update.reroute != nil, let coordinate {
            await reroute(from: coordinate)
        }
        let status = status
        feedback.forEach { $0.update(status) }
    }

    private func announceManeuverIfNear() {
        guard let maneuver = tracker.nextManeuver, maneuver.guidance.code != "depart",
              let distance = tracker.distanceToManeuver, distance <= Self.maneuverAnnouncementDistance
        else { return }
        let key = "\(option.id)-\(tracker.legIndex)-\(maneuver.distanceAlong)"
        guard announcedManeuvers.insert(key).inserted else { return }
        send(.maneuver(maneuver.guidance.spokenInstruction))
    }

    private func send(_ cue: NavigationCue) {
        feedback.forEach { $0.cue(cue) }
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
                send(.rerouteFailed)
                return
            }
            rerouteFailed = false
            tracker = NavigationTracker(route: NavigationRoute(option: next), startedAt: now)
            send(.rerouted)
        } catch {
            rerouteFailed = true
            send(.rerouteFailed)
        }
    }
}
