/* Headless owned-image/semantic controls only: no AppKit, screen capture, picker, TCC, image files, or network calls. */
#import "semantic.h"
#import <CoreText/CoreText.h>
#include <mach/mach_time.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sysexits.h>

/* Tests retain only bounded timing scalars and weak-only native census records across fixtures. */
static unsigned int checks = 0;
static NSMutableArray<OmniFrameRecognitionLifetime *> *lifetimes;
static double fixtureMilliseconds[32];
static unsigned fixtureCount = 0;

/** Stop immediately on a contract failure without printing recognized image text or persisting any buffer. */
static void require(BOOL condition, const char *contract) {
  checks += 1;
  if (!condition) {
    /* Failure is immediately visible as a nonzero process exit, without a successful trailing summary. */
    fprintf(stderr, "Headless semantic contract failed: %s\n", contract);
    exit(EX_SOFTWARE);
  }
}

/** Track actual source release while the independent crop remains alive. */
static void releaseSource(void *context, const void *baseAddress) {
  *(BOOL *)context = YES;
  free((void *)baseAddress);
}

/** Render synthetic glyphs into owned BGRA memory using CoreText without creating a screen/window. */
static CVPixelBufferRef renderSource(NSString *text, size_t width, size_t height, BOOL *released) {
  size_t stride = width * 4;
  void *bytes = calloc(height, stride);
  require(bytes != NULL, "synthetic pixel allocation");
  CGColorSpaceRef color = CGColorSpaceCreateDeviceRGB();
  CGContextRef drawing = CGBitmapContextCreate(bytes, width, height, 8, stride, color,
      kCGBitmapByteOrder32Little | (CGBitmapInfo)kCGImageAlphaPremultipliedFirst);
  require(drawing != NULL, "owned bitmap context");
  CGContextSetRGBFillColor(drawing, 1, 1, 1, 1);
  CGContextFillRect(drawing, CGRectMake(0, 0, (CGFloat)width, (CGFloat)height));
  CTFontRef font = CTFontCreateWithName(CFSTR("Menlo"), 32, NULL);
  NSDictionary *attributes = @{(__bridge NSString *)kCTFontAttributeName: (__bridge id)font};
  NSAttributedString *attributed = [[NSAttributedString alloc] initWithString:text attributes:attributes];
  CTLineRef line = CTLineCreateWithAttributedString((__bridge CFAttributedStringRef)attributed);
  /* Quartz text coordinates are lower-left; the output has a stable upright image interpretation. */
  CGContextSetTextPosition(drawing, 12, (CGFloat)height - 80);
  CTLineDraw(line, drawing);
  /* CoreText/context objects release before the caller receives the independently owned synthetic buffer. */
  CFRelease(line);
  CFRelease(font);
  CGContextRelease(drawing);
  CGColorSpaceRelease(color);
  CVPixelBufferRef source = NULL;
  /* The caller's release flag must outlive this returned buffer; its final CFRelease triggers the callback once. */
  CVReturn created = CVPixelBufferCreateWithBytes(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA,
      bytes, stride, releaseSource, released, NULL, &source);
  require(created == kCVReturnSuccess && source != NULL, "synthetic source publication");
  return source;
}

/** Exercise recognition through the production crop/handler path while checking independent object lifetime. */
static OmniFrameRecognition recognizeFixture(NSString *text) {
  BOOL sourceReleased = NO;
  CVPixelBufferRef source = renderSource(text, 1024, 256, &sourceReleased);
  CVPixelBufferRef crop = OmniFrameCopyOwnedCrop(source, CGRectMake(0, 0, 512, 128));
  require(crop != NULL, "bounded independent crop acquisition");
  require(OmniFrameOwnedCropBytes() == 256 * 1024, "exact maximum owned crop bytes");
  /* A second crop cannot create an implicit image queue even when both callers use individually valid bounds. */
  require(OmniFrameCopyOwnedCrop(source, CGRectMake(0, 0, 1, 1)) == NULL, "single crop lease");
  CFRelease(source);
  require(sourceReleased, "crop does not retain original source/provider");
  OmniFrameRecognitionLifetime *lifetime = [OmniFrameRecognitionLifetime new];
  [lifetimes addObject:lifetime];
  uint64_t started = mach_absolute_time();
  __block OmniFrameRecognition recognition = {0};
  dispatch_queue_t queue = dispatch_queue_create("omni.frame.headless.vision", DISPATCH_QUEUE_SERIAL);
  dispatch_sync(queue, ^{
    /* Match the production one-job local pool; all Vision/image work ends before the weak census. */
    @autoreleasepool {
      recognition = OmniFrameRecognize(crop, @"FRAMEZX729B", @"FRAMEZX729A", lifetime);
      CFRelease(crop);
    }
  });
  mach_timebase_info_data_t timebase = {0};
  require(mach_timebase_info(&timebase) == KERN_SUCCESS && timebase.denom != 0, "native test timebase");
  require(fixtureCount < 32, "fixed fixture timing storage");
  fixtureMilliseconds[fixtureCount++] = (double)(mach_absolute_time() - started) * timebase.numer / timebase.denom / 1000000.0;
  if (OmniFrameOwnedCropBytes() != 0 || !recognition.nativeObjectsReleased) {
    /* A failed strict boundary reports scalar ownership evidence only, never recognized text or pixel storage. */
    fprintf(stderr, "{\"failed_fixture\":%u,\"owned_crop_bytes\":%zu,\"handler_alive\":%d,\"request_alive\":%d,\"results_alive\":%d,\"observation_count\":%u}\n",
        fixtureCount, OmniFrameOwnedCropBytes(), lifetime.handler != nil, lifetime.request != nil,
        lifetime.results != nil, recognition.observationCount);
  }
  require(OmniFrameOwnedCropBytes() == 0, "last crop release relinquishes all owned bytes");
  require(recognition.succeeded, "Vision completed on synthetic owned crop");
  require(recognition.nativeObjectsReleased && lifetime.handler == nil && lifetime.request == nil && lifetime.results == nil,
      "strict actual-frame handler/request/results release");
  return recognition;
}

/** Check literal boundaries, geometry, actual Vision controls, and baseline/time state independently. */
int main(void) {
  @autoreleasepool {
    lifetimes = [NSMutableArray new];
    /* Nonce grammar and literal token boundaries are pure checks, independent of Vision's actual OCR behavior below. */
    require(OmniFrameTokenIsValid(@"FRAMEZX729B"), "bounded valid nonce");
    require(OmniFrameTokenIsValid(@"测试FRAME729"), "bounded UTF-8 nonce");
    require(!OmniFrameTokenIsValid(@"\u0301\u0301\u0301\u0301"), "combining-mark-only nonce rejected");
    require(!OmniFrameTokenIsValid(@"--------"), "mark/connector-only nonce rejected");
    require(!OmniFrameTokenIsValid(@"short"), "too-short nonce");
    require(!OmniFrameTokenIsValid(@"FRAME ZX729B"), "whitespace is not a nonce boundary repair");
    require(OmniFrameContainsToken(@"prefix FRAMEZX729B suffix", @"FRAMEZX729B"), "whole whitespace-delimited token");
    require(!OmniFrameContainsToken(@"FRAMEZX729BB", @"FRAMEZX729B"), "longer token suffix rejected");
    require(!OmniFrameContainsToken(@"XFRAMEZX729B", @"FRAMEZX729B"), "longer token prefix rejected");
    require(!OmniFrameContainsToken(@"FRAMEZX7298", @"FRAMEZX729B"), "near-match glyph spelling rejected");
    require(!OmniFrameContainsToken(@"FRAMEZX729B\u0301", @"FRAMEZX729B"), "combining-mark suffix rejected");

    /* Exact ROI geometry is a separate prerequisite; successful token recognition cannot authorize an inferred transform. */
    CGRect requested = CGRectZero;
    require(OmniFrameParseROI("10,20,256,64", &requested), "finite exact source points");
    require(!OmniFrameParseROI("nan,20,256,64", &requested), "NaN ROI rejected");
    require(!OmniFrameParseROI("0,0,256,0", &requested), "zero crop height rejected");
    require(!OmniFrameParseROI("0,0,256,64,1", &requested), "trailing component rejected");
    OmniFrameROIPlan plan = {0};
    CGRect display = CGRectMake(0, 0, 1920, 1080);
    require(OmniFrameMakeROIPlan(CGRectMake(10, 20, 256, 64), display, display, 2, &plan), "native retina crop plan");
    require(plan.pixelWidth == 512 && plan.pixelHeight == 128, "retina points become exact bounded pixels");
    require(!OmniFrameMakeROIPlan(CGRectMake(10, 20, 257, 64), display, display, 2, &plan), "oversized native-width crop rejected");
    require(!OmniFrameMakeROIPlan(CGRectMake(1910, 20, 256, 64), display, display, 2, &plan), "source crop outside display rejected");
    require(!OmniFrameMakeROIPlan(CGRectMake(10, 20, 256, 64), CGRectMake(100, 0, 1920, 1080), display, 2, &plan), "ambiguous display origin rejected");
    require(OmniFrameGeometryMatches(plan, CGRectMake(0, 0, 256, 64), 2, 1, 512, 128), "per-frame exact content transform");
    require(!OmniFrameGeometryMatches(plan, CGRectMake(0, 0, 256, 64), 2, 0.5, 512, 128), "resampled content rejected");

    /* The copy is a real owned row snapshot: later writes/release of the source cannot change its bytes. */
    BOOL layoutSourceReleased = NO;
    CVPixelBufferRef layoutSource = renderSource(@"", 1024, 256, &layoutSourceReleased);
    require(OmniFrameCopyOwnedCrop(layoutSource, CGRectMake(1024, 0, 1, 1)) == NULL, "crop outside source rejected");
    require(OmniFrameCopyOwnedCrop(layoutSource, CGRectMake(0, 0, 513, 128)) == NULL, "crop width budget enforced");
    require(OmniFrameCopyOwnedCrop(layoutSource, CGRectMake(0, 0, 512, 129)) == NULL, "crop height budget enforced");
    require(OmniFrameCopyOwnedCrop(layoutSource, CGRectMake(0.5, 0, 1, 1)) == NULL, "fractional crop rejected");
    CVPixelBufferRef snapshot = OmniFrameCopyOwnedCrop(layoutSource, CGRectMake(1, 1, 1, 1));
    require(snapshot != NULL && OmniFrameOwnedCropBytes() == 4, "minimum crop owns exactly one BGRA pixel");
    require(CVPixelBufferLockBaseAddress(layoutSource, 0) == kCVReturnSuccess, "writable synthetic source lock");
    memset(CVPixelBufferGetBaseAddress(layoutSource), 0, CVPixelBufferGetBytesPerRow(layoutSource) * CVPixelBufferGetHeight(layoutSource));
    CVPixelBufferUnlockBaseAddress(layoutSource, 0);
    CFRelease(layoutSource);
    require(layoutSourceReleased, "source release independent from crop");
    require(CVPixelBufferLockBaseAddress(snapshot, kCVPixelBufferLock_ReadOnly) == kCVReturnSuccess, "owned snapshot lock");
    unsigned char *pixel = CVPixelBufferGetBaseAddress(snapshot);
    require(pixel != NULL && pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255 && pixel[3] == 255,
        "source mutation cannot alter independent crop");
    CVPixelBufferUnlockBaseAddress(snapshot, kCVPixelBufferLock_ReadOnly);
    CFRelease(snapshot);
    require(OmniFrameOwnedCropBytes() == 0, "minimum crop releases exact lease");

    /* A content-free bootstrap isolates the single startup request that Vision may retain internally. */
    OmniFrameRecognitionLifetime *bootstrapLifetime = [OmniFrameRecognitionLifetime new];
    CVPixelBufferRef blank = OmniFrameCreateBlankCrop();
    require(blank != NULL && OmniFrameOwnedCropBytes() == 256 * 1024, "bounded blank bootstrap acquisition");
    OmniFrameRecognition bootstrap = OmniFrameRecognize(blank, @"FRAMEZX729B", @"FRAMEZX729A", bootstrapLifetime);
    CFRelease(blank);
    require(OmniFrameBootstrapIsValid(bootstrap, bootstrapLifetime), "bootstrap releases handler/image and has zero recognized content");
    OmniFrameRecognition old = recognizeFixture(@"FRAMEZX729A");
    require(old.baselineCertain && !old.expectedSeen, "actual old-state glyph control");
    OmniFrameRecognition expected = recognizeFixture(@"FRAMEZX729B");
    require(expected.expectedCertain && !expected.baselineSeen, "actual expected glyph control");
    OmniFrameRecognition prefix = recognizeFixture(@"XFRAMEZX729B");
    require(!prefix.expectedSeen, "actual longer-prefix glyph control");
    OmniFrameRecognition near = recognizeFixture(@"FRAMEZX7298");
    require(!near.expectedSeen, "actual near-match glyph control");

    /* Repeat blinded OLD/NEW/prefix/near controls to expose retained per-frame graphs accumulating across jobs. */
    NSArray<NSString *> *controls = @[@"FRAMEZX729A", @"FRAMEZX729B", @"XFRAMEZX729B", @"FRAMEZX7298", @"FRAMEZX729BB", @"UNRELATED729", @"framezx729b", @"LEFTEDGE729                            FRAMEZX729B"];
    for (unsigned index = 4; index < 32; ++index) {
      OmniFrameRecognition observed = recognizeFixture(controls[index % controls.count]);
      if (index % controls.count == 0) {
        /* Every old-state repetition is positive evidence, rather than an assumed empty OCR result. */
        require(observed.baselineCertain && !observed.expectedSeen, "repeated old-state control");
      } else if (index % controls.count == 1) {
        /* The new-state control must remain recognizable after repeated independent request teardown. */
        require(observed.expectedCertain && !observed.baselineSeen, "repeated new-state control");
      } else {
        /* Longer and near tokens cannot become an exact expected token through accumulated recognizer state. */
        require(!observed.expectedSeen, "repeated prefix/near control");
      }
    }
    for (OmniFrameRecognitionLifetime *lifetime in lifetimes) {
      /* Weak-only census cannot hide an accumulating request/result/image graph by retaining it itself. */
      require(lifetime.handler == nil && lifetime.request == nil && lifetime.results == nil, "32-frame weak census has no actual-frame graph");
    }
    require(bootstrapLifetime.handler == nil && bootstrapLifetime.results == nil, "bootstrap remains content/image free after batch");
    /* Exercise the production scalar sequence using actual OLD/NEW/near recognition results acquired above. */
    OmniFrameSemanticState state = {.startTicks = 100, .requiredBaselineFrames = 2};
    require(OmniFrameAdvance(&state, old, 101) == OmniFrameSemanticWaiting, "first positive baseline frame");
    require(OmniFrameAdvance(&state, old, 102) == OmniFrameSemanticBaselineReady, "consecutive positive baseline ready");
    require(OmniFrameAdvance(&state, near, 103) == OmniFrameSemanticWaiting, "near match cannot complete paint observation");
    require(OmniFrameAdvance(&state, expected, 104) == OmniFrameSemanticMatched, "later exact token completes observed transition");
    OmniFrameSemanticState preexisting = {.startTicks = 100, .requiredBaselineFrames = 1};
    require(OmniFrameAdvance(&preexisting, expected, 101) == OmniFrameSemanticPreexisting, "preexisting expected token rejected");
    OmniFrameSemanticState stale = {.startTicks = 100, .requiredBaselineFrames = 1};
    require(OmniFrameAdvance(&stale, old, 101) == OmniFrameSemanticBaselineReady, "fresh old-state control");
    require(OmniFrameAdvance(&stale, expected, 101) == OmniFrameSemanticStale, "duplicate displayed frame cannot count");
    /* Empty/uncertain OCR cannot manufacture the mandatory positive OLD baseline. */
    OmniFrameRecognition silence = {.succeeded = YES, .nativeObjectsReleased = YES};
    OmniFrameSemanticState interrupted = {.startTicks = 100, .requiredBaselineFrames = 2};
    require(OmniFrameAdvance(&interrupted, old, 101) == OmniFrameSemanticWaiting, "partial positive baseline");
    require(OmniFrameAdvance(&interrupted, silence, 102) == OmniFrameSemanticWaiting && interrupted.baselineFrames == 0,
        "OCR silence resets consecutive baseline");
    require(OmniFrameAdvance(&interrupted, old, 103) == OmniFrameSemanticWaiting, "one OLD after silence is insufficient");
    require(OmniFrameAdvance(&interrupted, old, 104) == OmniFrameSemanticBaselineReady, "new consecutive baseline required");
    OmniFrameRecognition uncertain = expected;
    uncertain.expectedCertain = NO;
    require(OmniFrameAdvance(&interrupted, uncertain, 105) == OmniFrameSemanticWaiting, "uncertain NEW cannot certify paint");
    require(OmniFrameAdvance(&interrupted, expected, 106) == OmniFrameSemanticMatched, "later confident NEW remains eligible");
    OmniFrameSemanticState uncertainPreexisting = {.startTicks = 100, .requiredBaselineFrames = 1};
    require(OmniFrameAdvance(&uncertainPreexisting, uncertain, 101) == OmniFrameSemanticPreexisting,
        "even uncertain preexisting NEW rejects baseline");
    OmniFrameRecognition ambiguous = expected;
    ambiguous.baselineSeen = YES;
    OmniFrameSemanticState both = {.startTicks = 100, .requiredBaselineFrames = 1};
    require(OmniFrameAdvance(&both, ambiguous, 101) == OmniFrameSemanticInvalid, "simultaneous OLD and NEW rejected");
    OmniFrameSemanticState earlier = {.startTicks = 100, .requiredBaselineFrames = 1};
    require(OmniFrameAdvance(&earlier, old, 100) == OmniFrameSemanticStale, "pre-session frame rejected");
    OmniFrameRecognition retained = old;
    retained.nativeObjectsReleased = NO;
    OmniFrameSemanticState leaked = {.startTicks = 100, .requiredBaselineFrames = 1};
    require(OmniFrameAdvance(&leaked, retained, 101) == OmniFrameSemanticInvalid, "actual-frame retained native graph rejects baseline");
    /* Continuous observation retains predicate chronology across provisional NEW, reverted OLD and a later NEW. */
    OmniFrameSemanticState continuous = {.startTicks = 100, .requiredBaselineFrames = 1, .continuous = YES};
    require(OmniFrameAdvance(&continuous, old, 101) == OmniFrameSemanticBaselineReady, "continuous OLD baseline");
    require(OmniFrameAdvance(&continuous, expected, 102) == OmniFrameSemanticMatched, "continuous first NEW predicate");
    require(OmniFrameAdvance(&continuous, old, 103) == OmniFrameSemanticWaiting, "continuous reverted OLD remains observable");
    require(OmniFrameAdvance(&continuous, expected, 104) == OmniFrameSemanticMatched, "continuous later NEW predicate preserved");
    require(continuous.matched && !continuous.failed, "continuous observation does not stop at provisional NEW");
    require(OmniFrameOwnedCropBytes() == 0, "no retained crop after all fixtures");
    /* Report timing/count/resource facts only; fixture text and image bytes never enter output. */
    double maximumMilliseconds = 0;
    for (unsigned index = 0; index < fixtureCount; ++index) {
      if (fixtureMilliseconds[index] > maximumMilliseconds) {
        /* The maximum is fixture OCR work, not actual application input or observed-frame latency. */
        maximumMilliseconds = fixtureMilliseconds[index];
      }
    }
    printf("{\"headless_checks\":%u,\"vision_fixtures\":%u,\"bootstrap_requests_retained\":%u,\"actual_frame_graphs_retained\":0,\"retained_crop_bytes\":0,\"max_fixture_ocr_ms\":%.3f,\"capture_invocations\":0}\n",
        checks, fixtureCount, bootstrapLifetime.request != nil ? 1U : 0U, maximumMilliseconds);
  }
  return EX_OK;
}
