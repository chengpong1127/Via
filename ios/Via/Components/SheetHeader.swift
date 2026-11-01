import SwiftUI

struct SheetHeader<Subtitle: View>: View {
    let title: LocalizedStringKey
    let close: () -> Void
    @ViewBuilder var subtitle: Subtitle

    var body: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.title2.bold())
                    .lineLimit(2)
                subtitle
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 12)
            CloseButton(action: close)
        }
        .padding(.horizontal, 20)
        .padding(.top, 20)
        .padding(.bottom, 8)
    }
}

extension SheetHeader where Subtitle == EmptyView {
    init(title: LocalizedStringKey, close: @escaping () -> Void) {
        self.init(title: title, close: close) { EmptyView() }
    }
}

struct CloseButton: View {
    var systemImage = "xmark"
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.body.weight(.semibold))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.glass)
        .buttonBorderShape(.circle)
        .accessibilityLabel(systemImage == "xmark" ? Text("Close") : Text("Back"))
    }
}
