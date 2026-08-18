import AppKit
import SwiftUI

enum BeenetPalette {
    static let honey = Color(red: 0.83, green: 0.48, blue: 0.16)
    static let honeySoft = Color(red: 0.83, green: 0.48, blue: 0.16).opacity(0.16)
}

struct HexShape: Shape {
    func path(in rect: CGRect) -> Path {
        let w = rect.width
        let h = rect.height
        let points = [
            CGPoint(x: w * 0.50, y: 0),
            CGPoint(x: w, y: h * 0.25),
            CGPoint(x: w, y: h * 0.75),
            CGPoint(x: w * 0.50, y: h),
            CGPoint(x: 0, y: h * 0.75),
            CGPoint(x: 0, y: h * 0.25),
        ]
        var path = Path()
        path.move(to: points[0])
        for point in points.dropFirst() {
            path.addLine(to: point)
        }
        path.closeSubpath()
        return path
    }
}

struct HexMark: View {
    let active: Bool
    @State private var pulse = false

    var body: some View {
        ZStack {
            HexShape()
                .fill(BeenetPalette.honey.opacity(active ? 0.14 : 0.04))
            HexShape()
                .stroke(
                    active ? BeenetPalette.honey : Color.primary.opacity(0.18),
                    lineWidth: 1.2
                )
        }
        .frame(width: 42, height: 46)
        .opacity(active && pulse ? 1 : 0.82)
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
