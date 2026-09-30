import SwiftUI

struct JourneyPlanningStateContent: View {
    let state: JourneySession.PresentationState
    let canSubmit: Bool
    let refreshMessage: String?
    let options: [JourneyOption]
    let selectedOptionID: String?
    let onSelect: (JourneyOption) -> Void
    let onShowDetails: (JourneyOption) -> Void
    let onEdit: () -> Void
    let onSubmit: () -> Void

    @ViewBuilder
    var body: some View {
        switch state {
        case .editing:
            primaryAction
        case .loading:
            LoadingView()
        case .results, .refreshing:
            if let refreshMessage {
                Label(refreshMessage, systemImage: "exclamationmark.arrow.triangle.2.circlepath")
                    .font(.caption)
                    .foregroundStyle(.orange)
            }
            Text("Journey Options")
                .font(.subheadline.bold())
            ForEach(options) { option in
                JourneyOptionCard(
                    option: option,
                    isSelected: selectedOptionID == option.id,
                    onSelect: { onSelect(option) },
                    onShowDetails: { onShowDetails(option) }
                )
            }
        case .empty:
            MessageStateView(
                symbol: "signpost.right.and.left",
                title: "No matching Journey Options",
                message: "Try another transit combination.",
                actionTitle: "Edit request",
                action: onEdit
            )
        case .invalidRequest(let field):
            MessageStateView(
                symbol: "exclamationmark.circle",
                title: "Check your request",
                message: "A required field needs attention: \(field).",
                actionTitle: "Edit request",
                action: onEdit
            )
        case .locationPermissionDenied:
            MessageStateView(
                symbol: "location.slash",
                title: "Location access is turned off",
                message: "Via needs your location to plan from Current Location. Choose another starting point or allow location access in Settings.",
                actionTitle: "Edit request",
                action: onEdit
            )
        case .locationRestricted:
            MessageStateView(
                symbol: "location.slash",
                title: "Location access is unavailable",
                message: "Via cannot use Current Location on this device. Choose another starting point.",
                actionTitle: "Edit request",
                action: onEdit
            )
        case .locationUnavailable:
            MessageStateView(
                symbol: "location.slash",
                title: "Current location unavailable",
                message: "We couldn't get your current location. Try again or choose a starting point manually.",
                actionTitle: "Try again",
                action: onSubmit
            )
        case .temporarilyUnavailable:
            MessageStateView(
                symbol: "wifi.exclamationmark",
                title: "Journey Planning is temporarily unavailable",
                message: "Via could not get current realtime information. Try again.",
                actionTitle: "Try again",
                action: onSubmit
            )
        case .unexpectedFailure:
            MessageStateView(
                symbol: "exclamationmark.triangle",
                title: "Something went wrong",
                message: "No partial result was shown because it could not be trusted.",
                actionTitle: "Try again",
                action: onSubmit
            )
        }
    }

    private var primaryAction: some View {
        Button(action: onSubmit) {
            Text("Find Journey Options")
                .font(.headline)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 13)
        }
        .buttonStyle(.borderedProminent)
        .tint(Color.viaBlue)
        .disabled(!canSubmit)
    }
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
