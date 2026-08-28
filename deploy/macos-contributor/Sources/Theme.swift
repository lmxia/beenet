import AppKit
import SwiftUI

enum BeenetPalette {
    static let honey = Color(red: 0.83, green: 0.48, blue: 0.16)
    static let honeySoft = Color(red: 0.83, green: 0.48, blue: 0.16).opacity(0.16)
}

enum BeenetIcon {
    static var app: NSImage {
        if let url = Bundle.main.url(forResource: "AppIcon", withExtension: "icns"),
           let image = NSImage(contentsOf: url) {
            return image
        }
        return NSApp.applicationIconImage
    }

    static var menuBar: NSImage {
        let image = app.copy() as? NSImage ?? app
        image.size = NSSize(width: 18, height: 18)
        image.isTemplate = false
        return image
    }
}

struct AppMark: View {
    let active: Bool
    @State private var pulse = false

    var body: some View {
        Image(nsImage: BeenetIcon.app)
            .resizable()
            .interpolation(.high)
            .aspectRatio(contentMode: .fit)
            .frame(width: 88, height: 88)
            .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
            .shadow(color: .black.opacity(active ? 0.2 : 0.1), radius: active ? 12 : 8, y: 4)
            .opacity(active && pulse ? 1 : (active ? 0.94 : 0.88))
            .onAppear {
                guard active else { return }
                withAnimation(.easeInOut(duration: 1.6).repeatForever(autoreverses: true)) {
                    pulse = true
                }
            }
            .onChange(of: active) { nowActive in
                pulse = false
                if nowActive {
                    withAnimation(.easeInOut(duration: 1.6).repeatForever(autoreverses: true)) {
                        pulse = true
                    }
                }
            }
    }
}
