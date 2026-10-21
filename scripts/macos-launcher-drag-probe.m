#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <math.h>
#import <unistd.h>

/* This diagnostic recognizes one fixed historical launcher geometry, not every resizable product surface. */
static const CGFloat kLauncherWidth = 760.0;
static const CGFloat kLauncherHeight = 495.0;

/* Match selected PID and visible bounds before using geometry as the synthetic drag target. */
static BOOL is_launcher_window(NSDictionary *window, pid_t target_pid,
                               CGRect *bounds) {
  NSNumber *owner_pid = window[(id)kCGWindowOwnerPID];
  NSNumber *alpha = window[(id)kCGWindowAlpha];
  NSDictionary *bounds_dictionary = window[(id)kCGWindowBounds];
  if (owner_pid.intValue != target_pid || alpha.doubleValue <= 0.0 ||
      bounds_dictionary == nil ||
      !CGRectMakeWithDictionaryRepresentation(
          (__bridge CFDictionaryRef)bounds_dictionary, bounds)) {
    /* Missing ownership/visibility/geometry is not an eligible target and supplies no fallback coordinates. */
    return NO;
  }

  return fabs(CGRectGetWidth(*bounds) - kLauncherWidth) < 0.5 &&
         fabs(CGRectGetHeight(*bounds) - kLauncherHeight) < 0.5;
}

/* Reject incomplete or nonpositive PID spelling before any window enumeration or input posting. */
static pid_t parse_target_pid(const char *value) {
  char *end = NULL;
  long parsed = strtol(value, &end, 10);
  if (value[0] == '\0' || end == NULL || end[0] != '\0' || parsed <= 0 ||
      parsed > INT32_MAX) {
    return -1;
  }

  return (pid_t)parsed;
}

/* Transfer the copied Core Graphics inventory into ARC and borrow its dictionaries only during this lookup. */
static BOOL launcher_bounds(pid_t target_pid, CGRect *bounds) {
  CFArrayRef copied_windows = CGWindowListCopyWindowInfo(
      kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
      kCGNullWindowID);
  if (copied_windows == NULL) {
    /* Enumeration failure cannot establish either window absence or valid drag coordinates. */
    return NO;
  }

  NSArray<NSDictionary *> *windows = CFBridgingRelease(copied_windows);
  for (NSDictionary *window in windows) {
    /* The first PID/geometry match selects the fixed diagnostic target; no window title/body is read. */
    if (is_launcher_window(window, target_pid, bounds)) {
      return YES;
    }
  }

  return NO;
}

/* Post one synthetic mouse event and release its Core Graphics owner immediately after submission. */
static void post_mouse_event(CGEventSourceRef source, CGEventType type,
                             CGPoint point) {
  CGEventRef event = CGEventCreateMouseEvent(source, type, point,
                                              kCGMouseButtonLeft);
  CGEventSetIntegerValueField(event, kCGMouseEventClickState, 1);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
}

/* Observe a finite synthetic drag; success establishes changed window origin only, not complete pointer/UI acceptance. */
int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc != 2) {
      /* Wrong argument shape cannot select a process or submit input. */
      fprintf(stderr, "usage: %s <host-pid>\n", argv[0]);
      return 64;
    }

    pid_t target_pid = parse_target_pid(argv[1]);
    if (target_pid < 0) {
      /* Invalid PID spelling ends before native lookup. */
      fprintf(stderr, "host pid must be a positive integer\n");
      return 64;
    }

    CGRect before = CGRectZero;
    if (!launcher_bounds(target_pid, &before)) {
      /* Do not inject at guessed desktop coordinates when the target is unavailable. */
      fprintf(stderr, "no on-screen launcher window for pid %d\n", target_pid);
      return 1;
    }

    if (!CGPreflightPostEventAccess()) {
      /* Require existing input-posting permission without prompting or changing system settings. */
      fprintf(stderr, "process does not have permission to post input events\n");
      return 77;
    }

    /* Start inside the fixed bottom drag region and keep the finite path relative to its observed window. */
    CGPoint start = CGPointMake(CGRectGetMidX(before), CGRectGetMaxY(before) - 18);
    CGPoint destination = CGPointMake(start.x + 120, start.y - 80);
    CGEventSourceRef source =
        CGEventSourceCreate(kCGEventSourceStateCombinedSessionState);
    post_mouse_event(source, kCGEventMouseMoved, start);
    usleep(100000);
    post_mouse_event(source, kCGEventLeftMouseDown, start);
    usleep(250000);

    for (int step = 1; step <= 16; step += 1) {
      /* Sixteen bounded interpolation steps avoid retaining a mouse-event history or an unbounded playback queue. */
      CGFloat progress = (CGFloat)step / 16.0;
      CGPoint point = CGPointMake(
          start.x + (destination.x - start.x) * progress,
          start.y + (destination.y - start.y) * progress);
      post_mouse_event(source, kCGEventLeftMouseDragged, point);
      usleep(30000);
    }

    /* Balance the synthetic press and source owner before observing final geometry. */
    post_mouse_event(source, kCGEventLeftMouseUp, destination);
    CFRelease(source);
    usleep(500000);

    CGRect after = CGRectZero;
    if (!launcher_bounds(target_pid, &after)) {
      /* A disappeared or differently sized target is failure, not evidence that the drag moved it successfully. */
      fprintf(stderr, "launcher window disappeared during drag\n");
      return 1;
    }

    /* Report scalar origin changes only; timing sleeps do not certify hardware input or rendering completion. */
    BOOL moved = !CGPointEqualToPoint(before.origin, after.origin);
    printf("before_x=%.0f before_y=%.0f after_x=%.0f after_y=%.0f moved=%s\n",
           before.origin.x, before.origin.y, after.origin.x, after.origin.y,
           moved ? "true" : "false");
    return moved ? 0 : 1;
  }
}
