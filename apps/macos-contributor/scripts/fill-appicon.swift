import AppKit
import Foundation

// Expand the painted squircle to the full 1024 square so macOS can apply
// its own mask. Near-black corner pixels take color from the nearest
// non-black pixel toward the center.

if CommandLine.arguments.count < 3 {
    fputs("usage: fill-appicon.swift <src.png> <dst.png>\n", stderr)
    exit(1)
}

let srcPath = CommandLine.arguments[1]
let dstPath = CommandLine.arguments[2]
guard let src = NSImage(contentsOfFile: srcPath),
      let tiff = src.tiffRepresentation,
      let rep = NSBitmapImageRep(data: tiff)
else {
    fputs("error: cannot read \(srcPath)\n", stderr)
    exit(1)
}

let width = rep.pixelsWide
let height = rep.pixelsHigh
guard let out = NSBitmapImageRep(
    bitmapDataPlanes: nil,
    pixelsWide: width,
    pixelsHigh: height,
    bitsPerSample: 8,
    samplesPerPixel: 4,
    hasAlpha: true,
    isPlanar: false,
    colorSpaceName: .deviceRGB,
    bytesPerRow: 0,
    bitsPerPixel: 32
), let pixels = out.bitmapData else {
    fputs("error: cannot allocate bitmap\n", stderr)
    exit(1)
}

struct RGB { var r: UInt8; var g: UInt8; var b: UInt8 }

func luma(_ c: RGB) -> Int { Int(c.r) + Int(c.g) + Int(c.b) }
func isCornerFill(_ c: RGB) -> Bool { luma(c) < 28 }

var source = [RGB](repeating: RGB(r: 0, g: 0, b: 0), count: width * height)
for y in 0..<height {
    for x in 0..<width {
        let color = rep.colorAt(x: x, y: y)!.usingColorSpace(.deviceRGB)!
        source[y * width + x] = RGB(
            r: UInt8(max(0, min(255, color.redComponent * 255))),
            g: UInt8(max(0, min(255, color.greenComponent * 255))),
            b: UInt8(max(0, min(255, color.blueComponent * 255)))
        )
    }
}

var filled = source
let cx = width / 2
let cy = height / 2
for y in 0..<height {
    for x in 0..<width {
        let idx = y * width + x
        if !isCornerFill(source[idx]) { continue }
        let dx = cx - x
        let dy = cy - y
        let steps = max(abs(dx), abs(dy), 1)
        for i in 1...steps {
            let nx = x + dx * i / steps
            let ny = y + dy * i / steps
            let sample = source[ny * width + nx]
            if !isCornerFill(sample) {
                filled[idx] = sample
                break
            }
        }
    }
}

for y in 0..<height {
    for x in 0..<width {
        let c = filled[y * width + x]
        let i = y * out.bytesPerRow + x * 4
        pixels[i] = c.r
        pixels[i + 1] = c.g
        pixels[i + 2] = c.b
        pixels[i + 3] = 255
    }
}

guard let png = out.representation(using: .png, properties: [:]) else {
    fputs("error: cannot encode png\n", stderr)
    exit(1)
}
try png.write(to: URL(fileURLWithPath: dstPath), options: .atomic)
