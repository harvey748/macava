// AudioWallpaper — a live audio visualiser that sits on the desktop, behind your icons.
// Listens to whatever the Mac is playing (system output) via ScreenCaptureKit.
// Build with ./build.sh (needs Xcode Command Line Tools). Requires macOS 13+.

import Cocoa
import ScreenCaptureKit
import Accelerate
import CoreMedia
import CoreAudio
import ServiceManagement

// MARK: - Spectrum analysis

/// Collects incoming audio, runs an FFT and exposes log-spaced band levels (0...1).
final class SpectrumAnalyzer: @unchecked Sendable {
    static let bandCount = 48

    private let n = 2048
    private let hop = 512
    private let dft: vDSP_DFT_Setup
    private var window: [Float]
    private let zeros: [Float]
    private var ring: [Float]
    private var writeIndex = 0
    private var pending = 0
    private var sampleRate: Double = 48_000

    private let lock = NSLock()
    private var levels = [Float](repeating: 0, count: SpectrumAnalyzer.bandCount)
    private var lastUpdate = Date.distantPast

    init() {
        dft = vDSP_DFT_zop_CreateSetup(nil, vDSP_Length(n), .FORWARD)!
        window = [Float](repeating: 0, count: n)
        vDSP_hann_window(&window, vDSP_Length(n), Int32(vDSP_HANN_DENORM))
        zeros = [Float](repeating: 0, count: n)
        ring = [Float](repeating: 0, count: n)
    }

    /// Called on the audio queue only.
    func append(_ samples: [Float], sampleRate: Double) {
        self.sampleRate = sampleRate
        for s in samples {
            ring[writeIndex] = s
            writeIndex = (writeIndex + 1) % n
        }
        pending += samples.count
        if pending >= hop {
            pending = 0
            analyze()
        }
    }

    /// Latest band levels; all zeros if no audio has arrived recently.
    func snapshot() -> [Float] {
        lock.lock()
        defer { lock.unlock() }
        if Date().timeIntervalSince(lastUpdate) > 0.3 {
            return [Float](repeating: 0, count: SpectrumAnalyzer.bandCount)
        }
        return levels
    }

    private func analyze() {
        // Unroll the ring buffer (oldest sample first) and apply the window.
        var frame = [Float](repeating: 0, count: n)
        for i in 0..<n {
            frame[i] = ring[(writeIndex + i) % n] * window[i]
        }

        var re = [Float](repeating: 0, count: n)
        var im = [Float](repeating: 0, count: n)
        vDSP_DFT_Execute(dft, frame, zeros, &re, &im)

        let half = n / 2
        var mags = [Float](repeating: 0, count: half)
        for i in 0..<half {
            mags[i] = (re[i] * re[i] + im[i] * im[i]).squareRoot()
        }

        let bands = SpectrumAnalyzer.bandCount
        let fMin = 50.0, fMax = 16_000.0
        let binWidth = sampleRate / Double(n)
        let fullScale = Float(n) / 4          // magnitude of a full-scale sine after a Hann window
        var out = [Float](repeating: 0, count: bands)

        for b in 0..<bands {
            let f0 = fMin * pow(fMax / fMin, Double(b) / Double(bands))
            let f1 = fMin * pow(fMax / fMin, Double(b + 1) / Double(bands))
            let x0 = f0 / binWidth
            let x1 = f1 / binWidth
            var mag: Float = 0

            if x1 - x0 < 1.5 {
                // Band narrower than a bin: interpolate so neighbouring bars don't come out identical.
                let c = (x0 + x1) / 2
                let i = min(max(Int(c), 1), half - 2)
                let t = Float(c - Double(i))
                mag = mags[i] * (1 - t) + mags[i + 1] * t
            } else {
                let lo = min(max(Int(x0), 1), half - 1)
                let hi = min(max(Int(x1), lo), half - 1)
                for i in lo...hi { mag = max(mag, mags[i]) }
            }

            var db = 20 * log10(max(mag / fullScale, 1e-7))
            db += 14 * Float(b) / Float(bands - 1)        // lift the treble, which is naturally quieter
            out[b] = min(max((db + 65) / 55, 0), 1)       // -65 dB ... -10 dB  ->  0 ... 1
        }

        lock.lock()
        levels = out
        lastUpdate = Date()
        lock.unlock()
    }
}

// MARK: - System audio capture

final class AudioCapture: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    let analyzer: SpectrumAnalyzer
    private var stream: SCStream?
    private let queue = DispatchQueue(label: "audiowallpaper.capture")

    init(analyzer: SpectrumAnalyzer) {
        self.analyzer = analyzer
    }

    func start() async throws {
        let content = try await SCShareableContent.current
        guard let display = content.displays.first else {
            throw NSError(domain: "AudioWallpaper", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "No display found"])
        }

        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.capturesAudio = true
        config.excludesCurrentProcessAudio = true
        config.sampleRate = 48_000
        config.channelCount = 2
        // We only want sound; keep the video side as cheap as possible.
        config.width = 2
        config.height = 2
        config.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        config.showsCursor = false

        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        try await stream.startCapture()
        self.stream = stream
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
                of type: SCStreamOutputType) {
        guard type == .audio, sampleBuffer.isValid else { return }

        var rate = 48_000.0
        if let desc = CMSampleBufferGetFormatDescription(sampleBuffer),
           let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(desc) {
            rate = asbd.pointee.mSampleRate
        }

        var mono: [Float] = []
        try? sampleBuffer.withAudioBufferList { buffers, _ in
            guard let first = buffers.first, first.mData != nil else { return }
            let floatSize = MemoryLayout<Float>.size

            if buffers.count > 1 {
                // One buffer per channel (planar).
                let frames = Int(first.mDataByteSize) / floatSize
                mono = [Float](repeating: 0, count: frames)
                for buffer in buffers {
                    guard let data = buffer.mData else { continue }
                    let p = data.assumingMemoryBound(to: Float.self)
                    let count = min(frames, Int(buffer.mDataByteSize) / floatSize)
                    for i in 0..<count { mono[i] += p[i] }
                }
                let scale = 1 / Float(buffers.count)
                for i in 0..<frames { mono[i] *= scale }
            } else {
                // Single buffer, possibly interleaved.
                let channels = max(Int(first.mNumberChannels), 1)
                let frames = Int(first.mDataByteSize) / (floatSize * channels)
                let p = first.mData!.assumingMemoryBound(to: Float.self)
                mono = [Float](repeating: 0, count: frames)
                for i in 0..<frames {
                    var sum: Float = 0
                    for c in 0..<channels { sum += p[i * channels + c] }
                    mono[i] = sum / Float(channels)
                }
            }
        }

        if !mono.isEmpty {
            analyzer.append(mono, sampleRate: rate)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        // Capture can stop on sleep, display changes, etc. Keep trying to bring it back.
        self.stream = nil
        Task { [weak self] in
            while let self = self {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                do {
                    try await self.start()
                    return
                } catch {
                    continue
                }
            }
        }
    }
}

// MARK: - Album colours (system Now Playing)

/// Asks macOS what is playing and turns the album cover into a small colour palette.
/// macOS only answers Now Playing queries from Apple's own tools, so the question is asked through osascript.
/// A small grid of the album cover's colours, top row first, used to draw the cover out of dots.
struct CoverPixels {
    let side: Int
    let colors: [(r: CGFloat, g: CGFloat, b: CGFloat)]
}

final class AlbumColorWatcher: @unchecked Sendable {
    private let queue = DispatchQueue(label: "audiowallpaper.nowplaying")
    private let lock = NSLock()
    private var lastKey = ""                 // track whose cover has already been used
    private var busy = false
    private var hasUpdate = false
    private var pending: [CGColor]?
    private var pendingCover: CGImage?

    private let script = """
    function run(argv) {
      try {
        ObjC.import('Foundation');
        const bundle = $.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/');
        bundle.load;
        const Request = $.NSClassFromString('MRNowPlayingRequest');
        const item = Request.localNowPlayingItem;
        if (!item || item.isNil()) return '';
        const info = item.nowPlayingInfo;
        if (!info || info.isNil()) return '';
        const text = function (k) {
          const v = info.valueForKey(k);
          return (v && !v.isNil()) ? ObjC.unwrap(v.description) : '';
        };
        const key = (text('kMRMediaRemoteNowPlayingInfoTitle') + ' / ' +
                     text('kMRMediaRemoteNowPlayingInfoArtist') + ' / ' +
                     text('kMRMediaRemoteNowPlayingInfoAlbum')).replace(/\\n/g, ' ');
        if (key === argv[0]) return key;
        const art = info.valueForKey('kMRMediaRemoteNowPlayingInfoArtworkData');
        if (!art || art.isNil()) return key;
        return key + '\\n' + ObjC.unwrap(art.base64EncodedStringWithOptions(0));
      } catch (e) {
        return '';
      }
    }
    """

    private let spotifyScript = """
    tell application "Spotify"
      if player state is playing then
        return (id of current track) & linefeed & (artwork url of current track)
      end if
    end tell
    return ""
    """

    private let musicScript = """
    on run argv
      set lastKey to item 1 of argv
      set artPath to item 2 of argv
      set k to ""
      set d to missing value
      tell application "Music"
        if player state is not playing then return ""
        set t to current track
        set k to (name of t) & " / " & (artist of t) & " / " & (album of t)
        if k is lastKey then return k
        try
          set d to raw data of artwork 1 of t
        end try
      end tell
      if d is missing value then return k
      try
        set f to open for access (POSIX file artPath) with write permission
        set eof f to 0
        write d to f
        close access f
        return k & linefeed & "ART"
      end try
      return k
    end run
    """

    private func runScript(_ source: String, language: String? = nil, arguments: [String] = []) -> [String] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/osascript")
        var args: [String] = []
        if let language = language { args += ["-l", language] }
        args += ["-e", source]
        args += arguments
        process.arguments = args
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { return [] }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let output = String(data: data, encoding: .utf8) ?? ""
        return output.split(separator: "\n", omittingEmptySubsequences: true).map { String($0) }
    }

    private func isRunning(_ bundleID: String) -> Bool {
        !NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).isEmpty
    }

    /// Kick off a check in the background. Safe to call often.
    func poll() {
        lock.lock()
        if busy { lock.unlock(); return }
        busy = true
        lock.unlock()
        queue.async {
            self.fetch()
            self.lock.lock()
            self.busy = false
            self.lock.unlock()
        }
    }

    /// Returns new colours once after each change. `colors == nil` means "back to the default look".
    func takeUpdate() -> (changed: Bool, colors: [CGColor]?, cover: CGImage?) {
        lock.lock()
        defer { lock.unlock() }
        if !hasUpdate { return (false, nil, nil) }
        hasUpdate = false
        return (true, pending, pendingCover)
    }

    private func publish(_ colors: [CGColor]?, art: Data? = nil) {
        let cover = art.flatMap { NSImage(data: $0)?.cgImage(forProposedRect: nil, context: nil, hints: nil) }
        lock.lock()
        pending = colors
        pendingCover = cover
        hasUpdate = true
        lock.unlock()
    }

    /// Shrinks the cover to a grid of colours, one per dot.
    static func coverPixels(from cg: CGImage, side: Int) -> CoverPixels? {
        guard side > 0,
              let ctx = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8,
                                  bytesPerRow: side * 4, space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.interpolationQuality = .high
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: side, height: side))
        guard let raw = ctx.data else { return nil }
        let px = raw.bindMemory(to: UInt8.self, capacity: side * side * 4)
        var colors: [(r: CGFloat, g: CGFloat, b: CGFloat)] = []
        colors.reserveCapacity(side * side)
        for i in 0..<(side * side) {
            colors.append((r: CGFloat(px[i * 4]) / 255,
                           g: CGFloat(px[i * 4 + 1]) / 255,
                           b: CGFloat(px[i * 4 + 2]) / 255))
        }
        return CoverPixels(side: side, colors: colors)
    }

    private func fetch() {
        // 1. Spotify: it hands out a web address for the cover.
        if isRunning("com.spotify.client") {
            let lines = runScript(spotifyScript)
            if lines.count >= 2 {
                let key = lines[0]
                if key == lastKey { return }
                if let url = URL(string: lines[1]),
                   let art = try? Data(contentsOf: url),
                   let colors = AlbumColorWatcher.palette(from: art) {
                    lastKey = key
                    publish(colors, art: art)
                }
                return
            }
        }

        // 2. Apple Music: it writes the cover to a temporary file for us.
        if isRunning("com.apple.Music") {
            let artPath = NSTemporaryDirectory() + "audiowallpaper-art"
            let lines = runScript(musicScript, arguments: [lastKey, artPath])
            if let key = lines.first, !key.isEmpty {
                if key == lastKey { return }
                if lines.count >= 2,
                   let art = try? Data(contentsOf: URL(fileURLWithPath: artPath)),
                   let colors = AlbumColorWatcher.palette(from: art) {
                    publish(colors, art: art)
                } else {
                    publish(nil)
                }
                lastKey = key
                return
            }
        }

        // 3. Anything else: the system's Now Playing info, when macOS lets us see it.
        let lines = runScript(script, language: "JavaScript", arguments: [lastKey])
        guard let key = lines.first, !key.isEmpty else {
            if !lastKey.isEmpty {
                lastKey = ""
                publish(nil)
            }
            return
        }
        if lines.count >= 2,
           let art = Data(base64Encoded: lines[lines.count - 1], options: .ignoreUnknownCharacters),
           let colors = AlbumColorWatcher.palette(from: art) {
            lastKey = key
            publish(colors, art: art)
        }
    }

    private static func hsb(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat) -> (h: CGFloat, s: CGFloat, v: CGFloat) {
        let mx = max(r, g, b), mn = min(r, g, b)
        let d = mx - mn
        var h: CGFloat = 0
        if d > 0 {
            if mx == r {
                h = ((g - b) / d).truncatingRemainder(dividingBy: 6)
            } else if mx == g {
                h = (b - r) / d + 2
            } else {
                h = (r - g) / d + 4
            }
            h /= 6
            if h < 0 { h += 1 }
        }
        return (h, mx == 0 ? 0 : d / mx, mx)
    }

    /// Picks the most prominent vivid colours from the cover, brightened enough to show on a desktop.
    static func palette(from data: Data) -> [CGColor]? {
        guard let image = NSImage(data: data),
              let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }

        let side = 32
        guard let ctx = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8,
                                  bytesPerRow: side * 4, space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        ctx.interpolationQuality = .medium
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: side, height: side))
        guard let raw = ctx.data else { return nil }
        let px = raw.bindMemory(to: UInt8.self, capacity: side * side * 4)

        let bucketCount = 12
        var weight = [CGFloat](repeating: 0, count: bucketCount)
        var sumR = [CGFloat](repeating: 0, count: bucketCount)
        var sumG = [CGFloat](repeating: 0, count: bucketCount)
        var sumB = [CGFloat](repeating: 0, count: bucketCount)
        var total: CGFloat = 0

        for i in 0..<(side * side) {
            let r = CGFloat(px[i * 4]) / 255
            let g = CGFloat(px[i * 4 + 1]) / 255
            let b = CGFloat(px[i * 4 + 2]) / 255
            let c = hsb(r, g, b)
            if c.s < 0.18 || c.v < 0.18 { continue }
            let w = c.s * c.s * c.v
            let bucket = min(Int(c.h * CGFloat(bucketCount)), bucketCount - 1)
            weight[bucket] += w
            sumR[bucket] += r * w
            sumG[bucket] += g * w
            sumB[bucket] += b * w
            total += w
        }

        // Black-and-white or washed-out cover: use a clean white wave.
        if total < 12 {
            let white = NSColor(calibratedWhite: 0.97, alpha: 1).cgColor
            let grey = NSColor(calibratedWhite: 0.72, alpha: 1).cgColor
            return [grey, white, grey]
        }

        let order = (0..<bucketCount).sorted { weight[$0] > weight[$1] }
        let top = weight[order[0]]
        var picked: [CGColor] = []
        for bucket in order.prefix(3) where weight[bucket] >= top * 0.15 {
            let w = weight[bucket]
            let c = hsb(sumR[bucket] / w, sumG[bucket] / w, sumB[bucket] / w)
            picked.append(NSColor(calibratedHue: c.h, saturation: max(c.s, 0.55),
                                  brightness: max(c.v, 0.90), alpha: 1).cgColor)
        }

        // Arrange symmetrically, main colour in the middle, to match the mirrored wave.
        switch picked.count {
        case 3: return [picked[2], picked[1], picked[0], picked[1], picked[2]]
        case 2: return [picked[1], picked[0], picked[1]]
        default:
            let main = picked[0]
            let soft = main.copy(alpha: 0.55) ?? main
            return [soft, main, soft]
        }
    }
}

// MARK: - System volume

/// Current output volume of the Mac, 0...1 (0 when muted). Returns 1 for outputs with no volume control.
func systemOutputVolume() -> Float {
    var device = AudioDeviceID(0)
    var size = UInt32(MemoryLayout<AudioDeviceID>.size)
    var deviceAddress = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyDefaultOutputDevice,
                                                   mScope: kAudioObjectPropertyScopeGlobal, mElement: 0)
    guard AudioObjectGetPropertyData(AudioObjectID(kAudioObjectSystemObject), &deviceAddress,
                                     0, nil, &size, &device) == noErr else { return 1 }

    var muted: UInt32 = 0
    size = UInt32(MemoryLayout<UInt32>.size)
    var muteAddress = AudioObjectPropertyAddress(mSelector: kAudioDevicePropertyMute,
                                                 mScope: kAudioObjectPropertyScopeOutput, mElement: 0)
    if AudioObjectHasProperty(device, &muteAddress),
       AudioObjectGetPropertyData(device, &muteAddress, 0, nil, &size, &muted) == noErr,
       muted != 0 {
        return 0
    }

    var volume: Float32 = 1
    size = UInt32(MemoryLayout<Float32>.size)
    // 'vmvc' = the device's overall ("virtual main") volume, the one the volume keys change.
    var volumeAddress = AudioObjectPropertyAddress(mSelector: AudioObjectPropertySelector(0x766D_7663),
                                                   mScope: kAudioObjectPropertyScopeOutput, mElement: 0)
    guard AudioObjectHasProperty(device, &volumeAddress),
          AudioObjectGetPropertyData(device, &volumeAddress, 0, nil, &size, &volume) == noErr else { return 1 }
    return min(max(volume, 0), 1)
}

// MARK: - Dock geometry

/// Rough width of the Dock, worked out from its settings (macOS doesn't tell apps the real size).
func estimatedDockWidth() -> CGFloat {
    let dock = UserDefaults(suiteName: "com.apple.dock")
    let tile = CGFloat((dock?.object(forKey: "tilesize") as? Double) ?? 48)
    let pinnedApps = dock?.array(forKey: "persistent-apps") ?? []
    let pinnedOthers = dock?.array(forKey: "persistent-others")?.count ?? 1

    var pinnedIDs = Set<String>()
    for entry in pinnedApps {
        if let dict = entry as? [String: Any],
           let tileData = dict["tile-data"] as? [String: Any],
           let id = tileData["bundle-identifier"] as? String {
            pinnedIDs.insert(id)
        }
    }
    let runningUnpinned = NSWorkspace.shared.runningApplications.filter {
        $0.activationPolicy == .regular
            && $0.bundleIdentifier != "com.apple.finder"
            && !pinnedIDs.contains($0.bundleIdentifier ?? "")
    }.count
    let showRecents = (dock?.object(forKey: "show-recents") as? Bool) ?? true
    let recents = showRecents ? min(dock?.array(forKey: "recent-apps")?.count ?? 0, 3) : 0

    let count = pinnedApps.count + pinnedOthers + max(runningUnpinned, recents) + 2   // + Finder and Bin
    return CGFloat(count) * tile * 1.1 + 40
}

// MARK: - Drawing

enum WaveStyle: Int {
    case wave = 0, bars = 1, orb = 2, cover = 3
}

enum OrbShape: Int, CaseIterable {
    case sphere = 0, cube, donut, diamond, helix

    var title: String {
        switch self {
        case .sphere: return "Sphere"
        case .cube: return "Cube"
        case .donut: return "Donut"
        case .diamond: return "Diamond"
        case .helix: return "Double Helix"
        }
    }
}

/// One dot of a 3D shape. `v` is its height within the shape, -1 (bottom) to 1 (top),
/// used to decide which slice of the sound moves it.
struct ShapePoint {
    var x: CGFloat
    var y: CGFloat
    var z: CGFloat
    var v: CGFloat = 0
}

/// Builds the cloud of dots for each shape, sized to fit roughly inside a ball of radius 1.
func makeShapePoints(_ shape: OrbShape) -> [ShapePoint] {
    var points: [ShapePoint] = []
    let golden = CGFloat.pi * (3 - CGFloat(5).squareRoot())

    // Dots spread evenly over a ball; several shapes start from this.
    func spherePoints(_ count: Int) -> [ShapePoint] {
        var result: [ShapePoint] = []
        for k in 0..<count {
            let y = 1 - 2 * (CGFloat(k) + 0.5) / CGFloat(count)
            let r = (1 - y * y).squareRoot()
            let phi = CGFloat(k) * golden
            result.append(ShapePoint(x: cos(phi) * r, y: y, z: sin(phi) * r))
        }
        return result
    }

    switch shape {
    case .sphere:
        points = spherePoints(720)

    case .cube:
        // A grid of dots on each of the six faces.
        let steps = 11
        let half: CGFloat = 0.68
        for i in 0..<steps {
            for j in 0..<steps {
                let a = (CGFloat(i) / CGFloat(steps - 1) * 2 - 1) * half
                let b = (CGFloat(j) / CGFloat(steps - 1) * 2 - 1) * half
                points.append(ShapePoint(x: a, y: b, z: half))
                points.append(ShapePoint(x: a, y: b, z: -half))
                points.append(ShapePoint(x: half, y: a, z: b))
                points.append(ShapePoint(x: -half, y: a, z: b))
                points.append(ShapePoint(x: a, y: half, z: b))
                points.append(ShapePoint(x: a, y: -half, z: b))
            }
        }

    case .donut:
        // An upright ring facing the viewer.
        let around = 44, tube = 16
        let major: CGFloat = 0.72, minor: CGFloat = 0.28
        for i in 0..<around {
            let u = CGFloat(i) / CGFloat(around) * 2 * .pi
            for j in 0..<tube {
                let t = CGFloat(j) / CGFloat(tube) * 2 * .pi
                let ring = major + minor * cos(t)
                points.append(ShapePoint(x: ring * cos(u), y: ring * sin(u), z: minor * sin(t)))
            }
        }

    case .diamond:
        // Ball dots pushed onto the faces of an eight-sided gem.
        for p in spherePoints(720) {
            let sum = abs(p.x) + abs(p.y) + abs(p.z)
            let scale = 1.05 / max(sum, 0.0001)
            points.append(ShapePoint(x: p.x * scale, y: p.y * scale, z: p.z * scale))
        }

    case .helix:
        // Two twisting strands joined by rungs, like DNA.
        let steps = 240
        let turns: CGFloat = 2.5
        let radius: CGFloat = 0.42
        for k in 0..<steps {
            let t = CGFloat(k) / CGFloat(steps - 1)
            let y = (1 - 2 * t) * 0.98
            let angle = t * turns * 2 * .pi
            let ax = cos(angle) * radius, az = sin(angle) * radius
            points.append(ShapePoint(x: ax, y: y, z: az))
            points.append(ShapePoint(x: -ax, y: y, z: -az))
            if k % 10 == 0 {
                for r in 1..<8 {
                    let f = CGFloat(r) / 8 * 2 - 1
                    points.append(ShapePoint(x: ax * f, y: y, z: az * f))
                }
            }
        }
    }

    // Record each dot's relative height.
    var top: CGFloat = 0.0001
    for p in points { top = max(top, abs(p.y)) }
    for i in points.indices { points[i].v = points[i].y / top }
    return points
}

final class VisualizerView: NSView {
    private let backdrop = CALayer()
    private let gradient = CAGradientLayer()
    private let waveMask = CALayer()
    private let fillLayer = CAShapeLayer()
    private let lineLayer = CAShapeLayer()

    static let defaultColors: [CGColor] = [
        NSColor(calibratedRed: 1.00, green: 0.55, blue: 0.35, alpha: 1).cgColor,
        NSColor(calibratedRed: 0.95, green: 0.35, blue: 0.85, alpha: 1).cgColor,
        NSColor(calibratedRed: 0.30, green: 0.75, blue: 1.00, alpha: 1).cgColor,
        NSColor(calibratedRed: 0.95, green: 0.35, blue: 0.85, alpha: 1).cgColor,
        NSColor(calibratedRed: 1.00, green: 0.55, blue: 0.35, alpha: 1).cgColor
    ]

    /// Shown while nothing is playing: a plain, colourless hairline.
    static let restingColors: [CGColor] = [
        NSColor(calibratedWhite: 1, alpha: 0.55).cgColor,
        NSColor(calibratedWhite: 1, alpha: 0.55).cgColor
    ]

    var dockWidth: CGFloat = 700
    var widthScale: CGFloat = 1          // fine-tuning from the menu bar slider
    var heightScale: CGFloat = 1         // follows the Mac's volume
    var mirrored = true                  // true: bass in the middle; false: bass left, treble right
    var style: WaveStyle = .wave {
        didSet { applyAppearance(duration: 0.3) }
    }
    private var bars: Bool { style == .bars }

    // Album cover drawn out of dots in the middle of the screen.
    private let coverLayer = CALayer()
    var coverImage: CGImage? {
        didSet { coverGrid = nil }
    }
    var coverResolution = 40 {          // dots across the cover
        didSet { if coverResolution != oldValue { coverGrid = nil } }
    }
    private var coverGrid: CoverPixels?
    var coverMotion = true                       // true: dots dance around; false: dots only grow
    private var coverClock: CGFloat = 0          // drives the dots' movement; runs faster on busy music
    private var lastCoverTime: CFTimeInterval = 0
    private var coverMix: CGFloat = 0    // 0 = grey (silent), 1 = full colour (playing)

    /// True while the cover is still fading between grey and colour, so it needs redrawing.
    var isAnimating: Bool {
        style == .cover && abs((active ? 1 : 0) - coverMix) > 0.01
    }

    // Orb: dots spread evenly over a sphere, drawn in three depth layers (far, middle, near).
    private let orbLayers = [CAShapeLayer(), CAShapeLayer(), CAShapeLayer()]
    private var orbAngle: CGFloat = 0
    private var lastOrbTime: CFTimeInterval = 0
    var shape: OrbShape = .sphere
    var shapeScale: CGFloat = 1          // size slider for the 3D shapes
    var orbitCount = 0                   // how many small copies circle the main shape
    private var orbitAngle: CGFloat = 0
    private static let shapePoints: [[ShapePoint]] = OrbShape.allCases.map { makeShapePoints($0) }
    var monstercatSmoothing = false      // loud bands lift their neighbours into rounded hills
    private var palette: [CGColor]?
    private var active = false

    var dimmed = false {
        didSet {
            backdrop.backgroundColor = NSColor.black.withAlphaComponent(dimmed ? 0.45 : 0).cgColor
        }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true

        gradient.colors = VisualizerView.restingColors
        gradient.startPoint = CGPoint(x: 0, y: 0.5)
        gradient.endPoint = CGPoint(x: 1, y: 0.5)

        // The mask's opacity decides how strongly the gradient shows:
        // a soft translucent body with a bright crest line on top.
        fillLayer.fillColor = NSColor.white.withAlphaComponent(0.40).cgColor
        lineLayer.fillColor = nil
        lineLayer.strokeColor = NSColor.white.cgColor
        lineLayer.lineWidth = 1.5
        fillLayer.opacity = 0
        lineLayer.lineJoin = .round
        lineLayer.lineCap = .round
        waveMask.addSublayer(fillLayer)
        waveMask.addSublayer(lineLayer)
        let orbAlphas: [CGFloat] = [0.28, 0.60, 1.0]
        for (index, orbLayer) in orbLayers.enumerated() {
            orbLayer.fillColor = NSColor.white.withAlphaComponent(orbAlphas[index]).cgColor
            orbLayer.isHidden = true
            waveMask.addSublayer(orbLayer)
        }
        gradient.mask = waveMask

        coverLayer.isHidden = true
        coverLayer.contentsGravity = .resize

        layer?.addSublayer(backdrop)
        layer?.addSublayer(gradient)
        layer?.addSublayer(coverLayer)
    }

    required init?(coder: NSCoder) {
        fatalError("init(coder:) is not supported")
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        backdrop.frame = bounds
        gradient.frame = bounds
        waveMask.frame = bounds
        fillLayer.frame = bounds
        lineLayer.frame = bounds
        orbLayers.forEach { $0.frame = bounds }
        CATransaction.commit()
    }

    /// Remember a new palette (nil = default colours) and fade to it if sound is playing.
    func setColors(_ colors: [CGColor]?) {
        palette = colors
        guard active else { return }
        CATransaction.begin()
        CATransaction.setAnimationDuration(1.2)
        gradient.colors = colors ?? VisualizerView.defaultColors
        CATransaction.commit()
    }

    /// Playing: coloured. Silent: colourless (a thin line for the wave, a row of low dashes for bars).
    func setActive(_ nowActive: Bool) {
        guard nowActive != active else { return }
        active = nowActive
        applyAppearance(duration: nowActive ? 0.35 : 0.9)
    }

    private func applyAppearance(duration: CFTimeInterval) {
        CATransaction.begin()
        CATransaction.setAnimationDuration(duration)
        gradient.colors = active ? (palette ?? VisualizerView.defaultColors) : VisualizerView.restingColors
        let orb = style == .orb
        let showCover = style == .cover
        gradient.isHidden = showCover
        coverLayer.isHidden = !showCover
        if !showCover { coverLayer.contents = nil }
        fillLayer.isHidden = orb
        lineLayer.isHidden = orb
        for orbLayer in orbLayers {
            orbLayer.isHidden = !orb
            if !orb { orbLayer.path = nil }
        }
        if bars {
            fillLayer.fillColor = NSColor.white.cgColor
            fillLayer.opacity = 1
            lineLayer.opacity = 0
        } else {
            fillLayer.fillColor = NSColor.white.withAlphaComponent(0.40).cgColor
            fillLayer.opacity = active ? 1 : 0
            lineLayer.opacity = 1
            lineLayer.lineWidth = active ? 3 : 1.5
        }
        CATransaction.commit()
    }

    func update(levels: [Float]) {
        let w = bounds.width
        let h = bounds.height
        let n = levels.count
        guard w > 0, h > 0, n > 2 else { return }

        // Where is the Dock on this screen?
        var baseY: CGFloat = 0
        var waveWidth = min(dockWidth * widthScale, w)
        if let screen = window?.screen {
            let f = screen.frame, v = screen.visibleFrame
            let dockOnSide = v.minX > f.minX + 1 || v.maxX < f.maxX - 1
            if dockOnSide {
                waveWidth = w                       // no Dock along the bottom: use the whole edge
            } else {
                baseY = max(v.minY - f.minY - 1, 0) // top edge of the Dock (0 if it is hidden)
            }
        }
        let left = (w - waveWidth) / 2
        let maxHeight = h * 0.40 * heightScale

        // Soften neighbouring bands so the outline reads as one wave.
        var soft = [CGFloat](repeating: 0, count: n)
        for i in 0..<n {
            let a = CGFloat(levels[max(i - 1, 0)])
            let b = CGFloat(levels[i])
            let c = CGFloat(levels[min(i + 1, n - 1)])
            let mixed = 0.15 * a + 0.7 * b + 0.15 * c
            // Exaggerate the difference between loud and quiet so peaks punch upwards.
            soft[i] = min(pow(mixed, 1.7) * 1.2, 1.1)
        }

        // The very lowest bands are often empty; stop them carving a notch in the middle.
        soft[1] = max(soft[1], soft[2])
        soft[0] = max(soft[0], soft[1])

        // Monstercat smoothing: every loud band props up its neighbours, giving smooth "hills".
        if monstercatSmoothing {
            for i in 0..<n {
                var falloff: CGFloat = 1
                for d in 1..<n {
                    falloff *= 1.5
                    let spread = soft[i] / falloff
                    if spread < 0.01 { break }
                    if i - d >= 0 { soft[i - d] = max(soft[i - d], spread) }
                    if i + d < n { soft[i + d] = max(soft[i + d], spread) }
                }
            }
        }

        if style == .orb {
            drawOrb(soft, width: w, height: h, baseY: baseY)
            return
        }
        if style == .cover {
            drawCover(soft, width: w, height: h, baseY: baseY)
            return
        }

        // Symmetrical: bass in the middle of the Dock, treble out to both ends.
        // Left to right: bass at the left end, treble at the right.
        let total = mirrored ? 2 * n - 1 : n
        let line = CGMutablePath()
        let fill = CGMutablePath()

        if bars {
            // Flat-topped bars standing on the Dock.
            let slot = waveWidth / CGFloat(total)
            let barWidth = max(slot * 0.68, 1)
            for j in 0..<total {
                let band = mirrored ? abs(j - (n - 1)) : j
                let barHeight = max(soft[band] * maxHeight, 2)
                let x = left + CGFloat(j) * slot + (slot - barWidth) / 2
                fill.addRect(CGRect(x: x, y: baseY, width: barWidth, height: barHeight))
            }
        } else {
            // One smooth wave, easing down to nothing at the Dock's corners.
            let taperSpan: CGFloat = mirrored ? 0.12 : 0.05
            var points: [CGPoint] = []
            points.reserveCapacity(total)
            for j in 0..<total {
                let band = mirrored ? abs(j - (n - 1)) : j
                let t = CGFloat(j) / CGFloat(total - 1)
                let edge = min(t, 1 - t) / taperSpan
                let taper = edge >= 1 ? 1 : sin(edge * .pi / 2)
                points.append(CGPoint(x: left + waveWidth * t, y: baseY + soft[band] * maxHeight * taper))
            }

            // Smooth curve through the points (Catmull-Rom converted to Bezier segments).
            line.move(to: points[0])
            for i in 0..<(points.count - 1) {
                let p0 = points[max(i - 1, 0)]
                let p1 = points[i]
                let p2 = points[i + 1]
                let p3 = points[min(i + 2, points.count - 1)]
                let c1 = CGPoint(x: p1.x + (p2.x - p0.x) / 6, y: max(p1.y + (p2.y - p0.y) / 6, baseY))
                let c2 = CGPoint(x: p2.x - (p3.x - p1.x) / 6, y: max(p2.y - (p3.y - p1.y) / 6, baseY))
                line.addCurve(to: p2, control1: c1, control2: c2)
            }

            // Same curve closed along the Dock's top edge: nothing is drawn below that line.
            fill.addPath(line)
            fill.addLine(to: CGPoint(x: left + waveWidth, y: baseY))
            fill.addLine(to: CGPoint(x: left, y: baseY))
            fill.closeSubpath()
        }

        CATransaction.begin()
        CATransaction.setDisableActions(true)
        gradient.startPoint = CGPoint(x: left / w, y: 0.5)
        gradient.endPoint = CGPoint(x: (left + waveWidth) / w, y: 0.5)
        lineLayer.path = line
        fillLayer.path = fill
        CATransaction.commit()
    }

    /// The album cover rebuilt from a grid of dots. Each dot keeps the cover's colour at that spot
    /// and dances around its home position with its slice of the sound.
    /// Silent: still grey dots. Playing: fades into full colour.
    private func drawCover(_ soft: [CGFloat], width w: CGFloat, height h: CGFloat, baseY: CGFloat) {
        let n = soft.count
        coverMix += ((active ? 1 : 0) - coverMix) * 0.08

        let now = CACurrentMediaTime()
        let dt = CGFloat(min(max(now - lastCoverTime, 0), 0.05))
        lastCoverTime = now

        var energy: CGFloat = 0
        for value in soft { energy += value }
        energy /= CGFloat(n)
        let bassCount = min(6, n)
        var bass: CGFloat = 0
        for i in 0..<bassCount { bass += soft[i] }
        bass /= CGFloat(bassCount)
        coverClock += dt * (1 + energy * 3)

        if coverGrid == nil, let image = coverImage {
            coverGrid = AlbumColorWatcher.coverPixels(from: image, side: coverResolution)
        }
        let cover = coverGrid
        let grid = cover?.side ?? coverResolution

        let side = (min(w, h - baseY) * 0.42 * shapeScale).rounded()
        let cell = side / CGFloat(grid)
        let room = (side * 1.3 + cell * 6).rounded()   // space for dots that wander outside the cover
        let scale = window?.backingScaleFactor ?? 2
        let pixels = Int(room * scale)
        guard pixels > 0,
              let ctx = CGContext(data: nil, width: pixels, height: pixels, bitsPerComponent: 8,
                                  bytesPerRow: 0, space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return }
        ctx.scaleBy(x: scale, y: scale)

        let half = CGFloat(grid) / 2
        let centre = room / 2
        let push = 1 + bass * 0.10 * heightScale
        let farthest = half * CGFloat(2).squareRoot()
        let alpha = 0.45 + 0.55 * coverMix
        let clock = coverClock

        for gy in 0..<grid {
            for gx in 0..<grid {
                let dx = CGFloat(gx) + 0.5 - half
                let dy = half - (CGFloat(gy) + 0.5)
                let distance = (dx * dx + dy * dy).squareRoot()

                // Symmetrical: bass in the centre of the cover, treble at the corners.
                // Left to right: bass on the left edge, treble on the right.
                let t = mirrored ? distance / farthest : (CGFloat(gx) + 0.5) / CGFloat(grid)
                let band = min(Int(t * CGFloat(n - 1)), n - 1)
                let level = soft[band]

                var r: CGFloat = 0.6, g: CGFloat = 0.6, b: CGFloat = 0.6
                if let cover = cover {
                    let c = cover.colors[gy * grid + gx]
                    r = c.r; g = c.g; b = c.b
                }
                let grey = 0.3 * r + 0.59 * g + 0.11 * b
                r = grey + (r - grey) * coverMix
                g = grey + (g - grey) * coverMix
                b = grey + (b - grey) * coverMix

                // Every dot gets its own rhythm so they don't all move in step.
                let seed = sin(CGFloat(gx) * 12.9898 + CGFloat(gy) * 78.233) * 43758.5453
                let phase = (seed - seed.rounded(.down)) * 2 * .pi
                let wander = coverMotion ? cell * 0.95 * level * heightScale : 0
                let ox = sin(clock * 2.3 + phase) * wander
                let oy = cos(clock * 1.9 + phase * 1.37) * wander

                // Moving: dots grow a little. Still: dots do all the reacting by growing (the original look).
                let radius = coverMotion
                    ? cell * 0.5 * min(0.5 + level * 0.3 * heightScale, 0.95)
                    : cell * 0.5 * min(0.42 + level * 0.65 * heightScale, 1.15)
                let x = centre + dx * cell * push + ox
                let y = centre + dy * cell * push + oy
                ctx.setFillColor(red: r, green: g, blue: b, alpha: alpha)
                ctx.fillEllipse(in: CGRect(x: x - radius, y: y - radius, width: radius * 2, height: radius * 2))
            }
        }

        guard let image = ctx.makeImage() else { return }
        let cy = baseY + (h - baseY) / 2
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        coverLayer.frame = CGRect(x: (w - room) / 2, y: cy - room / 2, width: room, height: room)
        coverLayer.contents = image
        CATransaction.commit()
    }

    /// A slowly spinning ball of dots in the middle of the screen. Each ring of dots swells with
    /// its slice of the sound, and the whole ball spins faster as the music gets busier.
    private func drawOrb(_ soft: [CGFloat], width w: CGFloat, height h: CGFloat, baseY: CGFloat) {
        let now = CACurrentMediaTime()
        let dt = CGFloat(min(max(now - lastOrbTime, 0), 0.05))
        lastOrbTime = now

        let n = soft.count
        var energy: CGFloat = 0
        for value in soft { energy += value }
        energy /= CGFloat(n)
        let bassCount = min(6, n)
        var bass: CGFloat = 0
        for i in 0..<bassCount { bass += soft[i] }
        bass /= CGFloat(bassCount)

        orbAngle += dt * (0.35 + energy * 1.6)
        if orbAngle > 2 * .pi { orbAngle -= 2 * .pi }
        orbitAngle -= dt * (0.45 + energy * 1.0)      // moons travel the opposite way round
        if orbitAngle < -2 * .pi { orbitAngle += 2 * .pi }

        let cx = w / 2
        let cy = baseY + (h - baseY) / 2
        let radius = min(w, h - baseY) * 0.17 * shapeScale * (1 + bass * 0.10 * heightScale)
        let cosA = cos(orbAngle), sinA = sin(orbAngle)
        let tilt: CGFloat = 0.38                     // lean towards the viewer so the top is visible
        let cosT = cos(tilt), sinT = sin(tilt)

        let paths = [CGMutablePath(), CGMutablePath(), CGMutablePath()]
        for p in VisualizerView.shapePoints[shape.rawValue] {
            // Symmetrical: bass around the middle, treble at both poles. Otherwise bass at the bottom, treble on top.
            let lat = mirrored ? abs(p.v) : (p.v + 1) / 2
            let band = min(Int(lat * CGFloat(n - 1)), n - 1)
            let level = soft[band]
            let swell = 1 + level * 0.55 * heightScale

            // Spin around the vertical axis, then tilt.
            let x1 = p.x * cosA + p.z * sinA
            let z1 = -p.x * sinA + p.z * cosA
            let y2 = p.y * cosT - z1 * sinT
            let z2 = p.y * sinT + z1 * cosT

            // Nearer dots are drawn larger, further apart and brighter.
            let perspective = 1 / (1 - z2 * 0.28)
            let px = cx + x1 * swell * radius * perspective
            let py = cy + y2 * swell * radius * perspective
            let size = (1.3 + level * 1.8) * perspective
            let depth = z2 < -0.3 ? 0 : (z2 < 0.3 ? 1 : 2)
            paths[depth].addEllipse(in: CGRect(x: px - size, y: py - size, width: size * 2, height: size * 2))
        }

        // Small copies of the shape circling the main one on differently angled orbits,
        // each pulsing with its own slice of the sound.
        if orbitCount > 0 {
            let all = VisualizerView.shapePoints[shape.rawValue]
            let spin = orbAngle * 2.3
            let cosS = cos(spin), sinS = sin(spin)
            let distance: CGFloat = 2.05
            for moon in 0..<orbitCount {
                let theta = orbitAngle + CGFloat(moon) / CGFloat(orbitCount) * 2 * .pi
                let band = min(moon * n / orbitCount + n / (2 * orbitCount), n - 1)
                let mini = 0.20 * (1 + soft[band] * 0.7 * heightScale)

                // Give every moon its own orbital plane: lean the circle over by a different
                // amount for each one, then swing it round to face a different direction.
                let lean = CGFloat.pi * CGFloat(moon) / CGFloat(orbitCount)
                let swing = CGFloat(moon) * 2.4
                let flatX = cos(theta) * distance
                let flatZ = sin(theta) * distance
                let leanedX = flatX * cos(lean)
                let oy = flatX * sin(lean)
                let ox = leanedX * cos(swing) + flatZ * sin(swing)
                let oz = -leanedX * sin(swing) + flatZ * cos(swing)
                let centreDepth = oy * sinT + oz * cosT
                let depth = centreDepth < -0.6 ? 0 : (centreDepth < 0.6 ? 1 : 2)

                var index = moon % 5
                while index < all.count {
                    let p = all[index]
                    index += 5
                    let lx = (p.x * cosS + p.z * sinS) * mini + ox
                    let lz = (-p.x * sinS + p.z * cosS) * mini + oz
                    let ly = p.y * mini + oy
                    let y2 = ly * cosT - lz * sinT
                    let z2 = ly * sinT + lz * cosT
                    let perspective = 1 / (1 - z2 * 0.14)
                    let px = cx + lx * radius * perspective
                    let py = cy + y2 * radius * perspective
                    let size = 1.1 * perspective
                    paths[depth].addEllipse(in: CGRect(x: px - size, y: py - size,
                                                       width: size * 2, height: size * 2))
                }
            }
        }

        let span = radius * (orbitCount > 0 ? 2.6 : 1.7)
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        gradient.startPoint = CGPoint(x: (cx - span) / w, y: 0.5)
        gradient.endPoint = CGPoint(x: (cx + span) / w, y: 0.5)
        for (index, orbLayer) in orbLayers.enumerated() {
            orbLayer.path = paths[index]
        }
        CATransaction.commit()
    }
}

// MARK: - App

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let analyzer = SpectrumAnalyzer()
    private lazy var capture = AudioCapture(analyzer: analyzer)
    private var windows: [NSWindow] = []
    private var views: [VisualizerView] = []
    private var statusItem: NSStatusItem?
    private var timer: Timer?
    private var display = [Float](repeating: 0, count: SpectrumAnalyzer.bandCount)
    private var idle = false
    private let albumWatcher = AlbumColorWatcher()
    private var albumColors: [CGColor]?
    private var albumCover: CGImage?
    private let resolutionKey = "coverResolution"
    private let motionKey = "coverMotion"

    private var coverMotion: Bool {
        (UserDefaults.standard.object(forKey: motionKey) as? Bool) ?? true
    }

    private var coverResolution: Int {
        let stored = UserDefaults.standard.integer(forKey: resolutionKey)
        return stored > 0 ? stored : 40
    }
    private var dockWidth = estimatedDockWidth()
    private var frame = 0
    private var active = false
    private var volumeTarget: CGFloat = 1
    private var volumeScale: CGFloat = 1

    private let dimKey = "dimWallpaper"
    private let widthKey = "waveWidthScale"
    private let layoutKey = "leftToRight"
    private var symmetricalItem: NSMenuItem?
    private var leftToRightItem: NSMenuItem?
    private let styleKey = "monstercat"          // older on/off setting for the Bars style
    private let styleNameKey = "waveStyle"
    private let smoothingKey = "monstercatSmoothing"
    private var styleItems: [NSMenuItem] = []
    private var style: WaveStyle = .wave
    private let shapeKey = "orbShape"
    private var shapeItems: [NSMenuItem] = []
    private var shape: OrbShape = .sphere
    private let shapeSizeKey = "shapeScale"
    private let orbitKey = "orbitCount"
    private var orbitItems: [NSMenuItem] = []

    private var shapeScale: CGFloat {
        let stored = UserDefaults.standard.double(forKey: shapeSizeKey)
        return stored > 0 ? CGFloat(stored) : 1
    }

    private var savedStyle: WaveStyle {
        let defaults = UserDefaults.standard
        if let raw = defaults.object(forKey: styleNameKey) as? Int, let saved = WaveStyle(rawValue: raw) {
            return saved
        }
        return defaults.bool(forKey: styleKey) ? .bars : .wave
    }

    private var widthScale: CGFloat {
        let stored = UserDefaults.standard.double(forKey: widthKey)
        return stored > 0 ? CGFloat(stored) : 1
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        style = savedStyle
        shape = OrbShape(rawValue: UserDefaults.standard.integer(forKey: shapeKey)) ?? .sphere
        buildWindows()
        buildMenu()

        NotificationCenter.default.addObserver(
            self, selector: #selector(screensChanged),
            name: NSApplication.didChangeScreenParametersNotification, object: nil)

        let t = Timer(timeInterval: 1.0 / 60.0, target: self, selector: #selector(tick),
                      userInfo: nil, repeats: true)
        RunLoop.main.add(t, forMode: .common)
        timer = t

        if !CGPreflightScreenCaptureAccess() {
            CGRequestScreenCaptureAccess()
        }
        startCapture()
    }

    // MARK: Windows

    private func buildWindows() {
        windows.forEach { $0.orderOut(nil) }
        windows.removeAll()
        views.removeAll()

        let dimmed = UserDefaults.standard.bool(forKey: dimKey)
        for screen in NSScreen.screens {
            let window = NSWindow(contentRect: screen.frame, styleMask: .borderless,
                                  backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            // Above the wallpaper, below the desktop icons.
            window.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.desktopWindow)) + 1)
            window.collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
            window.isOpaque = false
            window.backgroundColor = .clear
            window.hasShadow = false
            window.ignoresMouseEvents = true

            let view = VisualizerView(frame: NSRect(origin: .zero, size: screen.frame.size))
            view.autoresizingMask = [.width, .height]
            view.dimmed = dimmed
            view.dockWidth = dockWidth
            view.widthScale = widthScale
            view.heightScale = volumeScale
            view.mirrored = !UserDefaults.standard.bool(forKey: layoutKey)
            view.shape = shape
            view.shapeScale = shapeScale
            view.orbitCount = UserDefaults.standard.integer(forKey: orbitKey)
            view.style = style
            view.monstercatSmoothing = UserDefaults.standard.bool(forKey: smoothingKey)
            view.setColors(albumColors)
            view.coverResolution = coverResolution
            view.coverMotion = coverMotion
            view.coverImage = albumCover
            view.setActive(active)
            window.contentView = view
            window.setFrame(screen.frame, display: true)
            window.orderFrontRegardless()

            windows.append(window)
            views.append(view)
        }
        idle = false
    }

    @objc private func screensChanged() {
        buildWindows()
    }

    // MARK: Render loop

    @objc private func tick() {
        frame += 1

        // Keep the wave matched to the Dock as apps open and close.
        if frame % 120 == 0 {
            let width = estimatedDockWidth()
            if abs(width - dockWidth) > 1 {
                dockWidth = width
                views.forEach { $0.dockWidth = width }
                idle = false
            }
        }

        // Louder volume, taller wave. Checked a few times a second and eased so it glides.
        if frame % 10 == 0 {
            volumeTarget = CGFloat(pow(systemOutputVolume(), 0.8))
        }
        if abs(volumeTarget - volumeScale) > 0.001 {
            volumeScale += (volumeTarget - volumeScale) * 0.15
            views.forEach { $0.heightScale = volumeScale }
        }

        let update = albumWatcher.takeUpdate()
        if update.changed {
            albumColors = update.colors
            albumCover = update.cover
            views.forEach {
                $0.setColors(update.colors)
                $0.coverImage = update.cover
            }
            idle = false
        }

        let target = analyzer.snapshot()
        var peak: Float = 0
        for i in 0..<display.count {
            let t = i < target.count ? target[i] : 0
            if t > display[i] {
                display[i] += (t - display[i]) * 0.85     // snap up
            } else {
                display[i] += (t - display[i]) * 0.22     // drop fast
            }
            peak = max(peak, display[i])
        }

        // Colour only while there is sound.
        let nowActive = active ? peak > 0.015 : peak > 0.04
        if nowActive != active {
            active = nowActive
            views.forEach { $0.setActive(nowActive) }
        }

        // Nothing playing: draw the resting state once, then stop touching the layers.
        if peak < 0.003 {
            // The orb keeps spinning in silence; the cover redraws until its fade has finished.
            if idle && style != .orb && !views.contains(where: { $0.isAnimating }) { return }
            idle = true
        } else {
            // Check the album cover when sound starts, then every few seconds while it plays.
            if idle || frame % 300 == 0 {
                albumWatcher.poll()
            }
            idle = false
        }
        views.forEach { $0.update(levels: display) }
    }

    // MARK: Capture

    private func startCapture() {
        // macOS shows its own permission dialog; just keep trying until capture is allowed.
        let capture = self.capture
        Task.detached {
            while true {
                do {
                    try await capture.start()
                    return
                } catch {
                    try? await Task.sleep(nanoseconds: 4_000_000_000)
                }
            }
        }
    }

    // MARK: Menu bar

    private func buildMenu() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let image = NSImage(systemSymbolName: "waveform", accessibilityDescription: "AudioWallpaper") {
            item.button?.image = image
        } else {
            item.button?.title = "♪"
        }

        let menu = NSMenu()

        let dim = NSMenuItem(title: "Dim Wallpaper", action: #selector(toggleDim(_:)), keyEquivalent: "")
        dim.target = self
        dim.state = UserDefaults.standard.bool(forKey: dimKey) ? .on : .off
        menu.addItem(dim)

        menu.addItem(.separator())
        let styleLabel = NSMenuItem(title: "Style", action: nil, keyEquivalent: "")
        styleLabel.isEnabled = false
        menu.addItem(styleLabel)

        let choices: [(String, WaveStyle)] = [("Wave", .wave), ("Bars", .bars)]
        for (title, value) in choices {
            let item = NSMenuItem(title: title, action: #selector(chooseStyle(_:)), keyEquivalent: "")
            item.target = self
            item.tag = value.rawValue
            item.state = value == style ? .on : .off
            menu.addItem(item)
            styleItems.append(item)
        }

        // Third style: a spinning 3D shape, picked from a drop-down.
        let shapeParent = NSMenuItem(title: "3D Shape", action: nil, keyEquivalent: "")
        shapeParent.tag = WaveStyle.orb.rawValue
        shapeParent.state = style == .orb ? .on : .off
        let shapeMenu = NSMenu()
        for option in OrbShape.allCases {
            let item = NSMenuItem(title: option.title, action: #selector(chooseShape(_:)), keyEquivalent: "")
            item.target = self
            item.tag = option.rawValue
            item.state = (style == .orb && option == shape) ? .on : .off
            shapeMenu.addItem(item)
            shapeItems.append(item)
        }
        shapeParent.submenu = shapeMenu
        menu.addItem(shapeParent)
        styleItems.append(shapeParent)

        let coverItem = NSMenuItem(title: "Album Cover", action: #selector(chooseStyle(_:)), keyEquivalent: "")
        coverItem.target = self
        coverItem.tag = WaveStyle.cover.rawValue
        coverItem.state = style == .cover ? .on : .off
        menu.addItem(coverItem)
        styleItems.append(coverItem)

        let motionItem = NSMenuItem(title: "Moving Cover Dots", action: #selector(toggleCoverMotion(_:)),
                                    keyEquivalent: "")
        motionItem.target = self
        motionItem.state = coverMotion ? .on : .off
        motionItem.indentationLevel = 1
        menu.addItem(motionItem)

        // How many small copies orbit the 3D shape.
        let orbitParent = NSMenuItem(title: "Orbiting Shapes", action: nil, keyEquivalent: "")
        let orbitMenu = NSMenu()
        let savedOrbit = UserDefaults.standard.integer(forKey: orbitKey)
        for count in 0...8 {
            let item = NSMenuItem(title: count == 0 ? "None" : "\(count)",
                                  action: #selector(chooseOrbitCount(_:)), keyEquivalent: "")
            item.target = self
            item.tag = count
            item.state = count == savedOrbit ? .on : .off
            orbitMenu.addItem(item)
            orbitItems.append(item)
        }
        orbitParent.submenu = orbitMenu
        menu.addItem(orbitParent)

        menu.addItem(.separator())
        let smoothing = NSMenuItem(title: "Monstercat Smoothing", action: #selector(toggleSmoothing(_:)),
                                   keyEquivalent: "")
        smoothing.target = self
        smoothing.state = UserDefaults.standard.bool(forKey: smoothingKey) ? .on : .off
        menu.addItem(smoothing)

        menu.addItem(.separator())
        let layoutLabel = NSMenuItem(title: "Wave Layout", action: nil, keyEquivalent: "")
        layoutLabel.isEnabled = false
        menu.addItem(layoutLabel)

        let leftToRight = UserDefaults.standard.bool(forKey: layoutKey)
        let symmetrical = NSMenuItem(title: "Symmetrical", action: #selector(chooseSymmetrical), keyEquivalent: "")
        symmetrical.target = self
        symmetrical.state = leftToRight ? .off : .on
        menu.addItem(symmetrical)
        symmetricalItem = symmetrical

        let ltr = NSMenuItem(title: "Left to Right", action: #selector(chooseLeftToRight), keyEquivalent: "")
        ltr.target = self
        ltr.state = leftToRight ? .on : .off
        menu.addItem(ltr)
        leftToRightItem = ltr

        menu.addItem(.separator())
        let widthLabel = NSMenuItem(title: "Wave Width", action: nil, keyEquivalent: "")
        widthLabel.isEnabled = false
        menu.addItem(widthLabel)

        let slider = NSSlider(value: Double(widthScale), minValue: 0.5, maxValue: 1.3,
                              target: self, action: #selector(widthChanged(_:)))
        slider.isContinuous = true
        slider.frame = NSRect(x: 18, y: 4, width: 204, height: 22)
        let sliderBox = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: 30))
        sliderBox.addSubview(slider)
        let sliderItem = NSMenuItem()
        sliderItem.view = sliderBox
        menu.addItem(sliderItem)

        let sizeLabel = NSMenuItem(title: "Shape & Cover Size", action: nil, keyEquivalent: "")
        sizeLabel.isEnabled = false
        menu.addItem(sizeLabel)

        let sizeSlider = NSSlider(value: Double(shapeScale), minValue: 0.4, maxValue: 2.5,
                                  target: self, action: #selector(shapeSizeChanged(_:)))
        sizeSlider.isContinuous = true
        sizeSlider.frame = NSRect(x: 18, y: 4, width: 204, height: 22)
        let sizeBox = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: 30))
        sizeBox.addSubview(sizeSlider)
        let sizeItem = NSMenuItem()
        sizeItem.view = sizeBox
        menu.addItem(sizeItem)

        let resolutionLabel = NSMenuItem(title: "Cover Resolution", action: nil, keyEquivalent: "")
        resolutionLabel.isEnabled = false
        menu.addItem(resolutionLabel)

        let resolutionSlider = NSSlider(value: Double(coverResolution), minValue: 10, maxValue: 96,
                                        target: self, action: #selector(resolutionChanged(_:)))
        resolutionSlider.isContinuous = true
        resolutionSlider.frame = NSRect(x: 18, y: 4, width: 204, height: 22)
        let resolutionBox = NSView(frame: NSRect(x: 0, y: 0, width: 240, height: 30))
        resolutionBox.addSubview(resolutionSlider)
        let resolutionItem = NSMenuItem()
        resolutionItem.view = resolutionBox
        menu.addItem(resolutionItem)
        menu.addItem(.separator())

        let login = NSMenuItem(title: "Open at Login", action: #selector(toggleLogin(_:)), keyEquivalent: "")
        login.target = self
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(login)

        menu.addItem(.separator())

        let quit = NSMenuItem(title: "Quit AudioWallpaper", action: #selector(quit), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)

        item.menu = menu
        statusItem = item
    }

    @objc private func toggleDim(_ sender: NSMenuItem) {
        let on = sender.state != .on
        sender.state = on ? .on : .off
        UserDefaults.standard.set(on, forKey: dimKey)
        views.forEach { $0.dimmed = on }
    }

    @objc private func chooseStyle(_ sender: NSMenuItem) {
        guard let newStyle = WaveStyle(rawValue: sender.tag) else { return }
        style = newStyle
        UserDefaults.standard.set(newStyle.rawValue, forKey: styleNameKey)
        styleItems.forEach { $0.state = $0.tag == newStyle.rawValue ? .on : .off }
        shapeItems.forEach { $0.state = .off }
        views.forEach { $0.style = newStyle }
        idle = false
    }

    @objc private func chooseShape(_ sender: NSMenuItem) {
        guard let newShape = OrbShape(rawValue: sender.tag) else { return }
        shape = newShape
        style = .orb
        UserDefaults.standard.set(newShape.rawValue, forKey: shapeKey)
        UserDefaults.standard.set(WaveStyle.orb.rawValue, forKey: styleNameKey)
        styleItems.forEach { $0.state = $0.tag == WaveStyle.orb.rawValue ? .on : .off }
        shapeItems.forEach { $0.state = $0.tag == newShape.rawValue ? .on : .off }
        views.forEach {
            $0.shape = newShape
            $0.style = .orb
        }
        idle = false
    }

    @objc private func toggleSmoothing(_ sender: NSMenuItem) {
        let on = sender.state != .on
        sender.state = on ? .on : .off
        UserDefaults.standard.set(on, forKey: smoothingKey)
        views.forEach { $0.monstercatSmoothing = on }
        idle = false
    }

    @objc private func chooseSymmetrical() {
        setLayout(leftToRight: false)
    }

    @objc private func chooseLeftToRight() {
        setLayout(leftToRight: true)
    }

    private func setLayout(leftToRight: Bool) {
        UserDefaults.standard.set(leftToRight, forKey: layoutKey)
        symmetricalItem?.state = leftToRight ? .off : .on
        leftToRightItem?.state = leftToRight ? .on : .off
        views.forEach { $0.mirrored = !leftToRight }
        idle = false
    }

    @objc private func chooseOrbitCount(_ sender: NSMenuItem) {
        let count = sender.tag
        UserDefaults.standard.set(count, forKey: orbitKey)
        orbitItems.forEach { $0.state = $0.tag == count ? .on : .off }
        views.forEach { $0.orbitCount = count }
        idle = false
    }

    @objc private func toggleCoverMotion(_ sender: NSMenuItem) {
        let on = sender.state != .on
        sender.state = on ? .on : .off
        UserDefaults.standard.set(on, forKey: motionKey)
        views.forEach { $0.coverMotion = on }
        idle = false
    }

    @objc private func resolutionChanged(_ sender: NSSlider) {
        let value = Int(sender.doubleValue.rounded())
        guard value != coverResolution || views.first?.coverResolution != value else { return }
        UserDefaults.standard.set(value, forKey: resolutionKey)
        views.forEach { $0.coverResolution = value }
        idle = false
    }

    @objc private func shapeSizeChanged(_ sender: NSSlider) {
        let value = CGFloat(sender.doubleValue)
        UserDefaults.standard.set(Double(value), forKey: shapeSizeKey)
        views.forEach { $0.shapeScale = value }
        idle = false
    }

    @objc private func widthChanged(_ sender: NSSlider) {
        let value = CGFloat(sender.doubleValue)
        UserDefaults.standard.set(Double(value), forKey: widthKey)
        views.forEach { $0.widthScale = value }
        idle = false          // redraw even if nothing is playing
    }

    @objc private func toggleLogin(_ sender: NSMenuItem) {
        do {
            if SMAppService.mainApp.status == .enabled {
                try SMAppService.mainApp.unregister()
            } else {
                try SMAppService.mainApp.register()
            }
        } catch {
            NSSound.beep()
        }
        sender.state = SMAppService.mainApp.status == .enabled ? .on : .off
    }

    @objc private func quit() {
        NSApp.terminate(nil)
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory)
app.run()
