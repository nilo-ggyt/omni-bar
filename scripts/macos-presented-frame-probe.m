/*
 * Observe bounded ScreenCaptureKit DISPLAY-frame metadata for one explicitly
 * identified synthetic application. The default is metadata only; optional
 * bounded ROI/OCR observes a declared token transition but does not correlate
 * input or certify physical display scanout or latency.
 * Capture is opt-in with existing authorization or an explicit application picker;
 * help/argument rejection
 * never calls a permission API, presents a picker, or creates a stream.
 */
#import <AppKit/AppKit.h>
#import <CoreMedia/CoreMedia.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import "macos_frame_probe/semantic.h"
#include <errno.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <math.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sysexits.h>
#include <unistd.h>

/* All sizes/counts are fixed or validated before permission/native acquisition. */
static const NSInteger maximumFrames = 600;
static const double maximumDuration = 30.0;
static const size_t outputWidth = 640;
static const size_t outputHeight = 360;

/** Validated caller intent; no native surface or capture resources are owned here. */
@interface FrameOptions : NSObject
@property(nonatomic) pid_t pid;
@property(nonatomic, copy) NSString *bundleID;
@property(nonatomic) NSUInteger displayIndex;
@property(nonatomic) NSInteger frames;
@property(nonatomic) double duration;
@property(nonatomic) BOOL validationOnly;
@property(nonatomic) BOOL usesPicker;
@property(nonatomic, copy) NSString *expectedToken;
@property(nonatomic, copy) NSString *baselineToken;
@property(nonatomic) CGRect sourceROI;
@property(nonatomic) unsigned int baselineFrames;
@property(nonatomic) BOOL continuousSemantic;
@end
@implementation FrameOptions
@end

/** Print an intentionally noncapturing CLI contract without inspecting TCC or shareable content. */
static void printUsage(FILE *output) {
  fprintf(output,
          "Usage: macos-presented-frame-probe --capture|--validate\n"
          "  --existing-authorization|--picker\n"
          "  --synthetic-unlocked --pid PID --bundle-id ID --display-index INDEX\n"
          "  [--frames 1..600] [--duration 0.1..30]\n"
          "  [--expect-token UTF8_NONCE --baseline-token OLD_NONCE --source-roi X,Y,W,H\n"
          "   [--baseline-frames 1..10] [--continuous-semantic]]\n"
          "\n"
          "Requires an unlocked, isolated synthetic session.\n"
          "--existing-authorization requires macOS 14.2+ and nonprompting access preflight.\n"
          "--picker explicitly presents single-application session consent on macOS 15.2+.\n"
          "Select only the declared application on the declared display; other scopes fail.\n"
          "Captures DISPLAY content filtered to the exact PID/bundle ID only;\n"
          "640x360 BGRA, queue depth 3, desired maximum 60 fps.\n"
          "Default writes only JSONL metadata, never pixels, titles, OCR text, or latency.\n"
          "Optional OCR uses one owned <=512x128 BGRA crop and emits token digest IDs only.\n"
          "No global permission request is made. See frame-measurement.md.\n"
          "--help and --validate perform no capture or permission query.\n");
}

/** Accept decimal nonnegative integers without signs, whitespace, overflow, or suffix repair. */
static BOOL parseInteger(const char *text, uint64_t maximum, uint64_t *result) {
  if (text[0] == '\0') {
    /* Empty values cannot silently choose a different process/display or inherit defaults. */
    return NO;
  }
  for (const unsigned char *cursor = (const unsigned char *)text; *cursor; ++cursor) {
    /* Check the spelling before strtoull so signs/space do not gain platform-specific meaning. */
    if (*cursor < '0' || *cursor > '9') {
      /* Reject the complete value rather than accepting a misleading numeric prefix. */
      return NO;
    }
  }
  errno = 0;
  char *end = NULL;
  unsigned long long parsed = strtoull(text, &end, 10);
  if (errno != 0 || *end != '\0' || parsed > maximum) {
    /* Every later cast is within its target's independently selected bound. */
    return NO;
  }
  *result = parsed;
  return YES;
}

/** Parse all options before creating native objects or checking screen-recording access. */
static FrameOptions *parseOptions(int argc, const char **argv) {
  FrameOptions *options = [FrameOptions new];
  options.frames = 120;
  options.duration = 5.0;
  options.baselineFrames = 1;
  BOOL operation = NO, authorized = NO, synthetic = NO, sawPID = NO, sawBundle = NO;
  BOOL sawDisplay = NO, sawFrames = NO, sawDuration = NO;
  BOOL sawExpected = NO, sawBaseline = NO, sawROI = NO, sawBaselineFrames = NO;
  BOOL sawContinuous = NO;
  for (int index = 1; index < argc; ++index) {
    /* Each accepted spelling is unique; duplicate switches cannot obscure reviewed capture scope. */
    const char *argument = argv[index];
    if (strcmp(argument, "--capture") == 0 && !operation) {
      /* Only this explicit operation enables the acquisition phase after all validation succeeds. */
      operation = YES;
    } else if (strcmp(argument, "--validate") == 0 && !operation) {
      /* Validation exercises complete parsing without checking identity, TCC, content, or stream state. */
      options.validationOnly = YES;
      operation = YES;
    } else if (strcmp(argument, "--existing-authorization") == 0 && !authorized) {
      /* Existing access and picker session consent are mutually exclusive policies. */
      authorized = YES;
    } else if (strcmp(argument, "--picker") == 0 && !authorized) {
      /* Only this explicit spelling permits presenting the application consent picker in capture mode. */
      options.usesPicker = YES;
      authorized = YES;
    } else if (strcmp(argument, "--continuous-semantic") == 0 && !sawContinuous) {
      /* Explicit continuation records later predicates through the existing finite caps, without adding buffers. */
      options.continuousSemantic = YES;
      sawContinuous = YES;
    } else if (strcmp(argument, "--synthetic-unlocked") == 0 && !synthetic) {
      /* This is a caller attestation, not an invented public API for detecting screen lock. */
      synthetic = YES;
    } else {
      /* Value-taking options must provide a complete bounded argument; unknown switches fail closed. */
      if (index + 1 >= argc) {
        return nil;
      }
      const char *value = argv[++index];
      uint64_t integer = 0;
      if (strcmp(argument, "--pid") == 0 && !sawPID) {
        /* Zero/self/system/foreign processes are excluded before content enumeration. */
        if (!parseInteger(value, INT32_MAX, &integer) || integer <= 1 || integer == (uint64_t)getpid()) {
          return nil;
        }
        options.pid = (pid_t)integer;
        sawPID = YES;
      } else if (strcmp(argument, "--bundle-id") == 0 && !sawBundle) {
        /* Limit identifiers to bounded ASCII bundle syntax; no user-controlled body enters output. */
        size_t length = strlen(value);
        if (length == 0 || length > 255) {
          return nil;
        }
        for (size_t position = 0; position < length; ++position) {
          /* A bundle ID is an identity check, never a substring process-name selector. */
          char byte = value[position];
          if (!((byte >= 'a' && byte <= 'z') || (byte >= 'A' && byte <= 'Z') ||
                (byte >= '0' && byte <= '9') || byte == '.' || byte == '-')) {
            return nil;
          }
        }
        options.bundleID = [NSString stringWithUTF8String:value];
        sawBundle = YES;
      } else if (strcmp(argument, "--display-index") == 0 && !sawDisplay) {
        /* Resolve the chosen display only in capture mode; no validation-only display enumeration occurs. */
        if (!parseInteger(value, 15, &integer)) {
          return nil;
        }
        options.displayIndex = (NSUInteger)integer;
        sawDisplay = YES;
      } else if (strcmp(argument, "--frames") == 0 && !sawFrames) {
        /* Frame count bounds both work and JSON output independently from the deadline. */
        if (!parseInteger(value, (uint64_t)maximumFrames, &integer) || integer == 0) {
          return nil;
        }
        options.frames = (NSInteger)integer;
        sawFrames = YES;
      } else if (strcmp(argument, "--duration") == 0 && !sawDuration) {
        /* Reject NaN/infinity before multiplying into dispatch_time's signed integer interval. */
        errno = 0;
        char *end = NULL;
        double duration = strtod(value, &end);
        if (value[0] == '\0' || errno != 0 || *end != '\0' || !isfinite(duration) ||
            duration < 0.1 || duration > maximumDuration) {
          return nil;
        }
        options.duration = duration;
        sawDuration = YES;
      } else if ((strcmp(argument, "--expect-token") == 0 && !sawExpected) ||
                 (strcmp(argument, "--baseline-token") == 0 && !sawBaseline)) {
        /* These bounded synthetic UTF-8 values are never echoed or used as preferred OCR dictionary words. */
        if (strlen(value) > 64) {
          /* Bound raw bytes before creating an NSString or traversing Unicode scalars. */
          return nil;
        }
        NSString *token = [NSString stringWithUTF8String:value];
        if (!OmniFrameTokenIsValid(token)) {
          /* Invalid UTF-8/grammar cannot be repaired into a different synthetic measurement token. */
          return nil;
        }
        if (strcmp(argument, "--expect-token") == 0) {
          /* Explicit new-state intent alone never authorizes a baseline inferred from OCR silence. */
          options.expectedToken = token;
          sawExpected = YES;
        } else {
          /* A distinct positively recognized OLD token establishes the required pre-transition control. */
          options.baselineToken = token;
          sawBaseline = YES;
        }
      } else if (strcmp(argument, "--source-roi") == 0 && !sawROI) {
        /* Parse points only; live display/filter scale validation remains capture-side and cannot run here. */
        CGRect roi = CGRectZero;
        if (!OmniFrameParseROI(value, &roi)) {
          /* Reject malformed/nonfinite source coordinates before any display or image acquisition. */
          return nil;
        }
        options.sourceROI = roi;
        sawROI = YES;
      } else if (strcmp(argument, "--baseline-frames") == 0 && !sawBaselineFrames) {
        /* The required positive baseline count is bounded independently from total frame output. */
        if (!parseInteger(value, 10, &integer) || integer == 0) {
          /* Neither a vacuous baseline nor an unbounded count may authorize a later NEW predicate. */
          return nil;
        }
        options.baselineFrames = (unsigned int)integer;
        sawBaselineFrames = YES;
      } else {
        /* Unsupported/duplicate options cannot start even a permission preflight. */
        return nil;
      }
    }
  }
  if (!operation || !authorized || !synthetic || !sawPID || !sawBundle || !sawDisplay) {
    /* Explicit operation, authorization policy, fixture attestation, and complete identity are mandatory. */
    return nil;
  }
  if ((sawExpected || sawBaseline || sawROI || sawBaselineFrames || sawContinuous) &&
      (!sawExpected || !sawBaseline || !sawROI || [options.expectedToken isEqualToString:options.baselineToken])) {
    /* Partial/same-state semantic configuration cannot silently fall back to metadata-only success. */
    return nil;
  }
  return options;
}

/** Preserve JSON boolean types explicitly instead of boxing C comparison expressions as numeric zero/one. */
static NSNumber *booleanValue(BOOL value) {
  return [NSNumber numberWithBool:value];
}

/** Emit one small metadata-only record; serialization and output failure stop collection. */
static BOOL emitRecord(NSDictionary *record) {
  NSError *error = nil;
  NSData *data = [NSJSONSerialization dataWithJSONObject:record options:0 error:&error];
  if (data == nil || data.length > 2048) {
    /* Do not log arbitrary native error descriptions, paths, window titles, or malformed metadata. */
    return NO;
  }
  return fwrite(data.bytes, 1, data.length, stdout) == data.length && fputc('\n', stdout) != EOF && fflush(stdout) == 0;
}

/** Preserve every raw 64-bit clock tick even when a consumer uses IEEE-754 JSON number parsing. */
static NSString *clockTicks(uint64_t ticks) {
  /* Decimal strings avoid silently rounding long-uptime timestamps through JavaScript Number. */
  return [NSString stringWithFormat:@"%llu", (unsigned long long)ticks];
}

/** Read the SDK-declared libproc birth identity without enumerating another process's arguments/environment.
 * libproc marks its interfaces private/subject to change; this declaration is not a public ABI stability promise. */
static BOOL processIdentity(pid_t pid, struct proc_bsdinfo *identity) {
  int bytes = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, identity, sizeof(*identity));
  return bytes == sizeof(*identity) && identity->pbi_pid == (uint32_t)pid && identity->pbi_uid == getuid();
}

/** Order fixed-size display IDs without subtracting unsigned values into a signed comparison. */
static int compareDisplayIDs(const void *left, const void *right) {
  CGDirectDisplayID a = *(const CGDirectDisplayID *)left;
  CGDirectDisplayID b = *(const CGDirectDisplayID *)right;
  return (a > b) - (a < b);
}

/** Resolve one explicitly indexed active display through public noncapture metadata, only after capture intent. */
static BOOL activeDisplayAtIndex(NSUInteger index, CGDirectDisplayID *result) {
  uint32_t count = 0;
  if (CGGetActiveDisplayList(0, NULL, &count) != kCGErrorSuccess || count == 0 || count > 16) {
    /* A missing/oversized topology is unsupported; do not allocate an unbounded display inventory. */
    return NO;
  }
  CGDirectDisplayID displays[16] = {0};
  if (CGGetActiveDisplayList(16, displays, &count) != kCGErrorSuccess || count == 0 || count > 16 || index >= count) {
    /* A topology race or invalid caller index cannot silently select a different screen. */
    return NO;
  }
  qsort(displays, count, sizeof(*displays), compareDisplayIDs);
  *result = displays[index];
  return *result != kCGNullDirectDisplay;
}

/** One-shot observer; the serial queue owns capture transitions and stops before final process exit. */
@interface PresentedFrameObserver : NSObject <SCStreamOutput, SCStreamDelegate, SCContentSharingPickerObserver>
@property(nonatomic, strong) FrameOptions *options;
@property(nonatomic, strong) SCStream *stream;
@property(nonatomic, strong) dispatch_queue_t queue;
@property(nonatomic, strong) dispatch_source_t interruptSource;
@property(nonatomic, strong) dispatch_source_t terminateSource;
/* Main reads this one-way gate before picker presentation; only the serial queue closes it. */
@property(atomic) BOOL stopping;
@property(nonatomic) BOOL registeredOutput;
@property(nonatomic) NSInteger completeFrames;
@property(nonatomic) NSUInteger ignoredFrames;
@property(nonatomic) uint64_t previousDisplayTicks;
@property(nonatomic) uint64_t startAbsoluteTicks;
@property(nonatomic) uint64_t startContinuousTicks;
@property(nonatomic) mach_timebase_info_data_t timebase;
@property(nonatomic) struct proc_bsdinfo identity;
/* Picker ownership is published on main before callbacks; final cleanup also runs on main. */
@property(nonatomic, strong) SCContentSharingPicker *picker;
@property(nonatomic) BOOL pickerRegistered;
@property(nonatomic) BOOL pickerSelectionConsumed;
@property(nonatomic) CGDirectDisplayID expectedPickerDisplayID;
@property(nonatomic) BOOL targetWindowVerified;
/* Capture state and the compact semantic barrier stay on the existing serial owner queue. */
@property(nonatomic, strong) SCDisplay *captureDisplay;
@property(nonatomic) OmniFrameROIPlan roiPlan;
@property(nonatomic) OmniFrameSemanticState semanticState;
@property(nonatomic, strong) dispatch_queue_t visionQueue;
@property(nonatomic) BOOL semanticInFlight;
@property(nonatomic) BOOL semanticPrepared;
@property(nonatomic) BOOL actualFrameReleaseFailed;
@property(nonatomic, strong) OmniFrameRecognitionLifetime *bootstrapLifetime;
@property(nonatomic) BOOL nativeStopResolved;
@property(nonatomic) BOOL cleanupStarted;
@property(nonatomic, copy) NSString *pendingFinishReason;
@property(nonatomic) int pendingFinishStatus;
@property(nonatomic) BOOL pendingOutputRemoved;
@property(nonatomic) BOOL pendingStopCompleted;
- (void)start;
- (void)presentPicker API_AVAILABLE(macos(15.2));
- (void)acceptPickerFilter:(SCContentFilter *)filter picker:(SCContentSharingPicker *)picker stream:(SCStream *)stream API_AVAILABLE(macos(15.2));
- (void)startFilter:(SCContentFilter *)filter;
- (void)prepareRecognizerThenStartFilter:(SCContentFilter *)filter;
- (void)recognizeImage:(CVPixelBufferRef)image frame:(NSDictionary *)frame displayTicks:(uint64_t)displayTicks record:(NSDictionary *)record;
- (void)emitFrame:(NSDictionary *)record;
- (void)finish:(NSString *)reason status:(int)status;
- (void)finishIfDrained;
- (void)publishFinish:(NSString *)reason status:(int)status outputRemoved:(BOOL)outputRemoved stopCompleted:(BOOL)stopCompleted pickerReleased:(BOOL)pickerReleased;
@end

/* Main owns the observer until final completion; callbacks never retain an inactive reusable session. */
static PresentedFrameObserver *activeObserver;

@implementation PresentedFrameObserver

/** Check the same process incarnation before accepting or publishing another frame. */
- (BOOL)targetIsCurrent {
  struct proc_bsdinfo current = {0};
  struct proc_bsdinfo original = self.identity;
  NSRunningApplication *application = [NSRunningApplication runningApplicationWithProcessIdentifier:self.options.pid];
  return processIdentity(self.options.pid, &current) && application != nil && !application.terminated &&
         [application.bundleIdentifier isEqualToString:self.options.bundleID] &&
         current.pbi_start_tvsec == original.pbi_start_tvsec &&
         current.pbi_start_tvusec == original.pbi_start_tvusec;
}

/** Register bounded cancellation, then take exactly the caller-selected authorization route. */
- (void)start {
  self.queue = dispatch_queue_create("omni.synthetic-frame-observer", DISPATCH_QUEUE_SERIAL);
  if (self.queue == nil) {
    /* No callback or cancellation source can be installed without its serial owner queue. */
    _Exit(EX_OSERR);
  }
  dispatch_queue_t queue = self.queue;
  self.startAbsoluteTicks = mach_absolute_time();
  self.startContinuousTicks = mach_continuous_time();
  self.semanticState = (OmniFrameSemanticState){.startTicks = self.startAbsoluteTicks, .requiredBaselineFrames = self.options.baselineFrames, .continuous = self.options.continuousSemantic};
  if (self.options.expectedToken != nil) {
    /* One dedicated serial worker receives only an independently copied ROI, never a capture buffer. */
    self.visionQueue = dispatch_queue_create("omni.synthetic-frame-vision", DISPATCH_QUEUE_SERIAL);
    if (self.visionQueue == nil) {
      /* Missing serial worker cannot fall back to blocking capture delivery or unbounded global work. */
      _Exit(EX_OSERR);
    }
  }
  __weak PresentedFrameObserver *weakSelf = self;

  /* Signals request explicit asynchronous stop; they never turn a partial observation into success. */
  signal(SIGINT, SIG_IGN);
  signal(SIGTERM, SIG_IGN);
  self.interruptSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGINT, 0, self.queue);
  self.terminateSource = dispatch_source_create(DISPATCH_SOURCE_TYPE_SIGNAL, SIGTERM, 0, self.queue);
  if (self.interruptSource == nil || self.terminateSource == nil) {
    /* Resource acquisition failure has no stream to stop and cannot yield usable frame evidence. */
    _Exit(EX_OSERR);
  }
  dispatch_source_set_event_handler(self.interruptSource, ^{
    /* The queue serializes signal handling with frame callbacks and the stop state. */
    [weakSelf finish:@"interrupted" status:EX_TEMPFAIL];
  });
  dispatch_source_set_event_handler(self.terminateSource, ^{
    /* Termination follows the same release boundary instead of bypassing stream cancellation. */
    [weakSelf finish:@"terminated" status:EX_TEMPFAIL];
  });
  dispatch_resume(self.interruptSource);
  dispatch_resume(self.terminateSource);
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)(self.options.duration * NSEC_PER_SEC)), self.queue, ^{
    /* The deadline includes enumeration/startup and cannot wait indefinitely for changed content. */
    PresentedFrameObserver *owner = weakSelf;
    BOOL succeeded = owner.options.expectedToken == nil ? owner.completeFrames > 0 : owner.semanticState.matched;
    [owner finish:@"deadline" status:succeeded ? EX_OK : EX_UNAVAILABLE];
  });
  dispatch_after(dispatch_time(DISPATCH_TIME_NOW, (int64_t)((self.options.duration + 3.0) * NSEC_PER_SEC)), dispatch_get_global_queue(QOS_CLASS_UTILITY, 0), ^{
    /* A stuck native stop/output is a failed process, never a successful teardown or latency observation. */
    _Exit(EX_TEMPFAIL);
  });

  if (self.options.usesPicker) {
    /* Picker consent never falls through to a TCC preflight or broad shareable-content enumeration. */
    if (@available(macOS 15.2, *)) {
      dispatch_async(dispatch_get_main_queue(), ^{
        /* AppKit's real main event loop services this explicit UI operation; callbacks return to the owner queue. */
        [weakSelf presentPicker];
      });
    } else {
      /* Main rejects this route before start, but retain a defensive no-UI failure at the native boundary. */
      dispatch_async(queue, ^{
        [weakSelf finish:@"picker_filter_inspection_unavailable" status:EX_UNAVAILABLE];
      });
    }
    return;
  }

  /* Content enumeration itself may require TCC; main already refused missing permission without requesting it. */
  [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:YES completionHandler:^(SCShareableContent *content, NSError *error) {
    dispatch_async(queue, ^{
      @autoreleasepool {
        /* Late acquisition cannot create a stream after cancellation/deadline has closed the observer. */
        PresentedFrameObserver *owner = weakSelf;
        if (owner == nil || owner.stopping) {
          return;
        }
        if (error != nil || content == nil || ![owner targetIsCurrent]) {
          /* No fallback captures the entire desktop or adopts a recycled/foreign PID. */
          [owner finish:@"content_or_identity_unavailable" status:EX_UNAVAILABLE];
          return;
        }
        NSArray<SCDisplay *> *displays = [content.displays sortedArrayUsingComparator:^NSComparisonResult(SCDisplay *left, SCDisplay *right) {
          /* Sort transient display IDs only for deterministic indexing; never emit device identifiers. */
          return [@(left.displayID) compare:@(right.displayID)];
        }];
        if (owner.options.displayIndex >= displays.count) {
          /* A missing explicit display is an error rather than permission to capture another screen. */
          [owner finish:@"display_index_unavailable" status:EX_UNAVAILABLE];
          return;
        }
        SCRunningApplication *target = nil;
        for (SCRunningApplication *application in content.applications) {
          /* Select the exact authorized synthetic process and bundle ID, never a partial process name. */
          if (application.processID == owner.options.pid && [application.bundleIdentifier isEqualToString:owner.options.bundleID]) {
            target = application;
            break;
          }
        }
        if (target == nil) {
          /* A missing or changed identity cannot widen the filter to other applications. */
          [owner finish:@"synthetic_application_unavailable" status:EX_UNAVAILABLE];
          return;
        }
        SCDisplay *selectedDisplay = displays[owner.options.displayIndex];
        owner.captureDisplay = selectedDisplay;
        BOOL hasWindowOnDisplay = NO;
        for (SCWindow *window in content.windows) {
          /* A listed app on another display is not evidence of any content in the selected display scope. */
          if (window.owningApplication.processID == target.processID &&
              CGRectIntersectsRect(window.frame, selectedDisplay.frame)) {
            hasWindowOnDisplay = YES;
            break;
          }
        }
        if (!hasWindowOnDisplay) {
          /* Refuse an empty target scope instead of counting unrelated/blank display updates. */
          [owner finish:@"target_window_not_on_selected_display" status:EX_UNAVAILABLE];
          return;
        }
        owner.targetWindowVerified = YES;

        /* A DISPLAY filter contains only this app's windows; desktop/dock and all other apps are excluded. */
        SCContentFilter *filter = [[SCContentFilter alloc] initWithDisplay:selectedDisplay includingApplications:@[target] exceptingWindows:@[]];
        [owner startFilter:filter];
      }
    });
  }];
}

/** Present a single-application consent UI only on the explicitly selected, availability-checked route. */
- (void)presentPicker {
  if (self.stopping) {
    /* A queued presentation after deadline/cancellation cannot initiate another consent session. */
    return;
  }
  if (![self targetIsCurrent] || !activeDisplayAtIndex(self.options.displayIndex, &_expectedPickerDisplayID)) {
    /* Display lookup is noncapture metadata; failed identity/topology never falls back to enumeration/TCC. */
    __weak PresentedFrameObserver *weakSelf = self;
    dispatch_async(self.queue, ^{
      [weakSelf finish:@"picker_identity_or_display_unavailable" status:EX_UNAVAILABLE];
    });
    return;
  }

  /* Publish main-owned picker state before native registration can deliver callbacks. */
  SCContentSharingPickerConfiguration *configuration = [SCContentSharingPickerConfiguration new];
  configuration.allowedPickerModes = SCContentSharingPickerModeSingleApplication;
  configuration.allowsChangingSelectedContent = NO;
  self.picker = SCContentSharingPicker.sharedPicker;
  self.picker.defaultConfiguration = configuration;
  self.picker.maximumStreamCount = @1;
  [self.picker addObserver:self];
  self.pickerRegistered = YES;
  self.picker.active = YES;
  /* This is the only UI-producing call, reachable solely from --capture --picker. */
  [self.picker presentPickerUsingContentStyle:SCShareableContentStyleApplication];
}

/** Validate the returned authorization itself; never substitute a reconstructed or broader filter. */
- (void)acceptPickerFilter:(SCContentFilter *)filter picker:(SCContentSharingPicker *)picker stream:(SCStream *)stream {
  if (self.stopping) {
    /* Native callbacks can already be queued when removal completes; they have no reopening authority. */
    return;
  }
  if (picker != self.picker || stream != nil || self.pickerSelectionConsumed || self.stream != nil) {
    /* Only the first new-stream selection belongs to this one-shot observer; updates stop rather than switch scope. */
    [self finish:@"picker_unexpected_update" status:EX_UNAVAILABLE];
    return;
  }
  self.pickerSelectionConsumed = YES;
  if (filter == nil || filter.style != SCShareableContentStyleApplication ||
      filter.includedApplications.count != 1 || filter.includedDisplays.count != 1 ||
      filter.includedWindows.count > 128 || ![self targetIsCurrent]) {
    /* Window/full-display/multiple-app/uninspectable scopes are rejected before any stream exists. */
    [self finish:@"picker_scope_not_exact_application" status:EX_NOPERM];
    return;
  }
  SCRunningApplication *selectedApplication = filter.includedApplications.firstObject;
  SCDisplay *selectedDisplay = filter.includedDisplays.firstObject;
  if (selectedApplication.processID != self.options.pid ||
      ![selectedApplication.bundleIdentifier isEqualToString:self.options.bundleID] ||
      selectedDisplay.displayID != self.expectedPickerDisplayID ||
      !CGDisplayIsActive(selectedDisplay.displayID)) {
    /* The declared PID, bundle, original process birth and chosen live display must all still agree. */
    [self finish:@"picker_selected_identity_or_display_mismatch" status:EX_NOPERM];
    return;
  }
  self.captureDisplay = selectedDisplay;
  for (SCWindow *window in filter.includedWindows) {
    /* App filters may describe implicit windows; any explicit included window must have this exact owner. */
    SCRunningApplication *application = window.owningApplication;
    if (application == nil || application.processID != self.options.pid ||
        ![application.bundleIdentifier isEqualToString:self.options.bundleID]) {
      /* An excepted foreign window cannot ride along with an otherwise correct application selection. */
      [self finish:@"picker_foreign_window_in_filter" status:EX_NOPERM];
      return;
    }
    if (window.onScreen && CGRectIntersectsRect(window.frame, selectedDisplay.frame)) {
      /* This metadata supports visibility only when actually present; an empty implicit list is not proof. */
      self.targetWindowVerified = YES;
    }
  }
  /* Narrow the picker-owned filter's optional menu content without replacing its scoped session grant. */
  filter.includeMenuBar = NO;
  [self startFilter:filter];
}

/** Marshal selection callbacks onto the one capture-state queue without retaining an inactive owner. */
- (void)contentSharingPicker:(SCContentSharingPicker *)picker didUpdateWithFilter:(SCContentFilter *)filter forStream:(SCStream *)stream {
  __weak PresentedFrameObserver *weakSelf = self;
  dispatch_async(self.queue, ^{
    /* Keep only the returned filter until validation; no pixels or app/window names enter output. */
    if (@available(macOS 15.2, *)) {
      [weakSelf acceptPickerFilter:filter picker:picker stream:stream];
    } else {
      /* Older systems cannot authorize a scope that the public inspection API cannot prove. */
      [weakSelf finish:@"picker_filter_inspection_unavailable" status:EX_UNAVAILABLE];
    }
  });
}

/** Cancellation is a failed observation with the same cleanup boundary as stream errors/deadlines. */
- (void)contentSharingPicker:(SCContentSharingPicker *)picker didCancelForStream:(SCStream *)stream {
  (void)picker;
  (void)stream;
  __weak PresentedFrameObserver *weakSelf = self;
  dispatch_async(self.queue, ^{
    /* A canceled dialog neither requests global recording access nor creates an empty successful sample. */
    [weakSelf finish:@"picker_cancelled" status:EX_TEMPFAIL];
  });
}

/** Fail picker startup without logging native descriptions that may contain unrelated app/window data. */
- (void)contentSharingPickerStartDidFailWithError:(NSError *)error {
  (void)error;
  __weak PresentedFrameObserver *weakSelf = self;
  dispatch_async(self.queue, ^{
    /* Failure never falls through to CGRequestScreenCaptureAccess or broader content enumeration. */
    [weakSelf finish:@"picker_start_failure" status:EX_UNAVAILABLE];
  });
}

/** Prepare Vision on a blank owned image before capture; startup may retain one content-free framework request. */
- (void)prepareRecognizerThenStartFilter:(SCContentFilter *)filter {
  self.semanticInFlight = YES;
  OmniFrameRecognitionLifetime *lifetime = [OmniFrameRecognitionLifetime new];
  NSString *expected = self.options.expectedToken;
  NSString *baseline = self.options.baselineToken;
  dispatch_queue_t queue = self.queue;
  __weak PresentedFrameObserver *weakSelf = self;
  dispatch_async(self.visionQueue, ^{
    @autoreleasepool {
      /* The first request contains only a white private image; no capture sample or target text enters startup. */
      CVPixelBufferRef blank = OmniFrameCreateBlankCrop();
      OmniFrameRecognition recognition = OmniFrameRecognize(blank, expected, baseline, lifetime);
      if (blank != NULL) {
        /* Release the sole image before either capture starts or a closing observer crosses its release barrier. */
        CFRelease(blank);
      }
      BOOL prepared = OmniFrameBootstrapIsValid(recognition, lifetime);
      dispatch_async(queue, ^{
        PresentedFrameObserver *owner = weakSelf;
        if (owner == nil) {
          /* A missing mandatory owner cannot publish successful startup or silently strand the watchdog. */
          _Exit(EX_SOFTWARE);
        }
        owner.semanticInFlight = NO;
        owner.bootstrapLifetime = lifetime;
        if (owner.stopping) {
          /* Deadline/cancellation wins over startup; no delayed bootstrap can create a stream afterward. */
          if (!prepared) {
            /* Failed startup image/result release independently prevents a successful close report. */
            owner.pendingFinishStatus = EX_UNAVAILABLE;
          }
          [owner finishIfDrained];
          return;
        }
        if (!prepared) {
          /* OCR failure, any recognized content or retained startup image/result rejects semantic capture entirely. */
          [owner finish:@"semantic_blank_bootstrap_invalid" status:EX_UNAVAILABLE];
          return;
        }
        owner.semanticPrepared = YES;
        [owner startFilter:filter];
      });
    }
  });
}

/** Start only a validated original filter on the serial owner queue, preserving the bounded output contract. */
- (void)startFilter:(SCContentFilter *)filter {
  if (self.stopping) {
    /* A late picker/enumeration completion cannot create capture resources after the close gate. */
    return;
  }
  if (filter == nil || self.stream != nil || ![self targetIsCurrent]) {
    /* Duplicate acquisition or process replacement never upgrades or replaces the one accepted scope. */
    [self finish:@"filter_or_identity_invalid" status:EX_UNAVAILABLE];
    return;
  }
  if (self.options.expectedToken != nil && !self.semanticPrepared) {
    /* Cold model setup is outside frame processing; every subsequent actual request retains the strict release gate. */
    [self prepareRecognizerThenStartFilter:filter];
    return;
  }
  PresentedFrameObserver *owner = self;
  dispatch_queue_t queue = self.queue;
  SCStreamConfiguration *configuration = [SCStreamConfiguration new];
  configuration.width = outputWidth;
  configuration.height = outputHeight;
  configuration.pixelFormat = kCVPixelFormatType_32BGRA;
  configuration.queueDepth = 3;
  configuration.minimumFrameInterval = CMTimeMake(1, 60);
  configuration.showsCursor = NO;
  configuration.capturesAudio = NO;
  if (@available(macOS 14.2, *)) {
    /* Child-window augmentation could widen the selected owner scope; capture only filtered content. */
    configuration.includeChildWindows = NO;
  } else {
    /* Main rejects this version before permission/content acquisition; never construct a wider stream here. */
    [owner finish:@"child_window_scope_control_unavailable" status:EX_UNAVAILABLE];
    return;
  }
  if (@available(macOS 15.0, *)) {
    /* A system picker must not implicitly turn the metadata-only screen observer into microphone capture. */
    configuration.captureMicrophone = NO;
  }
  if (owner.options.expectedToken != nil) {
    /* Configure native-resolution acquisition of one declared ROI, with no full-display pixel allocation. */
    OmniFrameROIPlan plan = {0};
    if (owner.captureDisplay == nil || !OmniFrameMakeROIPlan(owner.options.sourceROI, owner.captureDisplay.frame,
            filter.contentRect, filter.pointPixelScale, &plan)) {
      /* An unknown transform/scale/extent is a failed observation, never a guessed glyph crop. */
      [owner finish:@"semantic_source_geometry_unproven" status:EX_DATAERR];
      return;
    }
    owner.roiPlan = plan;
    configuration.sourceRect = plan.sourcePoints;
    configuration.width = plan.pixelWidth;
    configuration.height = plan.pixelHeight;
    configuration.destinationRect = CGRectMake(0, 0, (CGFloat)plan.pixelWidth, (CGFloat)plan.pixelHeight);
  }
  /* Declare acquisition geometry and raw clock units; configuration alone proves no glyph transition or latency. */
  NSDictionary *configurationRecord = @{
    @"schema": @1,
    @"kind": @"configuration",
    @"scope": @"synthetic_app_display_filtered",
    @"authorization_mode": owner.options.usesPicker ? @"picker_application_session" : @"existing_screen_recording",
    @"target_window_metadata_verified": booleanValue(owner.targetWindowVerified),
    @"semantic_enabled": booleanValue(owner.options.expectedToken != nil),
    @"continuous_semantic": booleanValue(owner.options.continuousSemantic),
    @"expected_token_id": owner.options.expectedToken != nil ? OmniFrameTokenIdentifier(owner.options.expectedToken) : [NSNull null],
    @"baseline_token_id": owner.options.baselineToken != nil ? OmniFrameTokenIdentifier(owner.options.baselineToken) : [NSNull null],
    @"required_baseline_frames": @(owner.options.expectedToken != nil ? owner.options.baselineFrames : 0),
    @"source_roi_points": owner.options.expectedToken != nil ? @[@(owner.options.sourceROI.origin.x), @(owner.options.sourceROI.origin.y), @(owner.options.sourceROI.size.width), @(owner.options.sourceROI.size.height)] : [NSNull null],
    @"bootstrap_request_retained": booleanValue(owner.bootstrapLifetime.request != nil),
    @"semantic_match": @NO,
    @"latency_acceptance": @NO,
    @"display_clock": @"mach_absolute_ticks",
    @"callback_absolute_clock": @"mach_absolute_ticks",
    @"callback_continuous_clock": @"mach_continuous_ticks",
    @"timebase_numer": @(owner.timebase.numer),
    @"timebase_denom": @(owner.timebase.denom),
    @"start_absolute_ticks": clockTicks(owner.startAbsoluteTicks),
    @"start_continuous_ticks": clockTicks(owner.startContinuousTicks),
    @"width": @(configuration.width),
    @"height": @(configuration.height),
    @"queue_depth": @3,
    @"desired_max_fps": @60
  };
  if (!emitRecord(configurationRecord)) {
    /* Output is bounded metadata only; write failure cannot proceed as an unrecorded measurement. */
    [owner finish:@"output_failure" status:EX_IOERR];
    return;
  }
  owner.stream = [[SCStream alloc] initWithFilter:filter configuration:configuration delegate:owner];
  NSError *registrationError = nil;
  if (![owner.stream addStreamOutput:owner type:SCStreamOutputTypeScreen sampleHandlerQueue:owner.queue error:&registrationError]) {
    /* A stream without the intended observer has no accepted content and must still be stopped. */
    [owner finish:@"output_registration_failure" status:EX_UNAVAILABLE];
    return;
  }
  owner.registeredOutput = YES;
  __weak PresentedFrameObserver *startOwner = owner;
  [owner.stream startCaptureWithCompletionHandler:^(NSError *startError) {
    dispatch_async(queue, ^{
      /* Completion may race with the finite deadline; a closing observer never resumes capture. */
      PresentedFrameObserver *current = startOwner;
      if (current != nil && !current.stopping && startError != nil) {
        [current finish:@"capture_start_failure" status:EX_UNAVAILABLE];
      }
    });
  }];
}

/** Admit Complete metadata first; optional semantic work copies only a separately verified bounded ROI. */
- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer ofType:(SCStreamOutputType)type {
  @autoreleasepool {
    if (self.stopping || stream != self.stream || type != SCStreamOutputTypeScreen) {
      /* Closing, foreign-stream, and unsupported outputs cannot produce new accepted metadata. */
      return;
    }
    if (![self targetIsCurrent] || !CMSampleBufferIsValid(sampleBuffer)) {
      /* A process-reuse or invalid sample fails the observation instead of attributing unrelated content. */
      [self finish:@"identity_or_sample_invalid" status:EX_UNAVAILABLE];
      return;
    }
    CFArrayRef attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, false);
    if (attachments == NULL || CFArrayGetCount(attachments) != 1) {
      /* Missing or ambiguous attachment metadata cannot establish a displayed frame timestamp. */
      [self finish:@"frame_attachments_missing" status:EX_DATAERR];
      return;
    }
    CFTypeRef rawFrame = CFArrayGetValueAtIndex(attachments, 0);
    if (rawFrame == NULL || CFGetTypeID(rawFrame) != CFDictionaryGetTypeID()) {
      /* A malformed native attachment must not be messaged as an Objective-C dictionary. */
      [self finish:@"frame_attachment_type_invalid" status:EX_DATAERR];
      return;
    }
    /* SAFETY: the callback's live sample owns this verified dictionary; bridging borrows without transferring it. */
    NSDictionary *frame = (__bridge NSDictionary *)rawFrame;
    NSNumber *status = frame[SCStreamFrameInfoStatus];
    if (![status isKindOfClass:[NSNumber class]]) {
      /* Native metadata is validated before interpreting enum/integer representations. */
      [self finish:@"frame_status_missing" status:EX_DATAERR];
      return;
    }
    if (status.integerValue != SCFrameStatusComplete) {
      /* Idle/started frames are not new content; blank/suspended streams cannot support unlocked observations. */
      self.ignoredFrames += 1;
      if (status.integerValue == SCFrameStatusBlank || status.integerValue == SCFrameStatusSuspended || status.integerValue == SCFrameStatusStopped) {
        /* Refuse to label a blank/paused/stopped display as valid visible output. */
        [self finish:@"display_blank_suspended_or_stopped" status:EX_UNAVAILABLE];
      }
      return;
    }
    NSNumber *display = frame[SCStreamFrameInfoDisplayTime];
    CVImageBufferRef image = CMSampleBufferGetImageBuffer(sampleBuffer);
    if (![display isKindOfClass:[NSNumber class]] || [display compare:@0] != NSOrderedDescending || image == NULL || CFGetTypeID(image) != CVPixelBufferGetTypeID()) {
      /* A Complete status alone is insufficient without a timestamp and a structurally present image. */
      [self finish:@"complete_frame_metadata_missing" status:EX_DATAERR];
      return;
    }
    uint64_t displayTicks = display.unsignedLongLongValue;
    uint64_t absoluteTicks = mach_absolute_time();
    uint64_t continuousTicks = mach_continuous_time();
    size_t width = CVPixelBufferGetWidth(image), height = CVPixelBufferGetHeight(image);
    if (width > outputWidth || height > outputHeight || width == 0 || height == 0) {
      /* Refuse unexpected buffer dimensions without touching its potentially unrelated pixel storage. */
      [self finish:@"frame_dimensions_outside_budget" status:EX_DATAERR];
      return;
    }
    self.completeFrames += 1;
    /* Build scalar timing evidence before optional ROI access; no sample/image enters the asynchronous record. */
    NSDictionary *record = @{
      @"schema": @1,
      @"kind": @"complete_frame",
      @"index": @(self.completeFrames),
      @"display_ticks": clockTicks(displayTicks),
      @"callback_absolute_ticks": clockTicks(absoluteTicks),
      @"callback_continuous_ticks": clockTicks(continuousTicks),
      @"display_ticks_increasing": booleanValue(self.previousDisplayTicks == 0 || displayTicks > self.previousDisplayTicks),
      @"callback_after_display": booleanValue(absoluteTicks >= displayTicks),
      @"width": @(width),
      @"height": @(height)
    };
    self.previousDisplayTicks = displayTicks;
    if (self.options.expectedToken != nil) {
      /* The worker receives only independent ROI memory and scalar metadata, never this sample/image/frame dictionary. */
      [self recognizeImage:image frame:frame displayTicks:displayTicks record:record];
    } else {
      /* Metadata-only default never asks for a pixel base address or creates a Vision handler. */
      [self emitFrame:record];
    }
  }
}

/** Copy one verified cropped frame, then perform exactly one serial Vision job with independent image ownership. */
- (void)recognizeImage:(CVPixelBufferRef)image frame:(NSDictionary *)frame displayTicks:(uint64_t)displayTicks record:(NSDictionary *)record {
  if (self.semanticInFlight) {
    /* Never queue another image or silently skip a frame that could have contained an earlier token transition. */
    [self finish:@"semantic_worker_overrun" status:EX_TEMPFAIL];
    return;
  }
  NSNumber *scale = frame[SCStreamFrameInfoScaleFactor];
  NSNumber *contentScale = frame[SCStreamFrameInfoContentScale];
  id contentValue = frame[SCStreamFrameInfoContentRect];
  CGRect content = CGRectZero;
  if (![scale isKindOfClass:[NSNumber class]] || ![contentScale isKindOfClass:[NSNumber class]] ||
      ![contentValue isKindOfClass:[NSDictionary class]] ||
      !CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)contentValue, &content) ||
      !OmniFrameGeometryMatches(self.roiPlan, content, scale.doubleValue, contentScale.doubleValue,
          CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image))) {
    /* Every pixel read is gated by the actual Complete-frame geometry, not a presumed configured transform. */
    [self finish:@"semantic_frame_geometry_mismatch" status:EX_DATAERR];
    return;
  }
  CVPixelBufferRef crop = OmniFrameCopyOwnedCrop(image, CGRectMake(0, 0, (CGFloat)self.roiPlan.pixelWidth, (CGFloat)self.roiPlan.pixelHeight));
  if (crop == NULL) {
    /* Crop lease/layout/allocation failure cannot masquerade as absence of the expected token. */
    [self finish:@"semantic_crop_unavailable" status:EX_UNAVAILABLE];
    return;
  }
  self.semanticInFlight = YES;
  NSString *expected = self.options.expectedToken;
  NSString *baseline = self.options.baselineToken;
  dispatch_queue_t queue = self.queue;
  __weak PresentedFrameObserver *weakSelf = self;
  dispatch_async(self.visionQueue, ^{
    /* This block owns only the CF-created crop plus bounded strings/scalar metadata; no capture buffer is retained. */
    OmniFrameRecognition recognition = OmniFrameRecognize(crop, expected, baseline, nil);
    CFRelease(crop);
    recognition.nativeObjectsReleased &= OmniFrameOwnedCropBytes() == 0;
    dispatch_async(queue, ^{
      @autoreleasepool {
        PresentedFrameObserver *owner = weakSelf;
        if (owner == nil) {
          /* A lost owner cannot publish a result or leave the still-running process waiting without its barrier. */
          _Exit(EX_SOFTWARE);
        }
        owner.semanticInFlight = NO;
        owner.actualFrameReleaseFailed |= !recognition.nativeObjectsReleased;
        if (owner.stopping) {
          /* A late OCR result releases resources only; it cannot certify paint after deadline/cancellation. */
          if (!recognition.nativeObjectsReleased) {
            owner.pendingFinishStatus = EX_UNAVAILABLE;
          }
          [owner finishIfDrained];
          return;
        }
        OmniFrameSemanticOutcome outcome = OmniFrameAdvance(&owner->_semanticState, recognition, displayTicks);
        NSString *failure = nil;
        switch (outcome) {
          case OmniFrameSemanticWaiting:
            /* Low/absent recognition remains incomplete; OCR silence never establishes an old state. */
            break;
          case OmniFrameSemanticBaselineReady:
            /* A positive OLD token and observed NEW absence are now available for a later actual input. */
            break;
          case OmniFrameSemanticMatched:
            /* A strictly later exact NEW token completes only an observed-paint transition, without input latency. */
            break;
          case OmniFrameSemanticStale:
            /* Repeated/out-of-order/pre-session frames are failures even if their text happens to match. */
            failure = @"semantic_stale_frame";
            break;
          case OmniFrameSemanticPreexisting:
            /* NEW before a positive OLD control cannot become a falsely fast edit result. */
            failure = @"semantic_expected_preexists";
            break;
          case OmniFrameSemanticInvalid:
            /* Ambiguous/failed recognition or retained Vision resources cannot contribute accepted evidence. */
            failure = @"semantic_recognition_or_release_invalid";
            break;
        }
        NSMutableDictionary *semanticRecord = [record mutableCopy];
        semanticRecord[@"expected_token_id"] = OmniFrameTokenIdentifier(expected);
        semanticRecord[@"semantic_match"] = booleanValue(recognition.expectedCertain);
        semanticRecord[@"vision_frame_graph_released"] = booleanValue(recognition.nativeObjectsReleased);
        semanticRecord[@"baseline_token_observed"] = booleanValue(recognition.baselineCertain);
        semanticRecord[@"baseline_ready"] = booleanValue(owner.semanticState.baselineReady);
        semanticRecord[@"semantic_paint_observed"] = booleanValue(outcome == OmniFrameSemanticMatched);
        semanticRecord[@"latency_acceptance"] = @NO;
        if (failure != nil) {
          /* Emit only bounded predicates for the rejected sample, never recognized text or an image. */
          BOOL emitted = emitRecord(semanticRecord);
          [owner finish:emitted ? failure : @"output_failure" status:emitted ? EX_DATAERR : EX_IOERR];
        } else {
          /* One combined frame record preserves the original 600-frame / 602-record output bound. */
          [owner emitFrame:semanticRecord];
        }
      }
    });
  });
}

/** Preserve finite output limits, requiring a semantic transition rather than generic frames when opted in. */
- (void)emitFrame:(NSDictionary *)record {
  if (!emitRecord(record)) {
    /* Write failure cannot produce accepted evidence in either mode. */
    [self finish:@"output_failure" status:EX_IOERR];
  } else if (self.options.expectedToken != nil && self.semanticState.matched && !self.options.continuousSemantic) {
    /* One-shot observation is complete; explicit continuous mode retains subsequent OLD/NEW frame predicates. */
    [self finish:@"semantic_token_observed" status:EX_OK];
  } else if (self.completeFrames >= self.options.frames) {
    /* Exhausting semantic frames without a verified transition is failure; metadata mode retains its prior behavior. */
    [self finish:@"frame_limit" status:(self.options.expectedToken == nil || self.semanticState.matched) ? EX_OK : EX_UNAVAILABLE];
  }
}

/** Route foreign-queue native stop notifications through the same one-shot state transition. */
- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
  (void)error;
  __weak PresentedFrameObserver *weakSelf = self;
  dispatch_async(self.queue, ^{
    /* A delayed notification cannot finish or revive another collector/stream. */
    PresentedFrameObserver *owner = weakSelf;
    if (owner != nil && !owner.stopping && stream == owner.stream) {
      [owner finish:@"native_stream_stopped" status:EX_UNAVAILABLE];
    }
  });
}

/** Stop accepting frames before unregistering output, then stop capture and release the owned stream. */
- (void)finish:(NSString *)reason status:(int)status {
  if (self.stopping) {
    /* Repeated stop/error/deadline callbacks coalesce without emitting a false second completion. */
    return;
  }
  self.stopping = YES;
  self.pendingFinishReason = reason;
  self.pendingFinishStatus = status;
  dispatch_source_cancel(self.interruptSource);
  dispatch_source_cancel(self.terminateSource);
  self.interruptSource = nil;
  self.terminateSource = nil;
  int finalStatus = status;
  BOOL outputRemoved = YES;
  if (self.registeredOutput) {
    /* Removing the output breaks the stream-to-observer link before native stop completion. */
    NSError *removalError = nil;
    if (![self.stream removeStreamOutput:self type:SCStreamOutputTypeScreen error:&removalError]) {
      /* A failed native release remains a failure even if the requested frame count was reached. */
      finalStatus = EX_UNAVAILABLE;
      outputRemoved = NO;
    }
    self.registeredOutput = NO;
  }
  self.pendingOutputRemoved = outputRemoved;
  self.pendingFinishStatus = finalStatus;
  SCStream *stream = self.stream;
  dispatch_queue_t queue = self.queue;
  __weak PresentedFrameObserver *weakSelf = self;
  void (^complete)(NSError *) = ^(NSError *error) {
    dispatch_async(queue, ^{
      @autoreleasepool {
        /* Serialize final release/output after native stop; the independent watchdog bounds stuck callbacks. */
        PresentedFrameObserver *owner = weakSelf;
        if (owner == nil) {
          /* Missing finalization ownership cannot emit a successful stop or leave an indefinitely waiting process. */
          _Exit(EX_SOFTWARE);
        }
        owner.stream = nil;
        if (error != nil) {
          /* Preserve an earlier release failure; a native stop error independently prevents acceptance. */
          owner.pendingFinishStatus = EX_UNAVAILABLE;
        }
        owner.nativeStopResolved = YES;
        owner.pendingStopCompleted = error == nil;
        [owner finishIfDrained];
      }
    });
  };
  if (stream != nil) {
    /* Normal capture/acquisition failure both receive an explicit native stop attempt. */
    [stream stopCaptureWithCompletionHandler:complete];
  } else {
    /* Enumeration/validation failed before a stream existed, so there is no native owner to wait for. */
    complete(nil);
  }
}

/** Join native stop with the sole OCR job before revoking picker state and publishing final release facts. */
- (void)finishIfDrained {
  if (!self.stopping || !self.nativeStopResolved || self.semanticInFlight || self.cleanupStarted) {
    /* A stop/job callback can arrive in either order; only the first complete barrier starts final cleanup. */
    return;
  }
  self.cleanupStarted = YES;
  if (OmniFrameOwnedCropBytes() != 0) {
    /* A finished worker with retained crop bytes is a release failure, never successful bounded cleanup. */
    self.pendingFinishStatus = EX_UNAVAILABLE;
  }
  self.visionQueue = nil;
  self.captureDisplay = nil;
  PresentedFrameObserver *owner = self;
  NSString *reason = self.pendingFinishReason;
  int result = self.pendingFinishStatus;
  BOOL outputRemoved = self.pendingOutputRemoved;
  BOOL stopCompleted = self.pendingStopCompleted;
  dispatch_queue_t queue = self.queue;
  __weak PresentedFrameObserver *weakSelf = self;
  if (self.options.usesPicker) {
    /* Stop acknowledgement precedes main-owned picker revocation, avoiding an artificial stop error. */
    dispatch_async(dispatch_get_main_queue(), ^{
      PresentedFrameObserver *mainOwner = weakSelf;
      if (mainOwner == nil) {
        /* Losing the mandatory owner before consent cleanup is a failed teardown. */
        _Exit(EX_SOFTWARE);
      }
      SCContentSharingPicker *picker = mainOwner.picker;
      if (mainOwner.pickerRegistered) {
        /* Explicitly break any picker-to-observer link before clearing the strong picker field. */
        [picker removeObserver:mainOwner];
        mainOwner.pickerRegistered = NO;
      }
      if (picker != nil) {
        /* Deactivate this one-shot client; there is no automatic replacement/reopened sharing session. */
        picker.active = NO;
        picker.maximumStreamCount = @0;
      }
      BOOL released = !mainOwner.pickerRegistered && (picker == nil || !picker.active);
      mainOwner.picker = nil;
      dispatch_async(queue, ^{
        /* Main completion is a synchronization boundary; no later callback can pass the closed gate. */
        [weakSelf publishFinish:reason status:result outputRemoved:outputRemoved stopCompleted:stopCompleted pickerReleased:released];
      });
    });
  } else {
    /* Existing recording authorization has no picker/session UI owner to deactivate. */
    [owner publishFinish:reason status:result outputRemoved:outputRemoved stopCompleted:stopCompleted pickerReleased:YES];
  }
}

/** Publish one final bounded record after native stop and applicable main-thread picker cleanup. */
- (void)publishFinish:(NSString *)reason status:(int)status outputRemoved:(BOOL)outputRemoved stopCompleted:(BOOL)stopCompleted pickerReleased:(BOOL)pickerReleased {
  BOOL semanticSatisfied = self.options.expectedToken == nil || self.semanticState.matched;
  int result = status;
  if (result == EX_OK && (!pickerReleased || self.semanticInFlight || self.actualFrameReleaseFailed || OmniFrameOwnedCropBytes() != 0 || !semanticSatisfied)) {
    /* Missing success prerequisites revoke success without replacing an existing precise failure status. */
    result = EX_UNAVAILABLE;
  }
  BOOL emitted = emitRecord(@{
    @"schema": @1,
    @"kind": @"finished",
    @"reason": reason,
    @"authorization_mode": self.options.usesPicker ? @"picker_application_session" : @"existing_screen_recording",
    @"complete_frames": @(self.completeFrames),
    @"ignored_frames": @(self.ignoredFrames),
    @"semantic_enabled": booleanValue(self.options.expectedToken != nil),
    @"continuous_semantic": booleanValue(self.options.continuousSemantic),
    @"semantic_paint_observed": booleanValue(self.options.expectedToken != nil && self.semanticState.matched),
    @"semantic_baseline_ready": booleanValue(self.options.expectedToken != nil && self.semanticState.baselineReady),
    @"owned_crop_bytes_after_drain": @(OmniFrameOwnedCropBytes()),
    @"vision_job_drained": booleanValue(!self.semanticInFlight),
    @"all_actual_frame_graphs_released": booleanValue(!self.actualFrameReleaseFailed),
    @"bootstrap_request_retained": booleanValue(self.bootstrapLifetime.request != nil),
    @"bootstrap_handler_and_results_released": booleanValue(self.bootstrapLifetime.handler == nil && self.bootstrapLifetime.results == nil),
    @"output_removed": booleanValue(outputRemoved),
    @"stop_completed": booleanValue(stopCompleted),
    @"picker_client_deactivated_and_observer_removed": self.options.usesPicker ? booleanValue(pickerReleased) : [NSNull null],
    @"latency_acceptance": @NO,
    @"status": @(result)
  });
  /* This is client cleanup evidence only; process exit is not substituted for native stop acknowledgement. */
  activeObserver = nil;
  _Exit(emitted ? result : EX_IOERR);
}
@end

/** Validate noncapturing intent first, then exact identity and the caller-selected authorization policy. */
int main(int argc, const char **argv) {
  BOOL pickerEventLoop = NO;
  @autoreleasepool {
    if (argc == 2 && strcmp(argv[1], "--help") == 0) {
      /* Help cannot query recording permission, enumerate content, or display capture UI. */
      printUsage(stdout);
      return EX_OK;
    }
    FrameOptions *options = parseOptions(argc, argv);
    if (options == nil) {
      /* Invalid/missing/unknown arguments terminate before any permission or AppKit acquisition. */
      printUsage(stderr);
      return EX_USAGE;
    }
    if (options.validationOnly) {
      /* Successful parsing proves no live identity, permission, unlock state, or frame was checked. */
      BOOL emitted = emitRecord(@{
        @"schema": @1,
        @"kind": @"validated_options",
        @"capture_started": @NO,
        @"permission_checked": @NO,
        @"identity_checked": @NO,
        @"unlock_checked": @NO,
        @"picker_presented": @NO,
        @"authorization_mode": options.usesPicker ? @"picker_application_session" : @"existing_screen_recording",
        @"capture_minimum_macos": options.usesPicker ? @"15.2" : @"14.2",
        @"semantic_enabled": booleanValue(options.expectedToken != nil),
        @"continuous_semantic": booleanValue(options.continuousSemantic),
        @"expected_token_id": options.expectedToken != nil ? OmniFrameTokenIdentifier(options.expectedToken) : [NSNull null],
        @"baseline_token_id": options.baselineToken != nil ? OmniFrameTokenIdentifier(options.baselineToken) : [NSNull null],
        @"required_baseline_frames": @(options.expectedToken != nil ? options.baselineFrames : 0),
        @"source_roi_points": options.expectedToken != nil ? @[@(options.sourceROI.origin.x), @(options.sourceROI.origin.y), @(options.sourceROI.size.width), @(options.sourceROI.size.height)] : [NSNull null],
        @"pixels_accessed": @NO,
        @"vision_performed": @NO,
        @"latency_acceptance": @NO,
        @"frames": @(options.frames),
        @"duration_seconds": @(options.duration)
      });
      return emitted ? EX_OK : EX_IOERR;
    }
    if (@available(macOS 14.2, *)) {
      /* Exact owner scope can explicitly disable child-window augmentation; no content/permission API ran yet. */
    } else {
      /* Keeping a macOS 14 build floor is not permission to capture uncontrolled child-window content. */
      fprintf(stderr, "Exact application capture requires macOS 14.2 or later to exclude child windows.\n");
      return EX_UNAVAILABLE;
    }
    if (options.usesPicker) {
      if (@available(macOS 15.2, *)) {
        /* Public returned-filter inspection is available; this check itself acquires no UI or permission. */
      } else {
        /* The macOS 14 build floor does not justify accepting an uninspectable picker grant. */
        fprintf(stderr, "Exact application picker inspection requires macOS 15.2 or later.\n");
        return EX_UNAVAILABLE;
      }
    }
    struct proc_bsdinfo identity = {0};
    NSRunningApplication *application = [NSRunningApplication runningApplicationWithProcessIdentifier:options.pid];
    if (!processIdentity(options.pid, &identity) || application == nil || application.terminated ||
        ![application.bundleIdentifier isEqualToString:options.bundleID]) {
      /* A caller attestation cannot substitute for matching same-user process and exact bundle identity. */
      fprintf(stderr, "The exact same-user synthetic application identity is unavailable.\n");
      return EX_UNAVAILABLE;
    }
    if (!options.usesPicker) {
      /* Existing recording access and picker-session consent remain disjoint routes; neither repairs the other's failure. */
      if (!CGPreflightScreenCaptureAccess()) {
        /* Existing mode only preflights; it never requests permission or substitutes the picker route. */
        fprintf(stderr, "Existing screen-recording authorization is absent; this observer does not request access.\n");
        return EX_NOPERM;
      }
    }
    mach_timebase_info_data_t timebase = {0};
    if (mach_timebase_info(&timebase) != KERN_SUCCESS || timebase.numer == 0 || timebase.denom == 0) {
      /* Raw timestamps cannot be interpreted without their verified native timebase. */
      return EX_OSERR;
    }
    signal(SIGPIPE, SIG_IGN);
    if (options.usesPicker) {
      /* The explicit UI route needs a real AppKit main event loop, but creates no application window. */
      [NSApplication sharedApplication];
      [NSApp setActivationPolicy:NSApplicationActivationPolicyAccessory];
      pickerEventLoop = YES;
    }
    activeObserver = [PresentedFrameObserver new];
    activeObserver.options = options;
    activeObserver.identity = identity;
    activeObserver.timebase = timebase;
    [activeObserver start];
  }
  if (pickerEventLoop) {
    /* The picker is dispatched only after the application begins its main event loop; no run-loop polling exists. */
    [NSApp run];
    return EX_SOFTWARE;
  }
  /* Dispatch owns only this explicit observation; no application window, HID tap, or keyboard synthesis exists. */
  dispatch_main();
}
