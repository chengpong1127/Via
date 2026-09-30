import MapKit
import SwiftUI

struct DirectionsPanel: View {
    @Environment(JourneySession.self) private var session
    @State private var searchTarget: SearchTarget?

    let searchRegion: MKCoordinateRegion
    let onShowDetails: (JourneyOption) -> Void

    var body: some View {
        VStack(spacing: 0) {
            Capsule()
                .fill(.tertiary)
                .frame(width: 38, height: 5)
                .padding(.top, 9)
                .padding(.bottom, 6)

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    header
                    modeFilters
                    locationFields
                    departureRow
                    JourneyPlanningStateContent(
                        state: session.state,
                        canSubmit: session.canSubmit,
                        refreshMessage: session.refreshMessage,
                        options: session.result?.options ?? [],
                        selectedOptionID: session.selectedOptionID,
                        onSelect: session.select,
                        onShowDetails: onShowDetails,
                        onEdit: session.returnToEditing,
                        onSubmit: { Task { await session.submit() } }
                    )
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 28)
            }
            .refreshable {
                await session.refresh()
            }
        }
        .background(.ultraThickMaterial)
        .clipShape(UnevenRoundedRectangle(topLeadingRadius: 28, topTrailingRadius: 28))
        .shadow(color: .black.opacity(0.16), radius: 24, y: -5)
        .sheet(item: $searchTarget) { target in
            LocationSearchView(
                title: target.title,
                searchRegion: searchRegion,
                allowsCurrentLocation: target == .origin
            ) { endpoint in
                switch target {
                case .origin:
                    session.origin = endpoint
                case .destination:
                    if case .place(let place) = endpoint {
                        session.destination = place
                    }
                }
                session.returnToEditing()
            }
        }
    }

    private var header: some View {
        HStack {
            Text("Directions")
                .font(.title2.bold())
                .foregroundStyle(Color.viaInk)
            Spacer()
            if session.state == .refreshing {
                ProgressView()
                    .accessibilityLabel("Refreshing Journey Options")
            }
        }
    }

    private var modeFilters: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 8) {
                ForEach(PlanningMode.selectable) { mode in
                    Button {
                        session.toggle(mode)
                        session.returnToEditing()
                    } label: {
                        Label(LocalizedStringKey(mode.titleKey), systemImage: mode.symbolName)
                            .font(.subheadline.weight(.semibold))
                            .padding(.horizontal, 12)
                            .padding(.vertical, 9)
                            .foregroundStyle(session.selectedModes.contains(mode) ? Color.viaBlue : .secondary)
                            .background(
                                session.selectedModes.contains(mode) ? Color.viaBlue.opacity(0.12) : Color.secondary.opacity(0.08),
                                in: Capsule()
                            )
                            .overlay {
                                Capsule().stroke(session.selectedModes.contains(mode) ? Color.viaBlue.opacity(0.45) : .clear)
                            }
                    }
                    .buttonStyle(.plain)
                    .accessibilityValue(session.selectedModes.contains(mode) ? "Selected" : "Not selected")
                }
            }
        }
    }

    private var locationFields: some View {
        VStack(spacing: 0) {
            locationButton(target: .origin, endpoint: session.origin, symbol: "location.fill", tint: Color.viaBlue)
            Divider().padding(.leading, 40)
            locationButton(target: .destination, endpoint: session.destination.map { .place($0) }, symbol: "diamond.fill", tint: .brown)
        }
        .padding(.horizontal, 12)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 17, style: .continuous))
    }

    private func locationButton(target: SearchTarget, endpoint: JourneyEndpoint?, symbol: String, tint: Color) -> some View {
        Button {
            searchTarget = target
        } label: {
            HStack(spacing: 12) {
                Image(systemName: symbol)
                    .font(.caption)
                    .foregroundStyle(.white)
                    .frame(width: 24, height: 24)
                    .background(tint, in: Circle())
                VStack(alignment: .leading, spacing: 1) {
                    Text(target.title)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(endpointTitle(endpoint) ?? target.placeholder)
                        .foregroundStyle(endpoint == nil ? .secondary : .primary)
                        .lineLimit(1)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.caption.bold())
                    .foregroundStyle(.tertiary)
            }
            .frame(minHeight: 49)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private var departureRow: some View {
        HStack {
            Menu {
                Button("Leave now") {
                    session.departureSetting = .departNow
                    session.returnToEditing()
                }
                Button("Leave in 15 minutes") {
                    session.departureSetting = .departAt(.now.addingTimeInterval(900))
                    session.returnToEditing()
                }
                Button("Arrive in 45 minutes") {
                    session.departureSetting = .arriveBy(.now.addingTimeInterval(2_700))
                    session.returnToEditing()
                }
            } label: {
                Label(departureTitle, systemImage: "clock")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(Color.secondary.opacity(0.1), in: Capsule())
            }

            Spacer()

            if let option = session.selectedOption, option.liveStatusSummary.state == .fresh {
                Circle()
                    .fill(.green)
                    .frame(width: 10, height: 10)
                    .shadow(color: .green.opacity(0.35), radius: 0, x: 0, y: 0)
                    .padding(5)
                    .background(.green.opacity(0.14), in: Circle())
                    .accessibilityLabel("Live Status available")
            }
        }
    }

    private var primaryAction: some View {
        Button {
            Task { await session.submit() }
        } label: {
            Text("Find Journey Options")
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 13)
        }
        .buttonStyle(.borderedProminent)
        .tint(Color.viaBlue)
        .disabled(!session.canSubmit)
    }

    private var departureTitle: String {
        switch session.departureSetting {
        case .departNow: String(localized: "Leave now")
        case .departAt(let date): String(localized: "Leave at \(date.formatted(date: .omitted, time: .shortened))")
        case .arriveBy(let date): String(localized: "Arrive by \(date.formatted(date: .omitted, time: .shortened))")
        }
    }

    private func endpointTitle(_ endpoint: JourneyEndpoint?) -> String? {
        guard let endpoint else { return nil }
        switch endpoint {
        case .currentLocation:
            return String(localized: "Current Location")
        case .place(let place):
            return place.displayText
        }
    }
}

private enum SearchTarget: String, Identifiable, Equatable {
    case origin
    case destination

    var id: Self { self }
    var title: String { self == .origin ? String(localized: "From") : String(localized: "To") }
    var placeholder: String { self == .origin ? String(localized: "Choose a starting point") : String(localized: "Choose a destination") }
}

private struct LoadingView: View {
    var body: some View {
        HStack(spacing: 12) {
            ProgressView()
            VStack(alignment: .leading, spacing: 2) {
                Text("Finding Journey Options…").font(.headline)
                Text("Checking schedules and realtime availability").font(.caption).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 18)
        .accessibilityElement(children: .combine)
    }
}

private struct MessageStateView: View {
    let symbol: String
    let title: LocalizedStringKey
    let message: LocalizedStringKey
    let actionTitle: LocalizedStringKey
    let action: () -> Void

    var body: some View {
        ContentUnavailableView {
            Label(title, systemImage: symbol)
        } description: {
            Text(message)
        } actions: {
            Button(actionTitle, action: action)
                .buttonStyle(.borderedProminent)
        }
    }
}
