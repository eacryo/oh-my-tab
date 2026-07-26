import AppKit
import Foundation

// svg2png <svg> <iconset_dir>: 把 SVG 渲染成 macOS 标准 .iconset 的 10 张 PNG。
// 用 NSImage(WebKit) 加载 SVG,在每个目标尺寸下分别光栅化到带 alpha 的位图上下文,再写为 PNG。
// 保留透明圆角(NSImage 不会像 qlmanage 那样垫白底),各尺寸原生光栅化以保证清晰度。
//
// svg2png <svg> <iconset_dir>: render an SVG into the 10 standard macOS .iconset PNGs.
// Loads the SVG via NSImage (WebKit), rasterizes at each target size into an alpha-capable bitmap context, then writes PNGs.
// Transparent corners are preserved (NSImage does not composite onto white the way qlmanage does); each size is rasterized natively for crispness.

let args = CommandLine.arguments
guard args.count == 3 else {
    FileHandle.standardError.write("usage: svg2png <svg> <iconset_dir>\n".data(using: .utf8)!)
    exit(2)
}
let src = URL(fileURLWithPath: args[1])
let outDir = args[2]

// macOS 标准 .iconset 文件名 -> 像素尺寸。@2x 的像素尺寸 = 名义尺寸 × 2。
// Standard macOS .iconset filenames -> pixel sizes. @2x pixel size = nominal × 2.
let entries: [(String, Int)] = [
    ("icon_16x16.png",      16),
    ("icon_16x16@2x.png",   32),
    ("icon_32x32.png",      32),
    ("icon_32x32@2x.png",   64),
    ("icon_128x128.png",    128),
    ("icon_128x128@2x.png", 256),
    ("icon_256x256.png",    256),
    ("icon_256x256@2x.png", 512),
    ("icon_512x512.png",    512),
    ("icon_512x512@2x.png", 1024),
]

let cs = CGColorSpace(name: CGColorSpace.sRGB)!
try? FileManager.default.createDirectory(atPath: outDir, withIntermediateDirectories: true)

for (name, px) in entries {
    // 每个尺寸重新加载一次,避免 NSImage 缓存首次光栅化结果导致尺寸不对。
    // Reload per size so NSImage does not cache the first rasterization at the wrong size.
    guard let img = NSImage(contentsOf: src) else {
        FileHandle.standardError.write("error: cannot load \(src.path)\n".data(using: .utf8)!)
        exit(1)
    }
    img.size = NSSize(width: px, height: px)   // SVG: 设定本次光栅化尺寸 / set rasterization size for this render
    guard let cg = img.cgImage(forProposedRect: nil, context: nil, hints: nil),
          let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
                               space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else {
        FileHandle.standardError.write("error: rasterize failed at \(px)x\(px)\n".data(using: .utf8)!)
        exit(1)
    }
    ctx.clear(CGRect(x: 0, y: 0, width: px, height: px))
    ctx.draw(cg, in: CGRect(x: 0, y: 0, width: px, height: px))
    guard let out = ctx.makeImage(),
          let dest = CGImageDestinationCreateWithURL(URL(fileURLWithPath: outDir).appendingPathComponent(name) as CFURL,
                                                      "public.png" as CFString, 1, nil) else {
        FileHandle.standardError.write("error: write failed for \(name)\n".data(using: .utf8)!)
        exit(1)
    }
    CGImageDestinationAddImage(dest, out, nil)
    CGImageDestinationFinalize(dest)
}
print("wrote \(entries.count) PNGs to \(outDir)")
