import SwiftUI

struct NavigationBanner: View {
    let navigation: NavigationModel
    @State private var showsSteps = false
    @State private var stepsHeight: CGFloat = 0

    var body: some View {
        VStack(spacing: 8) {
            VStack(spacing: 0) {
                instruction
                    .contentShape(.rect)
                    .gesture(stepsDrag)
                    .onTapGesture { showsSteps.toggle() }
                if showsSteps {
                    Divider()
                    ScrollView {
                        NavigationStepList(navigation: navigation)
                            .onGeometryChange(for: CGFloat.self) { $0.size.height } action: { stepsHeight = $0 }
                    }
                    .scrollBounceBehavior(.basedOnSize)
                    .frame(height: min(stepsHeight, 460))
                    .transition(.move(edge: .top).combined(with: .opacity))
                }
                Capsule()
                    .fill(.secondary.opacity(0.5))
                    .frame(width: 36, height: 5)
                    .padding(.vertical, 8)
                    .frame(maxWidth: .infinity)
                    .contentShape(.rect)
                    .gesture(stepsDrag)
                    .onTapGesture { showsSteps.toggle() }
            }
            .glassEffect(.regular, in: .rect(cornerRadius: 24))
            .animation(.smooth(duration: 0.3), value: showsSteps)
            if let notice = navigation.notice {
                NoticeCapsule(alert: notice.alert)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    .onTapGesture(perform: navigation.dismissNotice)
            }
        }
        .padding(.horizontal, 12)
        .animation(.smooth, value: navigation.notice)
        .sensoryFeedback(.warning, trigger: navigation.notice) { _, new in new != nil }
        .sensoryFeedback(.selection, trigger: showsSteps)
        .task(id: navigation.notice?.id) {
            guard navigation.notice != nil else { return }
            try? await Task.sleep(for: .seconds(6))
            navigation.dismissNotice()
        }
    }

    private var stepsDrag: some Gesture {
        DragGesture(minimumDistance: 12).onEnded { value in
            if value.translation.height > 30 { showsSteps = true }
            if value.translation.height < -30 { showsSteps = false }
        }
    }

    private var instruction: some View {
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
        .padding(.horizontal, 16)
        .padding(.top, 16)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
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
    let toggle: () -> Void
    let isExpanded: Bool

    var body: some View {
        VStack(spacing: 0) {
            NavigationStatusRow(navigation: navigation)
                .padding(.horizontal, 20)
                .padding(.top, 14)
                .padding(.bottom, 10)
                .contentShape(.rect)
                .onTapGesture(perform: toggle)
                .accessibilityAddTraits(.isButton)
                .accessibilityHint(isExpanded ? "Hides End Route" : "Shows End Route")
            if isExpanded {
                Button(role: .destructive, action: end) {
                    Text(navigation.isFinished ? "Done" : "End Route")
                        .font(.headline)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.glassProminent)
                    .controlSize(.large)
                    .tint(.red)
                    .padding(.horizontal, 20)
                    .padding(.top, 6)
            }
            Spacer(minLength: 0)
        }
    }
}

private struct NavigationStepList: View {
    let navigation: NavigationModel

    private var tracker: NavigationTracker { navigation.tracker }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
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
        }
        .padding(.bottom, 8)
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

private struct NavigationStatusRow: View {
    let navigation: NavigationModel

    var body: some View {
        HStack(alignment: .firstTextBaseline) {
            metric(navigation.estimatedArrival.formatted(Date.FormatStyle(timeZone: .via).hour(.defaultDigits(amPM: .omitted)).minute()), "arrival")
            metric(minutes, "min")
            metric(distance.value, distance.unit)
        }
        .monospacedDigit()
    }

    private var minutes: String {
        let remaining = max(navigation.estimatedArrival.timeIntervalSince(navigation.now), 0)
        return Int((remaining / 60).rounded(.up)).formatted()
    }

    private var distance: (value: String, unit: LocalizedStringKey) {
        let metres = navigation.isFinished ? 0 : navigation.tracker.remainingDistance
        if metres < 1000 {
            return (Int(metres.rounded()).formatted(), "m")
        }
        return ((metres / 1000).formatted(.number.precision(.fractionLength(1))), "km")
    }

    private func metric(_ value: String, _ label: LocalizedStringKey) -> some View {
        VStack(spacing: 0) {
            Text(value)
                .font(.system(size: 30, weight: .bold))
                .lineLimit(1)
                .minimumScaleFactor(0.7)
            Text(label)
                .font(.title3.weight(.medium))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .accessibilityElement(children: .combine)
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
