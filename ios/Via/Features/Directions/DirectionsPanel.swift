import SwiftUI

struct DirectionsPanel: View {
    @Bindable var model: ExploreModel
    @State private var editingModes = false

    private var directions: DirectionsModel { model.directions }

    var body: some View {
        VStack(spacing: 0) {
            SheetHeader(title: "Directions", close: model.closeDirections)
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    EndpointCard(model: model)
                    controls
                    results
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 24)
            }
            .scrollBounceBehavior(.basedOnSize)
            .refreshable { await directions.plan() }
        }
        .sheet(isPresented: $editingModes) {
            ModePicker(modes: directions.modes) { modes in
                guard modes != directions.modes else { return }
                directions.modes = modes
                model.replan()
            }
            .presentationDetents([.medium])
            .presentationDragIndicator(.visible)
        }
    }

    private var controls: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 8) {
                ModeButton(modes: directions.modes) {
                    editingModes = true
                }
            }
            .padding(.vertical, 2)
        }
        .scrollIndicators(.hidden)
        .scrollClipDisabled()
    }

    @ViewBuilder
    private var results: some View {
        switch directions.phase {
        case .idle, .loading:
            VStack(spacing: 10) {
                ProgressView()
                Text("Finding routes…")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 32)
        case let .loaded(plan):
            if plan.options.isEmpty {
                ContentUnavailableView(
                    "No Routes Found",
                    systemImage: "point.topleft.down.to.point.bottomright.curvepath",
                    description: Text("Try other transport modes or a different time.")
                )
            } else {
                LazyVStack(spacing: 10) {
                    ForEach(plan.options) { option in
                        JourneyOptionRow(
                            option: option,
                            isSelected: option.id == directions.selectedOption?.id,
                            select: { model.select(option) },
                            showDetails: { model.showJourney(option) },
                            start: model.canNavigate ? { model.startNavigation(option) } : nil
                        )
                    }
                }
                .opacity(directions.isRefreshing ? 0.5 : 1)
                .overlay(alignment: .top) {
                    if directions.isRefreshing { ProgressView().padding(.top, 24) }
                }
                Text("Updated \(plan.generatedAt.formatted(.clockTime))")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
                    .padding(.top, 4)
            }
        case let .failed(failure):
            FailureView(failure: failure) { model.replan() }
        }
    }
}

private struct EndpointCard: View {
    let model: ExploreModel

    var body: some View {
        HStack(spacing: 8) {
            VStack(spacing: 0) {
                row(
                    endpoint: model.directions.origin,
                    placeholder: "Choose Start",
                    symbol: "circle.circle.fill",
                    tint: .blue,
                    role: .origin
                )
                Divider().padding(.leading, 40)
                row(
                    endpoint: model.directions.destination,
                    placeholder: "Choose Destination",
                    symbol: "mappin.circle.fill",
                    tint: .red,
                    role: .destination
                )
            }
            Button("Swap", systemImage: "arrow.up.arrow.down") {
                model.swapEndpoints()
            }
            .labelStyle(.iconOnly)
            .font(.body.weight(.semibold))
            .buttonStyle(.glass)
            .buttonBorderShape(.circle)
            .disabled(model.directions.destination == nil)
        }
        .padding(.leading, 12)
        .padding(.trailing, 10)
        .padding(.vertical, 4)
        .background(.fill.quaternary, in: .rect(cornerRadius: 16))
    }

    private func row(
        endpoint: JourneyEndpoint?,
        placeholder: LocalizedStringKey,
        symbol: String,
        tint: Color,
        role: EndpointRole
    ) -> some View {
        Button {
            model.pickEndpoint(role)
        } label: {
            HStack(spacing: 12) {
                Image(systemName: endpoint == .currentLocation ? "location.circle.fill" : symbol)
                    .font(.title2)
                    .foregroundStyle(tint)
                VStack(alignment: .leading, spacing: 1) {
                    Text(role == .origin ? "From" : "To")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Group {
                        switch endpoint {
                        case .currentLocation: Text("My Location").foregroundStyle(.blue)
                        case let .place(place): Text(place.name)
                        case nil: Text(placeholder).foregroundStyle(.secondary)
                        }
                    }
                    .lineLimit(1)
                }
                Spacer(minLength: 0)
            }
            .padding(.vertical, 9)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
    }
}

struct JourneyOptionRow: View {
    let option: JourneyOption
    let isSelected: Bool
    let select: () -> Void
    let showDetails: () -> Void
    var start: (() -> Void)?

    var body: some View {
        Button(action: isSelected ? showDetails : select) {
            VStack(alignment: .leading, spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(option.duration.travelTime)
                            .font(.title2.bold())
                            .monospacedDigit()
                        if let label = option.label {
                            Text(label.title)
                                .font(.caption.weight(.semibold))
                                .padding(.horizontal, 7)
                                .padding(.vertical, 2)
                                .foregroundStyle(.green)
                                .background(.green.opacity(0.15), in: .capsule)
                        }
                    }
                    if let boarding = firstRide {
                        boardingText(boarding)
                    }
                    Text("\(option.arrivalTime.formatted(.clockTime)) ETA")
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                .font(.subheadline)
                .lineLimit(1)
                .minimumScaleFactor(0.8)
                .padding(.trailing, start == nil ? 0 : 92)
                LegStrip(legs: option.legs)
            }
            .padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.background.secondary, in: .rect(cornerRadius: 20))
            .overlay {
                RoundedRectangle(cornerRadius: 20)
                    .strokeBorder(isSelected ? Color.accentColor : .clear, lineWidth: 2)
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .overlay(alignment: .topTrailing) {
            if let start {
                Button(action: start) {
                    Text("GO")
                        .font(.title2.weight(.heavy))
                        .foregroundStyle(.white)
                        .frame(width: 76, height: 60)
                        .background(.green, in: .rect(cornerRadius: 14))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Start Navigation")
                .padding(16)
            }
        }
        .accessibilityAddTraits(isSelected ? .isSelected : [])
    }

    private var firstRide: (ride: TransitRide, departure: Date)? {
        for leg in option.legs {
            if case let .transit(ride) = leg.kind { return (ride, leg.departureTime) }
        }
        return nil
    }

    private func boardingText(_ boarding: (ride: TransitRide, departure: Date)) -> some View {
        let mode = String(localized: boarding.ride.mode.title)
        let time = boarding.departure.formatted(.clockTime)
        return Group {
            if option.liveStatus == .fresh {
                Text("\(mode) departs at \(time)")
                    .foregroundStyle(.green)
            } else {
                Text("\(mode) scheduled at \(time)")
                    .foregroundStyle(.secondary)
            }
        }
        .monospacedDigit()
    }
}

struct LiveBadge: View {
    let status: LiveStatus

    var body: some View {
        switch status {
        case .fresh:
            Label("Live", systemImage: "dot.radiowaves.left.and.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.green)
                .symbolEffect(.variableColor.iterative, options: .repeating)
        case .degraded:
            Label("Delayed data", systemImage: "exclamationmark.triangle.fill")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.orange)
        case .unavailable:
            Label("Scheduled", systemImage: "clock")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
        case .notApplicable:
            EmptyView()
        }
    }
}

private struct FailureView: View {
    let failure: DirectionsFailure
    let retry: () -> Void
    @Environment(\.openURL) private var openURL

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(message)
        } actions: {
            if failure == .location(.denied) {
                Button("Open Settings") {
                    openURL(URL(string: "app-settings:")!)
                }
                .buttonStyle(.glassProminent)
            } else if failure != .sameEndpoints {
                Button("Try Again", action: retry)
                    .buttonStyle(.glassProminent)
            }
        }
    }

    private var title: LocalizedStringKey {
        switch failure {
        case .location(.denied), .location(.restricted): "Location Access Is Off"
        case .location(.unavailable): "Location Unavailable"
        case .sameEndpoints: "Already There"
        case .planning(.timedOut): "Taking Too Long"
        case .planning(.serverUnreachable): "Can't Reach Via"
        case .planning(.temporarilyUnavailable): "Routing Unavailable"
        case .planning: "Directions Not Available"
        }
    }

    private var message: LocalizedStringKey {
        switch failure {
        case .location(.denied): "Allow location access in Settings, or choose a start point."
        case .location(.restricted): "Location can't be used on this device. Choose a start point instead."
        case .location(.unavailable): "Your location couldn't be found. Try again or choose a start point."
        case .sameEndpoints: "The start and destination are the same place."
        case .planning(.timedOut): "Planning took too long. Try again."
        case .planning(.serverUnreachable): "Check your connection and try again."
        case .planning(.temporarilyUnavailable): "Live routing data is temporarily unavailable. Try again in a moment."
        case .planning(.invalidRequest): "These places can't be routed. Try a different start or destination."
        case .planning: "Something went wrong while planning. Try again."
        }
    }

    private var symbol: String {
        switch failure {
        case .location: "location.slash"
        case .sameEndpoints: "mappin.and.ellipse"
        case .planning(.serverUnreachable): "wifi.exclamationmark"
        default: "exclamationmark.triangle"
        }
    }
}
