#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>

static pid_t parse_target_pid(const char *value) {
  char *end = NULL;
  long parsed = strtol(value, &end, 10);
  if (value[0] == '\0' || end == NULL || end[0] != '\0' || parsed <= 0 ||
      parsed > INT32_MAX) {
    return -1;
  }

  return (pid_t)parsed;
}

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc != 2) {
      fprintf(stderr, "usage: %s <host-pid>\n", argv[0]);
      return 64;
    }

    pid_t target_pid = parse_target_pid(argv[1]);
    if (target_pid < 0) {
      fprintf(stderr, "host pid must be a positive integer\n");
      return 64;
    }

    NSRunningApplication *frontmost =
        NSWorkspace.sharedWorkspace.frontmostApplication;
    BOOL is_frontmost = frontmost != nil &&
                        frontmost.processIdentifier == target_pid;
    pid_t frontmost_pid = frontmost == nil ? -1 : frontmost.processIdentifier;

    CFArrayRef copied_windows = CGWindowListCopyWindowInfo(
        kCGWindowListOptionAll | kCGWindowListExcludeDesktopElements,
        kCGNullWindowID);
    if (copied_windows == NULL) {
      fprintf(stderr, "CoreGraphics did not return a window list\n");
      return 1;
    }

    NSArray<NSDictionary *> *windows = CFBridgingRelease(copied_windows);
    NSUInteger target_window_count = 0;
    for (NSDictionary *window in windows) {
      NSNumber *owner_pid = window[(id)kCGWindowOwnerPID];
      if (owner_pid.intValue == target_pid) {
        target_window_count += 1;
      }
    }

    printf("pid=%d frontmost=%s frontmost_pid=%d windows=%lu\n", target_pid,
           is_frontmost ? "true" : "false", frontmost_pid,
           (unsigned long)target_window_count);

    NSUInteger front_to_back_index = 0;
    for (NSDictionary *window in windows) {
      NSNumber *owner_pid = window[(id)kCGWindowOwnerPID];
      if (owner_pid.intValue != target_pid) {
        front_to_back_index += 1;
        continue;
      }

      CGRect bounds = CGRectZero;
      NSDictionary *bounds_dictionary = window[(id)kCGWindowBounds];
      if (bounds_dictionary != nil) {
        CGRectMakeWithDictionaryRepresentation(
            (__bridge CFDictionaryRef)bounds_dictionary, &bounds);
      }

      NSNumber *window_number = window[(id)kCGWindowNumber];
      NSNumber *layer = window[(id)kCGWindowLayer];
      NSNumber *onscreen = window[(id)kCGWindowIsOnscreen];
      NSNumber *alpha = window[(id)kCGWindowAlpha];

      printf("index=%lu id=%d layer=%d onscreen=%s alpha=%.3f "
             "x=%.0f y=%.0f width=%.0f height=%.0f\n",
             (unsigned long)front_to_back_index, window_number.intValue,
             layer.intValue, onscreen.boolValue ? "true" : "false",
             alpha.doubleValue, bounds.origin.x, bounds.origin.y,
             bounds.size.width, bounds.size.height);
      front_to_back_index += 1;
    }
  }

  return 0;
}
