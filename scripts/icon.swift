import AppKit

let size: CGFloat = 1024
let img = NSImage(size: NSSize(width: size, height: size))
img.lockFocus()
let ctx = NSGraphicsContext.current!.cgContext

// Squircle background with a violet gradient.
let inset: CGFloat = 100
let bgRect = NSRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
let bg = NSBezierPath(roundedRect: bgRect, xRadius: 185, yRadius: 185)
ctx.saveGState()
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.28)
shadow.shadowBlurRadius = 28
shadow.shadowOffset = NSSize(width: 0, height: -12)
shadow.set()
NSColor(calibratedRed: 0.36, green: 0.12, blue: 0.53, alpha: 1).setFill()
bg.fill()
ctx.restoreGState()
NSGradient(colors: [
  NSColor(calibratedRed: 0.55, green: 0.27, blue: 0.95, alpha: 1),
  NSColor(calibratedRed: 0.29, green: 0.08, blue: 0.47, alpha: 1),
])!.draw(in: bg, angle: -90)

// A browser window made of tabs: two tabs behind, one in front.
func tab(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ color: NSColor) {
  let p = NSBezierPath(roundedRect: NSRect(x: x, y: y, width: w, height: h), xRadius: 26, yRadius: 26)
  color.setFill(); p.fill()
}
let white = NSColor.white
tab(652, 630, 132, 100, white.withAlphaComponent(0.35))   // back tab
tab(468, 630, 160, 100, white.withAlphaComponent(0.6))     // middle tab
tab(240, 640, 200, 130, white)                             // active tab
// Window body joined to the active tab.
let body = NSBezierPath(roundedRect: NSRect(x: 240, y: 250, width: 544, height: 400), xRadius: 40, yRadius: 40)
white.setFill(); body.fill()

// Magnifying glass in the window.
let conf = NSImage.SymbolConfiguration(pointSize: 260, weight: .bold)
if let sym = NSImage(systemSymbolName: "magnifyingglass", accessibilityDescription: nil)?.withSymbolConfiguration(conf) {
  let tinted = NSImage(size: sym.size)
  tinted.lockFocus()
  sym.draw(at: .zero, from: .zero, operation: .sourceOver, fraction: 1)
  NSColor(calibratedRed: 0.42, green: 0.17, blue: 0.70, alpha: 1).set()
  NSRect(origin: .zero, size: sym.size).fill(using: .sourceAtop)
  tinted.unlockFocus()
  let r = NSRect(x: 512 - sym.size.width / 2, y: 450 - sym.size.height / 2, width: sym.size.width, height: sym.size.height)
  tinted.draw(in: r)
}
img.unlockFocus()

let rep = NSBitmapImageRep(data: img.tiffRepresentation!)!
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
