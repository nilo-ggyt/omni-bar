/* Public-API bounded semantic recognition; this file never captures, presents UI, writes images, or opens a network connection. */
#import "semantic.h"
#import <Vision/Vision.h>
#include <CommonCrypto/CommonDigest.h>
#include <errno.h>
#include <math.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

@implementation OmniFrameRecognitionLifetime
@end

/* The process admits one independent crop; Vision receives that crop rather than the capture sample. */
static _Atomic size_t ownedCropBytes = 0;
static const size_t maximumCropWidth = 512;
static const size_t maximumCropHeight = 128;

/** Define complete-token membership explicitly, including combining marks that cannot split a near match. */
static BOOL tokenCharacter(UTF32Char scalar) {
  return [[NSCharacterSet alphanumericCharacterSet] longCharacterIsMember:scalar] ||
         [[NSCharacterSet nonBaseCharacterSet] longCharacterIsMember:scalar] || scalar == '_' || scalar == '-';
}

/** Decode one NSString scalar without treating a surrogate half as a token boundary. */
static UTF32Char scalarAt(NSString *text, NSUInteger *offset) {
  unichar first = [text characterAtIndex:*offset];
  *offset += 1;
  if (CFStringIsSurrogateHighCharacter(first) && *offset < text.length) {
    /* A well-formed pair occupies one membership decision; malformed halves cannot equal a valid nonce. */
    unichar second = [text characterAtIndex:*offset];
    if (CFStringIsSurrogateLowCharacter(second)) {
      /* Consume both UTF-16 units only when they form a real supplementary scalar. */
      *offset += 1;
      return CFStringGetLongCharacterForSurrogatePair(first, second);
    }
  }
  return first;
}

/** Accept a bounded, nonempty synthetic UTF-8 token without whitespace, controls, or lossy repair. */
BOOL OmniFrameTokenIsValid(NSString *token) {
  NSData *bytes = [token dataUsingEncoding:NSUTF8StringEncoding allowLossyConversion:NO];
  if (token == nil || token.length > 64 || bytes.length < 8 || bytes.length > 64) {
    /* Bounded input also bounds hashing and every per-observation comparison. */
    return NO;
  }
  NSUInteger offset = 0;
  BOOL hasLetterOrNumber = NO;
  while (offset < token.length) {
    /* Require letters/numbers/marks/connectors as one complete token, never line/control separators. */
    UTF32Char scalar = scalarAt(token, &offset);
    if (!tokenCharacter(scalar)) {
      /* A delimiter/control invalidates the complete nonce rather than truncating it into an accepted prefix. */
      return NO;
    }
    /* Foundation alphanumeric is L/M/N, so explicitly exclude M when requiring a real letter/number. */
    hasLetterOrNumber |= [[NSCharacterSet alphanumericCharacterSet] longCharacterIsMember:scalar] &&
        ![[NSCharacterSet nonBaseCharacterSet] longCharacterIsMember:scalar];
  }
  /* Marks/connectors alone are not a useful positively recognized synthetic word. */
  return hasLetterOrNumber;
}

/** Publish a fixed digest identifier rather than the caller's token text or OCR output. */
NSString *OmniFrameTokenIdentifier(NSString *token) {
  NSData *bytes = [token dataUsingEncoding:NSUTF8StringEncoding allowLossyConversion:NO];
  if (!OmniFrameTokenIsValid(token)) {
    /* Invalid caller text must not enter a diagnostic identifier path through replacement encoding. */
    return nil;
  }
  unsigned char digest[CC_SHA256_DIGEST_LENGTH] = {0};
  CC_SHA256(bytes.bytes, (CC_LONG)bytes.length, digest);
  char encoded[CC_SHA256_DIGEST_LENGTH * 2 + 1] = {0};
  static const char hexadecimal[] = "0123456789abcdef";
  for (size_t index = 0; index < CC_SHA256_DIGEST_LENGTH; ++index) {
    /* Encode the entire digest so output remains fixed-size independently from UTF-8 spelling. */
    encoded[index * 2] = hexadecimal[digest[index] >> 4];
    encoded[index * 2 + 1] = hexadecimal[digest[index] & 15];
  }
  return [NSString stringWithUTF8String:encoded];
}

/** Match literal UTF-8 bytes of a complete recognized token, excluding prefix/suffix/normalization repairs. */
BOOL OmniFrameContainsToken(NSString *text, NSString *token) {
  if (text.length > 512 || !OmniFrameTokenIsValid(token)) {
    /* OCR strings are bounded before substring/data construction; callers treat oversized observations as errors. */
    return NO;
  }
  NSData *expected = [token dataUsingEncoding:NSUTF8StringEncoding allowLossyConversion:NO];
  NSUInteger offset = 0;
  while (offset < text.length) {
    /* Delimiters separate tokens, but combining marks/hyphens/underscores cannot hide an attached near match. */
    NSUInteger start = offset;
    if (!tokenCharacter(scalarAt(text, &offset))) {
      /* Delimiters advance the bounded scan without becoming part of a candidate token. */
      continue;
    }
    while (offset < text.length) {
      /* Look ahead one scalar and leave the first delimiter for the following outer iteration. */
      NSUInteger next = offset;
      if (!tokenCharacter(scalarAt(text, &next))) {
        /* End this candidate before the delimiter so attached marks/connectors remain part of the comparison. */
        break;
      }
      offset = next;
    }
    if (offset - start <= 64) {
      /* Exact byte identity rejects confusable, case, normalization, prefix and suffix substitutions. */
      NSData *candidate = [[text substringWithRange:NSMakeRange(start, offset - start)] dataUsingEncoding:NSUTF8StringEncoding allowLossyConversion:NO];
      if ([candidate isEqualToData:expected]) {
        /* Only the entire literal candidate authorizes a match; no partial substring can reach this return. */
        return YES;
      }
    }
  }
  return NO;
}

/** Parse four finite comma-separated source-point numbers without accepting missing/trailing components. */
BOOL OmniFrameParseROI(const char *text, CGRect *result) {
  if (text == NULL || strlen(text) > 128) {
    /* Parse storage/work stays independent from a caller's arbitrary command-line length. */
    return NO;
  }
  double values[4] = {0};
  const char *cursor = text;
  for (size_t index = 0; index < 4; ++index) {
    /* Require ordinary nonnegative decimal spelling; hexadecimal, signs and whitespace are not repaired. */
    if (*cursor < '0' || *cursor > '9') {
      return NO;
    }
    errno = 0;
    char *end = NULL;
    values[index] = strtod(cursor, &end);
    if (errno != 0 || end == cursor || !isfinite(values[index]) || values[index] > 16384.0 ||
        (index < 3 ? *end != ',' : *end != '\0')) {
      /* Every future conversion/crop coordinate has a finite source bound before native acquisition. */
      return NO;
    }
    for (const char *digit = cursor; digit < end; ++digit) {
      /* Disallow exponents/hex grammar so reviewed ROI coordinates have one unambiguous decimal representation. */
      if ((*digit < '0' || *digit > '9') && *digit != '.') {
        return NO;
      }
    }
    cursor = end + (index < 3 ? 1 : 0);
  }
  if (values[2] <= 0.0 || values[3] <= 0.0) {
    /* A zero-area crop cannot establish visible token absence or presence. */
    return NO;
  }
  *result = CGRectMake(values[0], values[1], values[2], values[3]);
  return YES;
}

/** Reject nonfinite/negative rectangle components before CoreGraphics containment or integer conversion. */
static BOOL validRectangle(CGRect rectangle) {
  return isfinite(rectangle.origin.x) && isfinite(rectangle.origin.y) &&
         isfinite(rectangle.size.width) && isfinite(rectangle.size.height) &&
         rectangle.origin.x >= 0.0 && rectangle.origin.y >= 0.0 &&
         rectangle.size.width > 0.0 && rectangle.size.height > 0.0;
}

/** Admit one unambiguous primary-display transform, retaining native glyph resolution within the fixed crop budget. */
BOOL OmniFrameMakeROIPlan(CGRect source, CGRect display, CGRect filter, double scale, OmniFrameROIPlan *result) {
  if (!validRectangle(source) || !validRectangle(display) || !validRectangle(filter) ||
      display.origin.x != 0.0 || display.origin.y != 0.0 || !CGRectEqualToRect(display, filter) ||
      !CGRectContainsRect(display, source) || !isfinite(scale) || scale < 1.0 || scale > 4.0) {
    /* Different origins/partial filter content are not guessed into a global/local coordinate conversion. */
    return NO;
  }
  double width = source.size.width * scale;
  double height = source.size.height * scale;
  if (!isfinite(width) || !isfinite(height) || width < 1.0 || height < 1.0 ||
      width > (double)maximumCropWidth || height > (double)maximumCropHeight ||
      width != floor(width) || height != floor(height) ||
      source.origin.x * scale != floor(source.origin.x * scale) ||
      source.origin.y * scale != floor(source.origin.y * scale)) {
    /* No resampling or subpixel rounding is silently introduced into a purported glyph match. */
    return NO;
  }
  *result = (OmniFrameROIPlan){source, (size_t)width, (size_t)height, scale};
  return YES;
}

/** Verify each cropped output's declared content geometry before exposing any pixel row to OCR. */
BOOL OmniFrameGeometryMatches(OmniFrameROIPlan plan, CGRect content, double scale, double contentScale, size_t width, size_t height) {
  return validRectangle(content) && width == plan.pixelWidth && height == plan.pixelHeight &&
         isfinite(scale) && scale == plan.pixelScale && isfinite(contentScale) && contentScale == 1.0 &&
         CGRectEqualToRect(content, CGRectMake(0, 0, plan.sourcePoints.size.width, plan.sourcePoints.size.height));
}

/** Release the sole independent crop's allocation and lease only when its last public CF owner releases it. */
static void releaseCrop(void *context, const void *baseAddress) {
  (void)context;
  free((void *)baseAddress);
  atomic_store(&ownedCropBytes, 0);
}

/** Prepare a content-free native-resolution image using the same sole crop lease as every captured frame. */
CVPixelBufferRef OmniFrameCreateBlankCrop(void) {
  size_t count = maximumCropWidth * maximumCropHeight * 4;
  size_t unowned = 0;
  if (!atomic_compare_exchange_strong(&ownedCropBytes, &unowned, count)) {
    /* Preparation is serial with capture recognition and cannot allocate alongside an existing image. */
    return NULL;
  }
  void *bytes = malloc(count);
  if (bytes == NULL) {
    /* Failed acquisition publishes no image and returns the one owned-image lease. */
    atomic_store(&ownedCropBytes, 0);
    return NULL;
  }
  memset(bytes, 255, count);
  CVPixelBufferRef image = NULL;
  CVReturn created = CVPixelBufferCreateWithBytes(kCFAllocatorDefault, maximumCropWidth, maximumCropHeight,
      kCVPixelFormatType_32BGRA, bytes, maximumCropWidth * 4, releaseCrop, NULL, NULL, &image);
  if (created != kCVReturnSuccess || image == NULL) {
    /* The callback owns bytes only after successful publication, so this failure releases them directly. */
    free(bytes);
    atomic_store(&ownedCropBytes, 0);
    return NULL;
  }
  return image;
}

/** Copy only finite BGRA ROI rows; the returned allocation has no provider/parent-frame ownership link. */
CVPixelBufferRef OmniFrameCopyOwnedCrop(CVPixelBufferRef source, CGRect pixels) {
  if (source == NULL || CVPixelBufferGetPixelFormatType(source) != kCVPixelFormatType_32BGRA ||
      CVPixelBufferIsPlanar(source) || !validRectangle(pixels) || pixels.size.width > (double)maximumCropWidth ||
      pixels.size.height > (double)maximumCropHeight || pixels.origin.x != floor(pixels.origin.x) ||
      pixels.origin.y != floor(pixels.origin.y) || pixels.size.width != floor(pixels.size.width) ||
      pixels.size.height != floor(pixels.size.height)) {
    /* Reject malformed layouts before integer conversion, lock acquisition or allocation. */
    return NULL;
  }
  size_t sourceWidth = CVPixelBufferGetWidth(source), sourceHeight = CVPixelBufferGetHeight(source);
  if (pixels.origin.x > (double)sourceWidth || pixels.origin.y > (double)sourceHeight ||
      pixels.size.width > (double)sourceWidth - pixels.origin.x || pixels.size.height > (double)sourceHeight - pixels.origin.y) {
    /* Subtraction-based bounds avoid overflow while retaining exact source extent checks. */
    return NULL;
  }
  size_t width = (size_t)pixels.size.width, height = (size_t)pixels.size.height;
  size_t count = width * height * 4;
  size_t unowned = 0;
  if (!atomic_compare_exchange_strong(&ownedCropBytes, &unowned, count)) {
    /* A second in-flight crop is rejected rather than growing an image queue. */
    return NULL;
  }
  unsigned char *copy = malloc(count);
  if (copy == NULL || CVPixelBufferLockBaseAddress(source, kCVPixelBufferLock_ReadOnly) != kCVReturnSuccess) {
    /* Failed storage/lock acquisition relinquishes only this call's owned lease and bytes. */
    free(copy);
    atomic_store(&ownedCropBytes, 0);
    return NULL;
  }
  const unsigned char *base = CVPixelBufferGetBaseAddress(source);
  size_t stride = CVPixelBufferGetBytesPerRow(source);
  if (base == NULL || sourceWidth > SIZE_MAX / 4 || stride < sourceWidth * 4 ||
      (sourceHeight > 0 && stride > SIZE_MAX / sourceHeight)) {
    /* Validate native layout arithmetic before forming any row pointer. */
    CVPixelBufferUnlockBaseAddress(source, kCVPixelBufferLock_ReadOnly);
    free(copy);
    atomic_store(&ownedCropBytes, 0);
    return NULL;
  }
  for (size_t row = 0; row < height; ++row) {
    /* The earlier source/crop/stride checks make every read and write lie within its owned extent. */
    memcpy(copy + row * width * 4, base + ((size_t)pixels.origin.y + row) * stride + (size_t)pixels.origin.x * 4, width * 4);
  }
  CVPixelBufferUnlockBaseAddress(source, kCVPixelBufferLock_ReadOnly);
  CVPixelBufferRef crop = NULL;
  CVReturn created = CVPixelBufferCreateWithBytes(kCFAllocatorDefault, width, height, kCVPixelFormatType_32BGRA,
      copy, width * 4, releaseCrop, NULL, NULL, &crop);
  if (created != kCVReturnSuccess || crop == NULL) {
    /* Ownership transfers to the release callback only after successful CVPixelBuffer publication. */
    free(copy);
    atomic_store(&ownedCropBytes, 0);
    return NULL;
  }
  return crop;
}

/** Report actual independently retained image bytes without estimating opaque Vision/WindowServer allocations. */
size_t OmniFrameOwnedCropBytes(void) {
  return atomic_load(&ownedCropBytes);
}

/** Recognize one owned immutable crop with per-frame request/handler/result lifetime and no textual output. */
OmniFrameRecognition OmniFrameRecognize(CVPixelBufferRef crop, NSString *expected, NSString *baseline, OmniFrameRecognitionLifetime *lifetime) {
  OmniFrameRecognition result = {0};
  __weak VNImageRequestHandler *handlerWeak;
  __weak VNRecognizeTextRequest *requestWeak;
  __weak NSArray *resultsWeak;
  @autoreleasepool {
    if (crop == NULL || CVPixelBufferGetPixelFormatType(crop) != kCVPixelFormatType_32BGRA ||
        CVPixelBufferGetWidth(crop) == 0 || CVPixelBufferGetHeight(crop) == 0 ||
        CVPixelBufferGetWidth(crop) > maximumCropWidth || CVPixelBufferGetHeight(crop) > maximumCropHeight ||
        !OmniFrameTokenIsValid(expected) || !OmniFrameTokenIsValid(baseline) || [expected isEqualToString:baseline] ||
        ![VNRecognizeTextRequest.supportedRevisions containsIndex:VNRecognizeTextRequestRevision3]) {
      /* Invalid/unavailable recognition cannot be interpreted as an absent expected token. */
      return result;
    }
    VNRecognizeTextRequest *request = [VNRecognizeTextRequest new];
    request.revision = VNRecognizeTextRequestRevision3;
    request.recognitionLevel = VNRequestTextRecognitionLevelAccurate;
    request.recognitionLanguages = @[@"en-US"];
    request.usesLanguageCorrection = NO;
    request.automaticallyDetectsLanguage = NO;
    request.regionOfInterest = CGRectMake(0, 0, 1, 1);
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithCVPixelBuffer:crop orientation:kCGImagePropertyOrientationUp options:@{}];
    handlerWeak = handler;
    requestWeak = request;
    lifetime.handler = handler;
    lifetime.request = request;
    NSError *error = nil;
    if ([handler performRequests:@[request] error:&error] && request.results.count <= 64) {
      /* Only completed, bounded observations can establish either positive baseline or expected-token evidence. */
      result.succeeded = YES;
      result.observationCount = (unsigned int)request.results.count;
      NSArray<VNRecognizedTextObservation *> *observations = request.results;
      if (observations.count > 0) {
        /* An immutable empty-array singleton owns no per-frame text/image graph and need not deallocate. */
        resultsWeak = observations;
        lifetime.results = observations;
      }
      for (VNRecognizedTextObservation *observation in observations) {
        /* Read only the best complete-token candidate; no arbitrary recognized strings escape this scope. */
        VNRecognizedText *candidate = [observation topCandidates:1].firstObject;
        if (candidate == nil || candidate.string.length > 512 || !isfinite(candidate.confidence)) {
          /* Oversized/invalid observations are errors rather than an invented empty OCR result. */
          result.succeeded = NO;
          break;
        }
        BOOL expectedFound = OmniFrameContainsToken(candidate.string, expected);
        BOOL baselineFound = OmniFrameContainsToken(candidate.string, baseline);
        result.expectedSeen |= expectedFound;
        result.baselineSeen |= baselineFound;
        result.expectedCertain |= expectedFound && candidate.confidence >= 0.9f;
        result.baselineCertain |= baselineFound && candidate.confidence >= 0.9f;
      }
    }
  }
  /* These observations are a strict local graph check, separate from opaque global model caches. */
  result.nativeObjectsReleased = handlerWeak == nil && requestWeak == nil && resultsWeak == nil;

  return result;
}

/** Permit at most the content-free bootstrap request retained internally by Vision, never its image or results. */
BOOL OmniFrameBootstrapIsValid(OmniFrameRecognition recognition, OmniFrameRecognitionLifetime *lifetime) {
  return lifetime != nil && recognition.succeeded && recognition.observationCount == 0 && !recognition.expectedSeen && !recognition.baselineSeen &&
      !recognition.expectedCertain && !recognition.baselineCertain && lifetime.handler == nil &&
      lifetime.results == nil && OmniFrameOwnedCropBytes() == 0;
}

/** Require positive old-state evidence before accepting a later exact new-state frame; never infer latency here. */
OmniFrameSemanticOutcome OmniFrameAdvance(OmniFrameSemanticState *state, OmniFrameRecognition recognition, uint64_t displayTicks) {
  if (state->failed || (state->matched && !state->continuous) || !recognition.succeeded || !recognition.nativeObjectsReleased ||
      state->requiredBaselineFrames == 0 || state->requiredBaselineFrames > 10 ||
      (recognition.expectedSeen && recognition.baselineSeen)) {
    /* Failed/one-shot-completed/ambiguous or unbounded state has no authority to certify a new transition. */
    state->failed = YES;
    return OmniFrameSemanticInvalid;
  }
  if (displayTicks <= state->startTicks || displayTicks <= state->lastTicks) {
    /* Old, repeated and out-of-order display timestamps cannot become a fresh semantic frame. */
    state->failed = YES;
    return OmniFrameSemanticStale;
  }
  state->lastTicks = displayTicks;
  if (!state->baselineReady) {
    if (recognition.expectedSeen) {
      /* Seeing the expected token before a proven old state is a preexisting-token failure, even at low confidence. */
      state->failed = YES;
      return OmniFrameSemanticPreexisting;
    }
    if (recognition.baselineCertain) {
      /* A confident explicit OLD token establishes observed expected-token absence without trusting OCR silence. */
      state->baselineFrames += 1;
      state->baselineReady = state->baselineFrames >= state->requiredBaselineFrames;
    } else {
      /* An empty/uncertain image breaks a consecutive baseline sequence rather than proving absence. */
      state->baselineFrames = 0;
    }
    return state->baselineReady ? OmniFrameSemanticBaselineReady : OmniFrameSemanticWaiting;
  }
  if (recognition.expectedCertain && !recognition.baselineSeen) {
    /* Preserve every later confident NEW predicate; continuous mode may observe provisional NEW/OLD/NEW transitions. */
    state->matched = YES;
    return OmniFrameSemanticMatched;
  }
  return OmniFrameSemanticWaiting;
}
