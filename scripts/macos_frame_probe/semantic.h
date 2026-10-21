/* Bounded, noncapturing crop/OCR/state contracts shared by the optional observer mode and headless tests. */
#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreVideo/CoreVideo.h>
#include <stdint.h>

/** An explicit source rectangle in display-local logical points and its exact native-resolution output. */
typedef struct {
  CGRect sourcePoints;
  size_t pixelWidth;
  size_t pixelHeight;
  double pixelScale;
} OmniFrameROIPlan;

/** Weak-only census never extends native request/result/image lifetime. */
@interface OmniFrameRecognitionLifetime : NSObject
@property(nonatomic, weak) NSObject *handler;
@property(nonatomic, weak) NSObject *request;
@property(nonatomic, weak) NSArray *results;
@end

/** Recognition reports token predicates only; recognized text never escapes the per-frame autorelease pool.
 * Seen means a literal token was observed; Certain additionally meets the fixed confidence threshold.
 * nativeObjectsReleased covers handler/request/nonempty results only: the caller must separately release its crop. */
typedef struct {
  BOOL succeeded;
  BOOL expectedSeen;
  BOOL expectedCertain;
  BOOL baselineSeen;
  BOOL baselineCertain;
  BOOL nativeObjectsReleased;
  unsigned int observationCount;
} OmniFrameRecognition;

/** One serial observation requires positive OLD-token baseline evidence and strictly increasing supplied display ticks.
 * These scalars do not establish actual capture completeness, input causality, or physical display scanout. */
typedef struct {
  uint64_t startTicks;
  uint64_t lastTicks;
  unsigned int baselineFrames;
  unsigned int requiredBaselineFrames;
  BOOL baselineReady;
  BOOL matched;
  BOOL failed;
  BOOL continuous;
} OmniFrameSemanticState;

/** Public outcomes distinguish an incomplete recognition/baseline from a valid observed transition. */
typedef NS_ENUM(NSInteger, OmniFrameSemanticOutcome) {
  OmniFrameSemanticWaiting,
  OmniFrameSemanticBaselineReady,
  OmniFrameSemanticMatched,
  OmniFrameSemanticStale,
  OmniFrameSemanticPreexisting,
  OmniFrameSemanticInvalid,
};

/** Validate nonce encoding, byte bounds and complete-token scalar grammar without running Vision. */
BOOL OmniFrameTokenIsValid(NSString *token);
/** Return the complete SHA-256 identifier of a valid nonce; never return the caller's text. */
NSString *OmniFrameTokenIdentifier(NSString *token);
/** Recognize literal complete-token byte identity within at most 512 UTF-16 units. */
BOOL OmniFrameContainsToken(NSString *text, NSString *token);
/** Parse one bounded source-point rectangle; result must point to writable CGRect storage. */
BOOL OmniFrameParseROI(const char *text, CGRect *result);
/** Prove the currently supported primary-display transform; result must point to writable plan storage. */
BOOL OmniFrameMakeROIPlan(CGRect source, CGRect display, CGRect filter, double scale, OmniFrameROIPlan *result);
/** Reject output metadata that would require an inferred crop transform or resampling. */
BOOL OmniFrameGeometryMatches(OmniFrameROIPlan plan, CGRect content, double scale, double contentScale, size_t width, size_t height);
/** Acquire the sole white bootstrap image; the caller must CFRelease the returned image after recognition. */
CVPixelBufferRef OmniFrameCreateBlankCrop(void) CF_RETURNS_RETAINED;
/** Copy verified source rows into the sole independent crop; the caller must CFRelease the returned image. */
CVPixelBufferRef OmniFrameCopyOwnedCrop(CVPixelBufferRef source, CGRect pixels) CF_RETURNS_RETAINED;
/** Count explicitly owned image bytes only, excluding opaque framework/model/WindowServer allocations. */
size_t OmniFrameOwnedCropBytes(void);
/** Perform one synchronous local-pool recognition; crop stays immutable/alive until return, lifetime may be nil.
 * The caller retains ownership of the crop and must join its release with the returned native-object release predicate. */
OmniFrameRecognition OmniFrameRecognize(CVPixelBufferRef crop, NSString *expected, NSString *baseline, OmniFrameRecognitionLifetime *lifetime);
/** Validate zero-content bootstrap release; a single opaque request may remain weakly observed until process exit. */
BOOL OmniFrameBootstrapIsValid(OmniFrameRecognition recognition, OmniFrameRecognitionLifetime *lifetime);
/** Advance caller-owned serial state only when the supplied recognition/release predicates and ordered display ticks agree.
 * The caller supplies writable state and independently establishes native capture and image-release provenance. */
OmniFrameSemanticOutcome OmniFrameAdvance(OmniFrameSemanticState *state, OmniFrameRecognition recognition, uint64_t displayTicks);
