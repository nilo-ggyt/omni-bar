#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <mach/mach_time.h>
#import <math.h>
#import <unistd.h>

static const CGFloat kLauncherWidth = 760.0;
static const CGFloat kLauncherHeight = 495.0;
static const CGKeyCode kSpaceKeyCode = 49;
static const CGKeyCode kCommandKeyCode = 55;
static const CGKeyCode kShiftKeyCode = 56;
static const NSUInteger kTransitionPollLimit = 2000;

static pid_t parse_target_pid(const char *value) {
  char *end = NULL;
  long parsed = strtol(value, &end, 10);
  if (value[0] == '\0' || end == NULL || end[0] != '\0' || parsed <= 0 ||
      parsed > INT32_MAX) {
    return -1;
  }

  return (pid_t)parsed;
}

static NSUInteger parse_sample_count(const char *value) {
  char *end = NULL;
  unsigned long parsed = strtoul(value, &end, 10);
  if (value[0] == '\0' || end == NULL || end[0] != '\0' || parsed == 0 ||
      parsed > 1000) {
    return 0;
  }

  return (NSUInteger)parsed;
}

static BOOL is_launcher_window(NSDictionary *window, pid_t target_pid) {
  NSNumber *owner_pid = window[(id)kCGWindowOwnerPID];
  NSNumber *alpha = window[(id)kCGWindowAlpha];
  NSDictionary *bounds_dictionary = window[(id)kCGWindowBounds];
  CGRect bounds = CGRectZero;
  if (owner_pid.intValue != target_pid || alpha.doubleValue <= 0.0 ||
      bounds_dictionary == nil ||
      !CGRectMakeWithDictionaryRepresentation(
          (__bridge CFDictionaryRef)bounds_dictionary, &bounds)) {
    return NO;
  }

  return fabs(CGRectGetWidth(bounds) - kLauncherWidth) < 0.5 &&
         fabs(CGRectGetHeight(bounds) - kLauncherHeight) < 0.5;
}

static NSUInteger visible_launcher_count(pid_t target_pid) {
  CFArrayRef copied_windows = CGWindowListCopyWindowInfo(
      kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
      kCGNullWindowID);
  if (copied_windows == NULL) {
    return NSNotFound;
  }

  NSArray<NSDictionary *> *windows = CFBridgingRelease(copied_windows);
  NSUInteger count = 0;
  for (NSDictionary *window in windows) {
    if (is_launcher_window(window, target_pid)) {
      count += 1;
    }
  }
  return count;
}

static void post_key_with_flags(CGKeyCode key_code, BOOL is_down,
                                CGEventFlags flags) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, is_down);
  CGEventSetFlags(event, flags);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
}

static void post_launcher_shortcut(BOOL includes_shift) {
  const CGEventFlags command = kCGEventFlagMaskCommand;
  const CGEventFlags command_shift =
      kCGEventFlagMaskCommand | kCGEventFlagMaskShift;

  post_key_with_flags(kCommandKeyCode, YES, command);
  usleep(5000);
  if (includes_shift) {
    post_key_with_flags(kShiftKeyCode, YES, command_shift);
    usleep(5000);
  }
  post_key_with_flags(kSpaceKeyCode, YES,
                      includes_shift ? command_shift : command);
  usleep(12000);
  post_key_with_flags(kSpaceKeyCode, NO,
                      includes_shift ? command_shift : command);
  usleep(5000);
  if (includes_shift) {
    post_key_with_flags(kShiftKeyCode, NO, command);
    usleep(5000);
  }
  post_key_with_flags(kCommandKeyCode, NO, 0);
}

static BOOL wait_for_launcher_count(pid_t target_pid, NSUInteger expected_count,
                                    NSUInteger *maximum_count) {
  for (NSUInteger poll = 0; poll < kTransitionPollLimit; poll += 1) {
    NSUInteger count = visible_launcher_count(target_pid);
    if (count == NSNotFound) {
      return NO;
    }
    if (count > *maximum_count) {
      *maximum_count = count;
    }
    if (count == expected_count) {
      return YES;
    }
    usleep(1000);
  }
  return NO;
}

static double elapsed_milliseconds(uint64_t start, uint64_t end) {
  mach_timebase_info_data_t timebase;
  mach_timebase_info(&timebase);
  double elapsed_ticks = (double)(end - start);
  double elapsed_nanoseconds =
      elapsed_ticks * (double)timebase.numer / (double)timebase.denom;
  return elapsed_nanoseconds / 1000000.0;
}

static int compare_doubles(const void *left, const void *right) {
  double left_value = *(const double *)left;
  double right_value = *(const double *)right;
  if (left_value < right_value) {
    return -1;
  }
  if (left_value > right_value) {
    return 1;
  }
  return 0;
}

static double nearest_rank(const double *sorted_values, NSUInteger count,
                           double percentile) {
  NSUInteger rank = (NSUInteger)ceil(percentile * (double)count);
  NSUInteger index = rank == 0 ? 0 : rank - 1;
  return sorted_values[index < count ? index : count - 1];
}

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc < 3 || argc > 5) {
      fprintf(stderr,
              "usage: %s <host-pid> <sample-count> [--shift] "
              "[--leave-open]\n",
              argv[0]);
      return 64;
    }
    if (!CGPreflightPostEventAccess()) {
      fprintf(stderr, "process does not have permission to post input events\n");
      return 77;
    }

    pid_t target_pid = parse_target_pid(argv[1]);
    NSUInteger sample_count = parse_sample_count(argv[2]);
    BOOL includes_shift = NO;
    BOOL leave_open = NO;
    for (int argument = 3; argument < argc; argument += 1) {
      if (strcmp(argv[argument], "--shift") == 0) {
        includes_shift = YES;
      } else if (strcmp(argv[argument], "--leave-open") == 0) {
        leave_open = YES;
      } else {
        fprintf(stderr, "unknown option: %s\n", argv[argument]);
        return 64;
      }
    }
    if (target_pid < 0 || sample_count == 0) {
      fprintf(stderr,
              "host pid must be positive and sample count must be in [1, 1000]\n");
      return 64;
    }

    NSUInteger maximum_count = visible_launcher_count(target_pid);
    if (maximum_count == NSNotFound) {
      fprintf(stderr, "CoreGraphics did not return a window list\n");
      return 1;
    }
    if (maximum_count > 1) {
      fprintf(stderr, "initial visible launcher count is %lu\n",
              (unsigned long)maximum_count);
      return 1;
    }
    if (maximum_count == 1) {
      post_launcher_shortcut(includes_shift);
      if (!wait_for_launcher_count(target_pid, 0, &maximum_count)) {
        fprintf(stderr, "could not establish hidden initial state\n");
        return 1;
      }
      usleep(100000);
    }

    double *latencies = calloc(sample_count, sizeof(*latencies));
    if (latencies == NULL) {
      fprintf(stderr, "could not allocate latency samples\n");
      return 1;
    }

    for (NSUInteger sample = 0; sample < sample_count; sample += 1) {
      uint64_t start = mach_continuous_time();
      post_launcher_shortcut(includes_shift);
      if (!wait_for_launcher_count(target_pid, 1, &maximum_count)) {
        fprintf(stderr, "sample %lu launcher summon timed out\n",
                (unsigned long)(sample + 1));
        free(latencies);
        return 1;
      }
      uint64_t end = mach_continuous_time();
      latencies[sample] = elapsed_milliseconds(start, end);
      printf("sample=%lu summon_ms=%.3f\n", (unsigned long)(sample + 1),
             latencies[sample]);

      BOOL is_final_sample = sample + 1 == sample_count;
      if (!(leave_open && is_final_sample)) {
        post_launcher_shortcut(includes_shift);
        if (!wait_for_launcher_count(target_pid, 0, &maximum_count)) {
          fprintf(stderr, "sample %lu launcher dismissal timed out\n",
                  (unsigned long)(sample + 1));
          free(latencies);
          return 1;
        }
        usleep(100000);
      }
    }

    qsort(latencies, sample_count, sizeof(*latencies), compare_doubles);
    printf("p50_ms=%.3f\n", nearest_rank(latencies, sample_count, 0.50));
    printf("p95_ms=%.3f\n", nearest_rank(latencies, sample_count, 0.95));
    printf("p99_ms=%.3f\n", nearest_rank(latencies, sample_count, 0.99));
    printf("maximum_visible_launchers=%lu\n", (unsigned long)maximum_count);
    printf("final_launcher_visible=%s\n", leave_open ? "true" : "false");
    free(latencies);
  }

  return 0;
}
