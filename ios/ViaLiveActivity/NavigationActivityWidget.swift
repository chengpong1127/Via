import ActivityKit
import SwiftUI
import WidgetKit

struct NavigationActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: NavigationActivityAttributes.self) { context in
            LockScreenView(context: context)
                .activityBackgroundTint(Color(.systemBackground).opacity(0.85))
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    Image(systemName: context.state.symbol)
                        .font(.title)
                        .foregroundStyle(.blue)
                        .padding(.leading, 4)
                }
                DynamicIslandExpandedRegion(.trailing) {
                    ArrivalLabel(state: context.state)
                        .padding(.trailing, 4)
                }
                DynamicIslandExpandedRegion(.center) {
                    Text(context.state.title)
                        .font(.headline)
                        .lineLimit(2)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    Text(context.state.detail)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            } compactLeading: {
                Image(systemName: context.state.symbol)
                    .foregroundStyle(.blue)
            } compactTrailing: {
                Text(context.state.detail)
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                    .frame(maxWidth: 64)
            } minimal: {
                Image(systemName: context.state.symbol)
                    .foregroundStyle(.blue)
            }
        }
    }
}

private struct LockScreenView: View {
    let context: ActivityViewContext<NavigationActivityAttributes>

    var body: some View {
        HStack(alignment: .center, spacing: 14) {
            Image(systemName: context.state.symbol)
                .font(.system(size: 30, weight: .semibold))
                .foregroundStyle(context.state.isFinished ? .red : .blue)
                .frame(width: 44)
            VStack(alignment: .leading, spacing: 2) {
                Text(context.state.title)
                    .font(.headline)
                    .lineLimit(2)
                Text(context.state.detail)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            ArrivalLabel(state: context.state)
        }
        .padding(16)
    }
}

private struct ArrivalLabel: View {
    let state: NavigationActivityAttributes.ContentState

    var body: some View {
        VStack(alignment: .trailing, spacing: 0) {
            Text(state.arrival, style: .time)
                .font(.headline)
                .monospacedDigit()
            Text(state.isFinished ? "Arrived" : "arrival")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
