import SwiftUI

struct ModeBadge: View {
    let mode: JourneyMode
    var compact = false

    var body: some View {
        Label {
            if !compact {
                Text(LocalizedStringKey(mode.titleKey))
            }
        } icon: {
            Image(systemName: mode.symbolName)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(.white)
        .padding(.horizontal, compact ? 6 : 9)
        .frame(minWidth: 28, minHeight: 27)
        .background(color, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        .accessibilityLabel(Text(LocalizedStringKey(mode.titleKey)))
    }

    private var color: Color {
        switch mode {
        case .transit: .purple
        case .walking: .gray
        }
    }
}

extension Color {
    static let viaBlue = Color(red: 0.03, green: 0.47, blue: 1)
    static let viaInk = Color(red: 0.09, green: 0.10, blue: 0.12)
}
