// macOS pieces that are much simpler in Objective-C: system audio capture (ScreenCaptureKit),
// output volume (Core Audio), placing the window on the desktop, and showing frames (Core Animation).
// Compiled by build.rs and called from src/platform/mac.rs.

#import <AppKit/AppKit.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreMedia/CoreMedia.h>
#import <QuartzCore/QuartzCore.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>

typedef void (*AWAudioCallback)(void *context, const float *samples, int count, double rate);

// MARK: - System audio capture

@interface AWCapture : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, assign) AWAudioCallback callback;
@property(nonatomic, assign) void *context;
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) dispatch_queue_t queue;
- (void)start;
@end

@implementation AWCapture

- (void)start {
    [SCShareableContent getShareableContentWithCompletionHandler:^(SCShareableContent *content, NSError *error) {
        if (error != nil || content.displays.count == 0) {
            [self retryLater];
            return;
        }
        SCDisplay *display = content.displays.firstObject;
        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
        SCStreamConfiguration *config = [[SCStreamConfiguration alloc] init];
        config.capturesAudio = YES;
        config.excludesCurrentProcessAudio = YES;
        config.sampleRate = 48000;
        config.channelCount = 2;
        // Only the sound is used; keep the video side as cheap as possible.
        config.width = 2;
        config.height = 2;
        config.minimumFrameInterval = CMTimeMake(1, 1);
        config.showsCursor = NO;

        SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:config delegate:self];
        NSError *addError = nil;
        if (![stream addStreamOutput:self type:SCStreamOutputTypeAudio sampleHandlerQueue:self.queue error:&addError]) {
            [self retryLater];
            return;
        }
        [stream addStreamOutput:self type:SCStreamOutputTypeScreen sampleHandlerQueue:self.queue error:nil];
        [stream startCaptureWithCompletionHandler:^(NSError *startError) {
            if (startError != nil) {
                [self retryLater];
            } else {
                self.stream = stream;
            }
        }];
    }];
}

// Until the user allows recording (or after sleep / display changes), keep trying quietly.
- (void)retryLater {
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(4 * NSEC_PER_SEC)), dispatch_get_main_queue(), ^{
        [self start];
    });
}

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    self.stream = nil;
    [self retryLater];
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer ofType:(SCStreamOutputType)type {
    if (type != SCStreamOutputTypeAudio || !CMSampleBufferIsValid(sampleBuffer) || self.callback == NULL) {
        return;
    }
    CMFormatDescriptionRef description = CMSampleBufferGetFormatDescription(sampleBuffer);
    const AudioStreamBasicDescription *asbd =
        description ? CMAudioFormatDescriptionGetStreamBasicDescription(description) : NULL;
    if (asbd == NULL || !(asbd->mFormatFlags & kAudioFormatFlagIsFloat) || asbd->mBitsPerChannel != 32) {
        return;
    }
    double rate = asbd->mSampleRate;

    size_t listSize = 0;
    if (CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sampleBuffer, &listSize, NULL, 0, NULL, NULL, 0, NULL) !=
            noErr ||
        listSize == 0) {
        return;
    }
    AudioBufferList *list = (AudioBufferList *)malloc(listSize);
    CMBlockBufferRef block = NULL;
    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sampleBuffer, NULL, list, listSize, NULL, NULL, kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment, &block);
    if (status == noErr && list->mNumberBuffers > 0 && list->mBuffers[0].mData != NULL) {
        UInt32 buffers = list->mNumberBuffers;
        int frames = 0;
        float *mono = NULL;
        if (buffers > 1) {
            // One buffer per channel.
            frames = (int)(list->mBuffers[0].mDataByteSize / sizeof(float));
            mono = (float *)calloc(MAX(frames, 1), sizeof(float));
            for (UInt32 b = 0; b < buffers; b++) {
                const float *p = (const float *)list->mBuffers[b].mData;
                if (p == NULL) continue;
                int count = MIN(frames, (int)(list->mBuffers[b].mDataByteSize / sizeof(float)));
                for (int i = 0; i < count; i++) mono[i] += p[i];
            }
            for (int i = 0; i < frames; i++) mono[i] /= (float)buffers;
        } else {
            // One interleaved buffer.
            int channels = MAX(1, (int)list->mBuffers[0].mNumberChannels);
            frames = (int)(list->mBuffers[0].mDataByteSize / (sizeof(float) * channels));
            const float *p = (const float *)list->mBuffers[0].mData;
            mono = (float *)calloc(MAX(frames, 1), sizeof(float));
            for (int i = 0; i < frames; i++) {
                float sum = 0;
                for (int c = 0; c < channels; c++) sum += p[i * channels + c];
                mono[i] = sum / channels;
            }
        }
        if (frames > 0) {
            self.callback(self.context, mono, frames, rate);
        }
        free(mono);
    }
    if (block != NULL) CFRelease(block);
    free(list);
}

@end

static AWCapture *gCapture = nil;

void aw_audio_start(AWAudioCallback callback, void *context) {
    dispatch_async(dispatch_get_main_queue(), ^{
        if (!CGPreflightScreenCaptureAccess()) {
            CGRequestScreenCaptureAccess();
        }
        gCapture = [[AWCapture alloc] init];
        gCapture.callback = callback;
        gCapture.context = context;
        gCapture.queue = dispatch_queue_create("audiowallpaper.capture", DISPATCH_QUEUE_SERIAL);
        [gCapture start];
    });
}

// MARK: - Volume

float aw_system_volume(void) {
    AudioObjectPropertyAddress deviceAddress = {kAudioHardwarePropertyDefaultOutputDevice,
                                                kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyElementMain};
    AudioDeviceID device = 0;
    UInt32 size = sizeof(device);
    if (AudioObjectGetPropertyData(kAudioObjectSystemObject, &deviceAddress, 0, NULL, &size, &device) != noErr) {
        return 1;
    }

    AudioObjectPropertyAddress muteAddress = {kAudioDevicePropertyMute, kAudioObjectPropertyScopeOutput,
                                              kAudioObjectPropertyElementMain};
    UInt32 muted = 0;
    size = sizeof(muted);
    if (AudioObjectHasProperty(device, &muteAddress) &&
        AudioObjectGetPropertyData(device, &muteAddress, 0, NULL, &size, &muted) == noErr && muted != 0) {
        return 0;
    }

    // 'vmvc' = the device's overall volume, the one the volume keys change.
    AudioObjectPropertyAddress volumeAddress = {(AudioObjectPropertySelector)'vmvc', kAudioObjectPropertyScopeOutput,
                                                kAudioObjectPropertyElementMain};
    Float32 volume = 1;
    size = sizeof(volume);
    if (!AudioObjectHasProperty(device, &volumeAddress) ||
        AudioObjectGetPropertyData(device, &volumeAddress, 0, NULL, &size, &volume) != noErr) {
        return 1;
    }
    return fminf(fmaxf(volume, 0), 1);
}

// MARK: - Window

static CALayer *AWFindLayer(NSView *view, NSString *name) {
    for (CALayer *layer in view.layer.sublayers) {
        if ([layer.name isEqualToString:name]) return layer;
    }
    return nil;
}

void aw_configure_window(void *viewPointer) {
    NSView *view = (__bridge NSView *)viewPointer;
    NSWindow *window = view.window;
    if (window == nil) return;
    // Above the wallpaper, below the desktop icons, on every Space, never takes clicks.
    window.level = CGWindowLevelForKey(kCGDesktopWindowLevelKey) + 1;
    window.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces | NSWindowCollectionBehaviorStationary |
                                NSWindowCollectionBehaviorIgnoresCycle;
    window.opaque = NO;
    window.backgroundColor = NSColor.clearColor;
    window.hasShadow = NO;
    window.ignoresMouseEvents = YES;

    view.wantsLayer = YES;
    CALayer *root = view.layer;
    if (AWFindLayer(view, @"aw-dim") == nil) {
        CALayer *dim = [CALayer layer];
        dim.name = @"aw-dim";
        dim.frame = root.bounds;
        dim.autoresizingMask = kCALayerWidthSizable | kCALayerHeightSizable;
        [root addSublayer:dim];

        CALayer *stage = [CALayer layer];
        stage.name = @"aw-stage";
        stage.contentsGravity = kCAGravityResize;
        [root addSublayer:stage];
    }
    [window orderFrontRegardless];
}

void aw_set_dim(void *viewPointer, double alpha) {
    NSView *view = (__bridge NSView *)viewPointer;
    CALayer *dim = AWFindLayer(view, @"aw-dim");
    if (dim == nil) return;
    CGColorRef color = CGColorCreateGenericGray(0, alpha);
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    dim.frame = view.layer.bounds;
    dim.backgroundColor = color;
    [CATransaction commit];
    CGColorRelease(color);
}

void aw_present_stage(void *viewPointer, const uint8_t *rgba, int width, int height, double x, double y, double w,
                      double h) {
    NSView *view = (__bridge NSView *)viewPointer;
    CALayer *stage = AWFindLayer(view, @"aw-stage");
    if (stage == nil || rgba == NULL || width <= 0 || height <= 0) return;

    CFDataRef data = CFDataCreate(NULL, rgba, (CFIndex)width * height * 4);
    CGDataProviderRef provider = CGDataProviderCreateWithCFData(data);
    CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
    CGImageRef image = CGImageCreate(width, height, 8, 32, (size_t)width * 4, space,
                                     (CGBitmapInfo)kCGImageAlphaPremultipliedLast | kCGBitmapByteOrderDefault, provider,
                                     NULL, false, kCGRenderingIntentDefault);

    // Rust measures from the top-left; flip if this view's layer measures from the bottom.
    CGFloat top = view.isFlipped ? y : view.bounds.size.height - y - h;
    [CATransaction begin];
    [CATransaction setDisableActions:YES];
    stage.frame = CGRectMake(x, top, w, h);
    stage.contents = (__bridge id)image;
    [CATransaction commit];

    CGImageRelease(image);
    CGColorSpaceRelease(space);
    CGDataProviderRelease(provider);
    CFRelease(data);
}

// MARK: - Dock

typedef struct {
    double bottom_inset;
    double dock_width;
    int dock_on_side;
} AWScreenMetrics;

// macOS doesn't tell apps the Dock's size, so estimate it from the Dock's settings.
static double AWEstimatedDockWidth(void) {
    NSUserDefaults *dock = [[NSUserDefaults alloc] initWithSuiteName:@"com.apple.dock"];
    double tile = [dock objectForKey:@"tilesize"] ? [dock doubleForKey:@"tilesize"] : 48;
    NSArray *pinnedApps = [dock arrayForKey:@"persistent-apps"] ?: @[];
    NSArray *others = [dock arrayForKey:@"persistent-others"];
    NSInteger pinnedOthers = others ? (NSInteger)others.count : 1;

    NSMutableSet *pinnedIDs = [NSMutableSet set];
    for (id entry in pinnedApps) {
        if (![entry isKindOfClass:NSDictionary.class]) continue;
        NSDictionary *tileData = entry[@"tile-data"];
        if (![tileData isKindOfClass:NSDictionary.class]) continue;
        NSString *bundleID = tileData[@"bundle-identifier"];
        if ([bundleID isKindOfClass:NSString.class]) [pinnedIDs addObject:bundleID];
    }
    NSInteger runningUnpinned = 0;
    for (NSRunningApplication *app in NSWorkspace.sharedWorkspace.runningApplications) {
        NSString *bundleID = app.bundleIdentifier ?: @"";
        if (app.activationPolicy == NSApplicationActivationPolicyRegular &&
            ![bundleID isEqualToString:@"com.apple.finder"] && ![pinnedIDs containsObject:bundleID]) {
            runningUnpinned++;
        }
    }
    BOOL showRecents = [dock objectForKey:@"show-recents"] ? [dock boolForKey:@"show-recents"] : YES;
    NSInteger recents = showRecents ? MIN((NSInteger)[[dock arrayForKey:@"recent-apps"] count], 3) : 0;

    NSInteger count = (NSInteger)pinnedApps.count + pinnedOthers + MAX(runningUnpinned, recents) + 2;  // + Finder, Bin
    return count * tile * 1.1 + 40;
}

void aw_screen_metrics(void *viewPointer, AWScreenMetrics *out) {
    NSView *view = (__bridge NSView *)viewPointer;
    NSScreen *screen = view.window.screen ?: NSScreen.mainScreen;
    NSRect frame = screen.frame;
    NSRect visible = screen.visibleFrame;
    BOOL side = visible.origin.x > frame.origin.x + 1 || NSMaxX(visible) < NSMaxX(frame) - 1;
    out->dock_on_side = side ? 1 : 0;
    out->bottom_inset = side ? 0 : MAX(visible.origin.y - frame.origin.y - 1, 0);
    out->dock_width = side ? 0 : AWEstimatedDockWidth();
}

// MARK: - Misc

int aw_app_running(const char *bundleID) {
    NSString *identifier = [NSString stringWithUTF8String:bundleID];
    return identifier != nil && [NSRunningApplication runningApplicationsWithBundleIdentifier:identifier].count > 0;
}

// The settings window runs as its own process; make sure it comes to the front.
void aw_bring_to_front(void) {
    [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];
    [NSApp activateIgnoringOtherApps:YES];
}
