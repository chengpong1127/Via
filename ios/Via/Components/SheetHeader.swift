import SwiftUI

struct SheetHeader: View {
    let title: LocalizedStringKey
    let close: () -> Void

    var body: some View {
        HStack(alignment: .top) {
            Text(title)
                .font(.title2.bold())
                .lineLimit(2)
            Spacer(minLength: 12)
            CloseButton(action: close)
        }
        .padding(.horizontal, 20)
        .padding(.top, 20)
        .padding(.bottom, 8)
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
