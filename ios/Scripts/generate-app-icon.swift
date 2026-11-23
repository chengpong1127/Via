import AppKit
import CoreGraphics
import Foundation

let size = 1024
let outputDirectory = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : ".")

struct Variant {
    let name: String
    let top: CGColor
    let bottom: CGColor
    let text: CGColor
}

func rgb(_ r: Double, _ g: Double, _ b: Double) -> CGColor {
    CGColor(srgbRed: r, green: g, blue: b, alpha: 1)
}

let variants = [
    Variant(name: "AppIcon-Light", top: rgb(0.20, 0.55, 0.95), bottom: rgb(0.05, 0.30, 0.75), text: rgb(1, 1, 1)),
    Variant(name: "AppIcon-Dark", top: rgb(0.10, 0.12, 0.18), bottom: rgb(0.02, 0.03, 0.06), text: rgb(0.35, 0.65, 1)),
    Variant(name: "AppIcon-Tinted", top: rgb(0.45, 0.45, 0.45), bottom: rgb(0.10, 0.10, 0.10), text: rgb(1, 1, 1)),
]

func render(_ variant: Variant) throws {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    guard let context = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0, space: space,
        bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
    ) else { throw CocoaError(.fileWriteUnknown) }

    let gradient = CGGradient(colorsSpace: space, colors: [variant.top, variant.bottom] as CFArray, locations: [0, 1])!
    context.drawLinearGradient(gradient, start: CGPoint(x: 0, y: size), end: CGPoint(x: 0, y: 0), options: [])

    let graphics = NSGraphicsContext(cgContext: context, flipped: false)
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = graphics
    let style = NSMutableParagraphStyle()
    style.alignment = .center
    let attributes: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 460, weight: .heavy),
        .foregroundColor: NSColor(cgColor: variant.text)!,
        .paragraphStyle: style,
    ]
    let text = NSAttributedString(string: "Via", attributes: attributes)
    let bounds = text.boundingRect(with: CGSize(width: size, height: size), options: [.usesLineFragmentOrigin])
    text.draw(in: CGRect(x: 0, y: (CGFloat(size) - bounds.height) / 2, width: CGFloat(size), height: bounds.height))
    NSGraphicsContext.restoreGraphicsState()

    guard let image = context.makeImage(),
          let data = NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])
    else { throw CocoaError(.fileWriteUnknown) }
    try data.write(to: outputDirectory.appendingPathComponent("\(variant.name).png"))
}

for variant in variants { try render(variant) }
