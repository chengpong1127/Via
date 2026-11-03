import SwiftUI

struct NavigationBanner: View {
    let navigation: NavigationModel

    var body: some View {
        VStack(spacing: 8) {
            HStack(alignment: .center, spacing: 14) {
                Image(systemName: symbol)
                    .font(.system(size: 34, weight: .semibold))
                    .frame(width: 48)
                    .foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 2) {
                    headline
                        .font(.title2.bold())
                        .lineLimit(2)
                        .minimumScaleFactor(0.7)
                    subtitle
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
                Spacer(minLength: 0)
                if navigation.isRerouting {
                    ProgressView()
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .glassEffect(.regular, in: .rect(cornerRadius: 24))
            if let notice = navigation.notice {
                NoticeCapsule(alert: notice.alert)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    .onTapGesture(perform: navigation.dismissNotice)
            }
        }
        .padding(.horizontal, 12)
        .animation(.smooth, value: navigation.notice)
        .sensoryFeedback(.warning, trigger: navigation.notice) { _, new in new != nil }
        .task(id: navigation.notice?.id) {
            guard navigation.notice != nil else { return }
            try? await Task.sleep(for: .seconds(6))
            navigation.dismissNotice()
        }
    }

    private var tracker: NavigationTracker { navigation.tracker }
    private var leg: Leg { navigation.currentLeg.leg }

    private var symbol: String {
        if navigation.isFinished { return "mappin.circle.fill" }
        if navigation.isRerouting { return "arrow.triangle.branch" }
        switch leg.kind {
        case let .transit(ride):
            return tracker.boarded ? ride.mode.symbol : "figure.walk.arrival"
        case .walking, .bikeShare:
            return tracker.nextManeuver?.guidance.navigationSymbol ?? "arrow.up"
        }
    }

    private var tint: Color {
        navigation.isFinished ? .red : leg.kind.routeColor
    }

    @ViewBuilder
    private var headline: some View {
        if navigation.isFinished {
            Text("You've arrived")
        } else if navigation.isRerouting {
            Text("Rerouting…")
        } else {
            switch leg.kind {
            case let .transit(ride):
                if !tracker.boarded {
                    Text("Board \(ride.serviceLabel)")
                } else if let stops = tracker.stopsRemaining, stops <= 1 {
                    Text("Get off next stop")
                } else if let stops = tracker.stopsRemaining {
                    Text("^[\(stops) stop](inflect: true) left")
                }
            case .walking, .bikeShare:
                if let distance = tracker.distanceToManeuver {
                    Text(distance.navigationDistance)
                }
            }
        }
    }

    @ViewBuilder
    private var subtitle: some View {
        if navigation.isFinished {
            Text(navigation.destination.name)
        } else if navigation.rerouteFailed {
            Text("Couldn't update the route. Keep following the current one.")
        } else {
            switch leg.kind {
            case let .transit(ride):
                if tracker.boarded {
                    Text("Get off at \(leg.to.name)")
                } else if ride.headsign.isEmpty {
                    Text("At \(leg.from.name) · \(leg.departureTime.formatted(.clockTime))")
                } else {
                    Text("Toward \(ride.headsign) · \(leg.from.name) · \(leg.departureTime.formatted(.clockTime))")
                }
            case .walking, .bikeShare:
                if let maneuver = tracker.nextManeuver {
                    maneuver.guidance.code == "arrive" ? arrival : maneuver.guidance.detailedInstruction
                }
            }
        }
    }

    private var arrival: Text {
        switch (leg.kind, navigation.tracker.nextLeg?.leg.kind) {
        case let (.bikeShare(ride), _):
            Text("Return the bike at \(ride.dropoffStationName)")
        case (_, .transit?):
            Text("Arrive at \(leg.to.name)")
        case let (_, .bikeShare(ride)?):
            Text("Pick up a YouBike at \(ride.pickupStationName)")
        default:
            Text("Arrive at \(navigation.destination.name)")
        }
    }
}

private struct NoticeCapsule: View {
    let alert: NavigationAlert

    var body: some View {
        Label {
            message
                .font(.headline)
        } icon: {
            Image(systemName: symbol)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .foregroundStyle(.white)
        .background(color, in: .capsule)
    }

    private var message: Text {
        switch alert {
        case .leaveNow: Text("Time to leave")
        case .hurry: Text("Hurry to make your connection")
        case let .stopsLeft(count): Text("^[\(count) stop](inflect: true) left")
        case let .nextStop(name): Text("Next stop: \(name)")
        case let .getOff(name): Text("Get off at \(name)")
        case .arrived: Text("You've arrived")
        }
    }

    private var symbol: String {
        switch alert {
        case .leaveNow: "figure.walk.departure"
        case .hurry: "hare.fill"
        case .stopsLeft, .nextStop: "bell.fill"
        case .getOff: "figure.walk.arrival"
        case .arrived: "mappin.circle.fill"
        }
    }

    private var color: Color {
        switch alert {
        case .hurry: .orange
        case .getOff, .nextStop: .red
        default: .blue
        }
    }
}

struct NavigationSheet: View {
    let navigation: NavigationModel
    let end: () -> Void
    let isExpanded: Bool

    private var tracker: NavigationTracker { navigation.tracker }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                CurrentStageCard(navigation: navigation, end: end)
                    .padding(.horizontal, 20)
                    .padding(.top, 22)
                    .padding(.bottom, 18)
                if isExpanded {
                    allSteps
                }
            }
        }
        .scrollBounceBehavior(.basedOnSize)
        .scrollDisabled(!isExpanded)
    }

    @ViewBuilder
    private var allSteps: some View {
        Divider()
        summary
            .padding(.horizontal, 20)
            .padding(.vertical, 16)
        ForEach(Array(tracker.route.legs.enumerated()), id: \.element.leg.id) { index, item in
            NavigationStepRows(
                item: item,
                state: state(of: index),
                stopsRemaining: index == tracker.legIndex ? tracker.stopsRemaining : nil
            )
        }
        NavigationStepRow(
            title: Text("Arrive at \(navigation.destination.name)"),
            subtitle: Text(navigation.option.arrivalTime.formatted(.clockTime)),
            state: navigation.isFinished ? .current : .upcoming
        ) {
            Image(systemName: "mappin.circle.fill")
                .font(.title2)
                .foregroundStyle(.red)
        }
        Button(navigation.isFinished ? "Done" : "End Route", role: .destructive, action: end)
            .font(.headline)
            .frame(maxWidth: .infinity)
            .buttonStyle(.glassProminent)
            .controlSize(.large)
            .tint(.red)
            .padding(20)
    }

    private var summary: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(navigation.destination.name)
                .font(.title2.bold())
                .lineLimit(2)
            Group {
                if navigation.isFinished {
                    Text("Arrived at \(navigation.estimatedArrival.formatted(.clockTime))")
                } else {
                    Text("Arrive \(navigation.estimatedArrival.formatted(.clockTime)) · \(remaining.travelTime)")
                }
            }
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .monospacedDigit()
        }
    }

    private var remaining: TimeInterval {
        max(navigation.estimatedArrival.timeIntervalSince(navigation.now), 0)
    }

    private func state(of index: Int) -> NavigationStepState {
        if navigation.isFinished || index < tracker.legIndex { return .done }
        return index == tracker.legIndex ? .current : .upcoming
    }
}

private struct CurrentStageCard: View {
    let navigation: NavigationModel
    let end: () -> Void

    private var tracker: NavigationTracker { navigation.tracker }
    private var leg: Leg { navigation.currentLeg.leg }

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            icon
                .frame(minWidth: 44)
                .padding(.top, 4)
            VStack(alignment: .leading, spacing: 4) {
                title
                    .font(.title2.bold())
                    .lineLimit(2)
                    .minimumScaleFactor(0.8)
                subtitle
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                detail
                    .font(.body)
                    .monospacedDigit()
                    .padding(.top, 4)
            }
            Spacer(minLength: 8)
            CloseButton(action: end)
                .accessibilityLabel(navigation.isFinished ? Text("Done") : Text("End Route"))
        }
    }

    @ViewBuilder
    private var icon: some View {
        if navigation.isFinished {
            Image(systemName: "mappin.circle.fill")
                .font(.largeTitle)
                .foregroundStyle(.red)
        } else {
            switch leg.kind {
            case let .transit(ride):
                ServiceBadge(ride: ride)
            case .walking:
                Image(systemName: "figure.walk")
                    .font(.largeTitle)
            case .bikeShare:
                Image(systemName: "bicycle")
                    .font(.title)
                    .foregroundStyle(Color.bikeShare)
            }
        }
    }

    @ViewBuilder
    private var title: some View {
        if navigation.isFinished {
            Text("You've arrived")
        } else {
            switch leg.kind {
            case let .transit(ride):
                if tracker.boarded {
                    Text("Exit at \(leg.to.name)")
                } else {
                    Text("Board \(String(localized: ride.mode.title)) \(ride.serviceLabel)")
                }
            case .walking:
                Text("Walk to \(leg.to.name)")
            case let .bikeShare(ride):
                Text("Ride YouBike to \(ride.dropoffStationName)")
            }
        }
    }

    @ViewBuilder
    private var subtitle: some View {
        if navigation.isFinished {
            Text(navigation.destination.name)
        } else {
            switch leg.kind {
            case let .transit(ride):
                if tracker.boarded, let stops = tracker.stopsRemaining {
                    Text("^[\(stops) stop](inflect: true) left")
                } else if !ride.headsign.isEmpty {
                    Text("Towards \(ride.headsign)")
                } else {
                    Text("At \(leg.from.name)")
                }
            case .walking, .bikeShare:
                Text("\(tracker.remainingLegDistance.distanceText), about \(remainingLegTime.travelTime)")
            }
        }
    }

    @ViewBuilder
    private var detail: some View {
        if !navigation.isFinished {
            switch leg.kind {
            case .transit:
                if tracker.boarded {
                    Text("Arrives at \(leg.arrivalTime.formatted(.clockTime))")
                } else {
                    Text("Departs at \(leg.departureTime.formatted(.clockTime))")
                }
            case .walking, .bikeShare:
                if let next = tracker.nextLeg?.leg, let ride = next.transit {
                    HStack(spacing: 8) {
                        ServiceBadge(ride: ride)
                        Text("Departs at \(next.departureTime.formatted(.clockTime))")
                            .lineLimit(2)
                    }
                }
            }
        }
    }

    private var remainingLegTime: TimeInterval {
        let length = navigation.currentLeg.length
        return length > 0 ? leg.duration * tracker.remainingLegDistance / length : 0
    }
}

private struct NavigationStepRows: View {
    let item: NavigationLeg
    let state: NavigationStepState
    let stopsRemaining: Int?

    private var leg: Leg { item.leg }

    var body: some View {
        switch leg.kind {
        case .walking:
            NavigationStepRow(
                title: Text("Walk to \(leg.to.name)"),
                subtitle: Text("\(item.length.distanceText), about \(leg.duration.travelTime)"),
                state: state
            ) {
                Image(systemName: "figure.walk")
                    .font(.title)
            }
        case let .bikeShare(ride):
            NavigationStepRow(
                title: Text("Ride YouBike to \(ride.dropoffStationName)"),
                subtitle: Text("\(item.length.distanceText), about \(leg.duration.travelTime)"),
                state: state
            ) {
                Image(systemName: "bicycle")
                    .font(.title2)
                    .foregroundStyle(Color.bikeShare)
            }
        case let .transit(ride):
            NavigationStepRow(
                title: Text("Board \(String(localized: ride.mode.title)) \(ride.serviceLabel)"),
                subtitle: ride.headsign.isEmpty ? Text("At \(leg.from.name)") : Text("Towards \(ride.headsign)"),
                detail: Text("Departs at \(leg.departureTime.formatted(.clockTime))"),
                state: state
            ) {
                ServiceBadge(ride: ride)
            }
            NavigationStepRow(
                title: Text("Exit at \(leg.to.name)"),
                subtitle: nil,
                state: state
            ) {
                Image(systemName: "rectangle.portrait.and.arrow.right")
                    .font(.title2)
            } extra: {
                RideTimeline(leg: leg, ride: ride, stopsRemaining: stopsRemaining)
            }
        }
    }
}

private struct RideTimeline: View {
    let leg: Leg
    let ride: TransitRide
    let stopsRemaining: Int?

    var body: some View {
        let color = ride.lineStyle.background
        VStack(alignment: .leading, spacing: 6) {
            stop(leg.from.name, leg.departureTime, color: color)
            HStack(spacing: 12) {
                VStack(spacing: 5) {
                    ForEach(0..<3, id: \.self) { _ in
                        Circle().fill(color).frame(width: 5, height: 5)
                    }
                }
                .frame(width: 16)
                Group {
                    if let stopsRemaining {
                        Text("^[\(stopsRemaining) stop](inflect: true) left")
                    } else {
                        Text("Go ^[\(ride.intermediateStops.count + 1) stop](inflect: true), \(leg.duration.travelTime)")
                    }
                }
                .font(.subheadline)
                .foregroundStyle(Color.accentColor)
            }
            stop(leg.to.name, leg.arrivalTime, color: color)
        }
        .padding(.top, 6)
    }

    private func stop(_ name: String, _ time: Date, color: Color) -> some View {
        HStack(spacing: 12) {
            Circle()
                .strokeBorder(color, lineWidth: 4)
                .frame(width: 16, height: 16)
            Text(name)
                .font(.subheadline.weight(.semibold))
                .lineLimit(1)
            Spacer(minLength: 8)
            Text(time.formatted(.clockTime))
                .font(.subheadline)
                .monospacedDigit()
                .foregroundStyle(.secondary)
        }
    }
}

private enum NavigationStepState {
    case done
    case current
    case upcoming
}

private struct NavigationStepRow<Extra: View>: View {
    let title: Text
    let subtitle: Text?
    var detail: Text?
    let state: NavigationStepState
    @ViewBuilder let icon: () -> AnyView
    @ViewBuilder let extra: () -> Extra

    init<Icon: View>(
        title: Text,
        subtitle: Text?,
        detail: Text? = nil,
        state: NavigationStepState,
        @ViewBuilder icon: @escaping () -> Icon,
        @ViewBuilder extra: @escaping () -> Extra = { EmptyView() }
    ) {
        self.title = title
        self.subtitle = subtitle
        self.detail = detail
        self.state = state
        self.icon = { AnyView(icon()) }
        self.extra = extra
    }

    var body: some View {
        HStack(alignment: .top, spacing: 14) {
            icon()
                .frame(minWidth: 44)
                .padding(.top, 2)
            VStack(alignment: .leading, spacing: 3) {
                title
                    .font(.title3.bold())
                subtitle?
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                detail?
                    .font(.subheadline)
                    .monospacedDigit()
                    .padding(.top, 4)
                extra()
                Divider()
                    .padding(.top, 12)
            }
        }
        .padding(.horizontal, 20)
        .padding(.top, 14)
        .background(state == .current ? AnyShapeStyle(.fill.quaternary) : AnyShapeStyle(.clear))
        .opacity(state == .done ? 0.45 : 1)
    }
}

extension Guidance {
    var detailedInstruction: Text {
        guard let street = streetName, code != "arrive", code != "depart" else { return Text(instruction) }
        return Text("\(Text(instruction)) onto \(street)")
    }

    var navigationSymbol: String {
        code == "arrive" ? "mappin.and.ellipse" : symbol
    }
}

extension Double {
    var navigationDistance: String {
        let rounded = self < 100 ? (self / 5).rounded() * 5 : (self / 10).rounded() * 10
        return Measurement(value: rounded, unit: UnitLength.meters)
            .formatted(.measurement(width: .abbreviated, usage: .road))
    }
}
