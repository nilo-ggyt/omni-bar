#import <Carbon/Carbon.h>
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>
#import <math.h>
#import <unistd.h>

/* Fixed diagnostic geometries and title markers identify synthetic surfaces only, not arbitrary product documents. */
static const CGFloat kLauncherWidth = 760.0;
static const CGFloat kProductLauncherHeight = 495.0;
static const CGFloat kNativeFloorCompactHomeHeight = 168.0;
static const CGFloat kNativeFloorInputHeight = 116.0;
static const CGFloat kMappedMarkdownWidth = 1180.0;
static const CGFloat kMappedMarkdownHeight = 760.0;
static const CGFloat kWindowGeometryTolerance = 1.5;
static const CGKeyCode kSpaceKeyCode = 49;
static const CGKeyCode kCommandKeyCode = 55;
static const CGKeyCode kShiftKeyCode = 56;
static const NSUInteger kTransitionPollLimit = 2000;
static const NSUInteger kLauncherResultRowCount = 5;
static NSString *const kLauncherCommittedInputMarker = @"Omni IME committed";
static NSString *const kMappedMarkdownCommittedInputMarker =
    @"Omni Markdown IME committed";

/* The caller selects which existing synthetic input contract to exercise; the probe does not create a window. */
typedef NS_ENUM(uint8_t, OmniInputProbeSurface) {
  OmniInputProbeSurfaceLauncher = 0,
  OmniInputProbeSurfaceMappedMarkdown = 1,
};

/* Require the selected PID and supported visible dimensions before using a window as the input target. */
static BOOL is_input_surface_window(NSDictionary *window, pid_t target_pid,
                                    OmniInputProbeSurface surface,
                                    CGRect *bounds) {
  NSNumber *owner_pid = window[(id)kCGWindowOwnerPID];
  NSNumber *alpha = window[(id)kCGWindowAlpha];
  NSDictionary *bounds_dictionary = window[(id)kCGWindowBounds];
  if (owner_pid.intValue != target_pid || alpha.doubleValue <= 0.0 ||
      bounds_dictionary == nil ||
      !CGRectMakeWithDictionaryRepresentation(
          (__bridge CFDictionaryRef)bounds_dictionary, bounds)) {
    /* Unavailable or invisible geometry cannot become fallback coordinates for desktop input. */
    return NO;
  }

  CGFloat width = CGRectGetWidth(*bounds);
  CGFloat height = CGRectGetHeight(*bounds);
  if (surface == OmniInputProbeSurfaceMappedMarkdown) {
    /* Markdown uses its own fixed reference geometry, independent of the launcher alternatives below. */
    return fabs(width - kMappedMarkdownWidth) < kWindowGeometryTolerance &&
           fabs(height - kMappedMarkdownHeight) < kWindowGeometryTolerance;
  }

  BOOL has_supported_launcher_height =
      fabs(height - kProductLauncherHeight) < kWindowGeometryTolerance ||
      fabs(height - kNativeFloorCompactHomeHeight) < kWindowGeometryTolerance ||
      fabs(height - kNativeFloorInputHeight) < kWindowGeometryTolerance;
  return fabs(width - kLauncherWidth) < kWindowGeometryTolerance &&
         has_supported_launcher_height;
}

/* Reject incomplete/nonpositive PID spelling before native window lookup; birth identity is not established by this tool. */
static pid_t parse_target_pid(const char *value) {
  char *end = NULL;
  long parsed = strtol(value, &end, 10);
  if (value[0] == '\0' || end == NULL || end[0] != '\0' || parsed <= 0 ||
      parsed > INT32_MAX) {
    return -1;
  }

  return (pid_t)parsed;
}

/* Own one copied window snapshot through ARC; optional marker matching observes only the synthetic title protocol. */
static BOOL find_input_surface_window(pid_t target_pid,
                                      OmniInputProbeSurface surface,
                                      CGRect *bounds,
                                      BOOL require_committed_input_marker) {
  CFArrayRef copied_windows = CGWindowListCopyWindowInfo(
      kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
      kCGNullWindowID);
  if (copied_windows == NULL) {
    /* This boolean lookup cannot distinguish failed enumeration from absence; callers must not infer resource release from it. */
    return NO;
  }

  NSArray<NSDictionary *> *windows = CFBridgingRelease(copied_windows);
  NSString *committed_input_marker =
      surface == OmniInputProbeSurfaceMappedMarkdown
          ? kMappedMarkdownCommittedInputMarker
          : kLauncherCommittedInputMarker;
  for (NSDictionary *window in windows) {
    /* A geometry match alone is enough for targeting; committed-input checks additionally require the exact marker. */
    if (is_input_surface_window(window, target_pid, surface, bounds) &&
        (!require_committed_input_marker ||
         [window[(id)kCGWindowName]
             isEqualToString:committed_input_marker])) {
      return YES;
    }
  }

  return NO;
}

/* Retrieve targeting coordinates without claiming that an IME transaction has committed. */
static BOOL input_surface_bounds(pid_t target_pid, OmniInputProbeSurface surface,
                                 CGRect *bounds) {
  return find_input_surface_window(target_pid, surface, bounds, NO);
}

/* Read the fixture's committed-title marker; this is not observation of candidate placement or the full document. */
static BOOL input_surface_committed_input_method_text(
    pid_t target_pid, OmniInputProbeSurface surface) {
  CGRect ignored_bounds = CGRectZero;
  return find_input_surface_window(target_pid, surface, &ignored_bounds, YES);
}

/* Return retained mode/parent identities from a finite system list; selection and restoration belong to the caller. */
static TISInputSourceRef
copy_pinyin_input_source(TISInputSourceRef *parent_input_source) {
  CFStringRef pinyin_mode = CFSTR("com.apple.inputmethod.SCIM.ITABC");
  CFStringRef pinyin_parent_identifier = CFSTR("com.apple.inputmethod.SCIM");
  *parent_input_source = NULL;
  CFArrayRef matching_sources = TISCreateInputSourceList(NULL, true);
  if (matching_sources == NULL || CFArrayGetCount(matching_sources) == 0) {
    /* Balance an empty copied list and return no invented default input mode. */
    if (matching_sources != NULL) {
      CFRelease(matching_sources);
    }
    return NULL;
  }

  TISInputSourceRef source = NULL;
  for (CFIndex index = 0; index < CFArrayGetCount(matching_sources); index += 1) {
    /* Inspect declarations only; neither enumeration nor retained identity enables or selects a source. */
    TISInputSourceRef candidate =
        (TISInputSourceRef)CFArrayGetValueAtIndex(matching_sources, index);
    CFStringRef input_source_identifier =
        TISGetInputSourceProperty(candidate, kTISPropertyInputSourceID);
    CFStringRef input_mode =
        TISGetInputSourceProperty(candidate, kTISPropertyInputModeID);
    CFBooleanRef enabled =
        TISGetInputSourceProperty(candidate, kTISPropertyInputSourceIsEnabled);
    CFBooleanRef selectable = TISGetInputSourceProperty(
        candidate, kTISPropertyInputSourceIsSelectCapable);
    if (source == NULL && input_mode != NULL && CFEqual(input_mode, pinyin_mode) &&
        enabled == kCFBooleanTrue && selectable == kCFBooleanTrue) {
      /* Keep one enabled selectable Pinyin mode beyond the copied array's lifetime. */
      source = candidate;
      CFRetain(source);
    }
    if (*parent_input_source == NULL && input_source_identifier != NULL &&
        CFEqual(input_source_identifier, pinyin_parent_identifier)) {
      /* The parent has separate retained ownership so explicit opt-in can later restore its enablement state. */
      *parent_input_source = candidate;
      CFRetain(*parent_input_source);
    }
  }
  CFRelease(matching_sources);
  return source;
}

/* Post one synthetic mouse event and release its native owner after submission. */
static void post_mouse(CGEventType type, CGPoint point) {
  CGEventRef event =
      CGEventCreateMouseEvent(NULL, type, point, kCGMouseButtonLeft);
  CGEventSetIntegerValueField(event, kCGMouseEventClickState, 1);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
}

/* Submit a raw virtual-key event; system delivery and application interpretation remain separate observations. */
static void post_key_with_flags(CGKeyCode key_code, BOOL is_down,
                                CGEventFlags flags) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, is_down);
  CGEventSetFlags(event, flags);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
}

/* Preserve the raw-key path without modifiers so the selected IME, rather than injected Unicode, interprets it. */
static void post_key(CGKeyCode key_code, BOOL is_down) {
  post_key_with_flags(key_code, is_down, 0);
}

/* Replay only the fixed historical Shift-Command-Space chord; do not register or replace the product's configured shortcut. */
static void post_launcher_shortcut(void) {
  const CGEventFlags command = kCGEventFlagMaskCommand;
  const CGEventFlags command_shift =
      kCGEventFlagMaskCommand | kCGEventFlagMaskShift;
  post_key_with_flags(kCommandKeyCode, YES, command);
  usleep(5000);
  post_key_with_flags(kShiftKeyCode, YES, command_shift);
  usleep(5000);
  post_key_with_flags(kSpaceKeyCode, YES, command_shift);
  usleep(12000);
  post_key_with_flags(kSpaceKeyCode, NO, command_shift);
  usleep(5000);
  post_key_with_flags(kShiftKeyCode, NO, command);
  usleep(5000);
  post_key_with_flags(kCommandKeyCode, NO, 0);
}

/* Click one geometry-derived synthetic input position; a posted click does not independently prove first-responder readiness. */
static void focus_input_surface(CGRect surface_bounds,
                                OmniInputProbeSurface surface) {
  CGFloat vertical_offset =
      surface == OmniInputProbeSurfaceMappedMarkdown
          ? CGRectGetHeight(surface_bounds) * 0.5
          : 38.0;
  CGPoint input_point = CGPointMake(CGRectGetMinX(surface_bounds) + 300,
                                    CGRectGetMinY(surface_bounds) +
                                        vertical_offset);
  post_mouse(kCGEventMouseMoved, input_point);
  usleep(50000);
  post_mouse(kCGEventLeftMouseDown, input_point);
  usleep(20000);
  post_mouse(kCGEventLeftMouseUp, input_point);
  usleep(250000);
}

/* Bound visibility polling; this coarse boolean lookup is not native graph-release or presented-frame evidence. */
static BOOL wait_for_input_surface_visibility(pid_t target_pid,
                                              OmniInputProbeSurface surface,
                                              BOOL visible) {
  for (NSUInteger poll = 0; poll < kTransitionPollLimit; poll += 1) {
    /* Take a fresh scalar geometry observation on each attempt, retaining no window dictionary across polls. */
    CGRect bounds = CGRectZero;
    if (input_surface_bounds(target_pid, surface, &bounds) == visible) {
      return YES;
    }
    usleep(1000);
  }
  return NO;
}

/* Map only the finite supported ASCII key set; unsupported characters never receive an invented virtual key code. */
static CGKeyCode key_code_for_character(unichar character) {
  if (character >= 'A' && character <= 'Z') {
    /* Physical key identity is case-independent; the Unicode posting path carries the caller's original character. */
    character = (unichar)(character + ('a' - 'A'));
  }
  switch (character) {
  case 'a':
    return 0;
  case 'c':
    return 8;
  case 'd':
    return 2;
  case 'e':
    return 14;
  case 'g':
    return 5;
  case 'h':
    return 4;
  case 'i':
    return 34;
  case 'l':
    return 37;
  case 'm':
    return 46;
  case 'n':
    return 45;
  case 'o':
    return 31;
  case 'p':
    return 35;
  case 'r':
    return 15;
  default:
    /* Let the caller fail explicitly instead of silently dropping unsupported input. */
    return UINT16_MAX;
  }
}

/* Attach one caller-supplied ASCII UTF-16 unit for direct text testing, distinct from the raw-key IME path. */
static void post_character(CGKeyCode key_code, unichar character,
                           BOOL is_down) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, is_down);
  CGEventKeyboardSetUnicodeString(event, 1, &character);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
}

/* Drive one selected synthetic surface. Explicit --pinyin-ime changes system input-source selection and attempts restoration. */
int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc < 3 || argc > 8) {
      /* Require explicit target/text arguments before any permission or window query. */
      fprintf(stderr,
              "usage: %s <host-pid> <ascii-text> [--enter] [--escape] "
              "[--summon-shift] [--click-row=N] [--pinyin-ime] "
              "[--markdown-surface]\n",
              argv[0]);
      return 64;
    }
    if (!CGPreflightPostEventAccess()) {
      /* Use existing posting access only, without opening a permission prompt. */
      fprintf(stderr, "process does not have permission to post input events\n");
      return 77;
    }
    pid_t target_pid = parse_target_pid(argv[1]);
    CGRect bounds = CGRectZero;
    if (target_pid < 0) {
      /* Invalid PID cannot reach window lookup or synthetic input submission. */
      fprintf(stderr, "host pid must be a positive integer\n");
      return 64;
    }

    BOOL press_enter = NO;
    BOOL press_escape = NO;
    BOOL summon_with_shift = NO;
    BOOL use_pinyin_input_method = NO;
    BOOL use_mapped_markdown_surface = NO;
    NSInteger click_row = -1;
    for (int argument = 3; argument < argc; argument += 1) {
      /* These switches select the existing finite diagnostic actions, never arbitrary native selectors or shell input. */
      if (strcmp(argv[argument], "--enter") == 0) {
        press_enter = YES;
      } else if (strcmp(argv[argument], "--escape") == 0) {
        press_escape = YES;
      } else if (strcmp(argv[argument], "--summon-shift") == 0) {
        summon_with_shift = YES;
      } else if (strcmp(argv[argument], "--pinyin-ime") == 0) {
        use_pinyin_input_method = YES;
      } else if (strcmp(argv[argument], "--markdown-surface") == 0) {
        use_mapped_markdown_surface = YES;
      } else if (strncmp(argv[argument], "--click-row=", 12) == 0) {
        /* A row target is limited to this synthetic launcher's declared result geometry. */
        char *end = NULL;
        long parsed = strtol(argv[argument] + 12, &end, 10);
        if (end == NULL || end[0] != '\0' || parsed < 0 ||
            parsed >= (long)kLauncherResultRowCount) {
          /* Reject an unsupported row before coordinates or a click can be generated. */
          fprintf(stderr, "click row must be in [0, %lu)\n",
                  (unsigned long)kLauncherResultRowCount);
          return 64;
        }
        click_row = (NSInteger)parsed;
      } else {
        /* Unknown options must not silently trigger a default action. */
        fprintf(stderr, "unknown option: %s\n", argv[argument]);
        return 64;
      }
    }

    NSString *text = [NSString stringWithUTF8String:argv[2]];
    if (text == nil || ![text canBeConvertedToEncoding:NSASCIIStringEncoding]) {
      /* This input protocol accepts ASCII keystroke spelling only, without lossy Unicode replacement. */
      fprintf(stderr, "input must be valid ASCII text\n");
      return 64;
    }
    if (use_pinyin_input_method &&
        (press_enter || press_escape || summon_with_shift || click_row >= 0)) {
      /* IME commitment must remain separate from launcher activation or dismissal actions. */
      fprintf(stderr, "pinyin IME validation cannot be combined with actions\n");
      return 64;
    }
    if (use_mapped_markdown_surface && click_row >= 0) {
      /* A launcher-row click has no defined target inside the Markdown reference surface. */
      fprintf(stderr,
              "Markdown surface validation cannot click Launcher rows\n");
      return 64;
    }

    OmniInputProbeSurface surface =
        use_mapped_markdown_surface
            ? OmniInputProbeSurfaceMappedMarkdown
            : OmniInputProbeSurfaceLauncher;

    if (summon_with_shift) {
      /* Establish a hidden baseline first when necessary, then request the fixed historical summon chord. */
      if (input_surface_bounds(target_pid, surface, &bounds)) {
        post_launcher_shortcut();
        if (!wait_for_input_surface_visibility(target_pid, surface, NO)) {
          /* Failed hidden-state observation prevents input from being sent to a guessed target. */
          fprintf(stderr, "could not establish hidden initial state\n");
          return 1;
        }
      }
      post_launcher_shortcut();
      if (!wait_for_input_surface_visibility(target_pid, surface, YES) ||
          !input_surface_bounds(target_pid, surface, &bounds)) {
        /* A visible target and fresh coordinates are both required after the summon request. */
        fprintf(stderr, "launcher summon timed out\n");
        return 1;
      }
      usleep(50000);
    } else if (!input_surface_bounds(target_pid, surface, &bounds)) {
      /* Without a summon request, the caller must already have made the intended surface visible. */
      fprintf(stderr, "no on-screen input surface for target pid\n");
      return 1;
    }

    TISInputSourceRef previous_input_source = NULL;
    TISInputSourceRef pinyin_parent_input_source = NULL;
    BOOL pinyin_parent_enabled_by_probe = NO;
    if (use_pinyin_input_method) {
      /* Retain the current mode before any explicit system source change, and separately own the Pinyin parent. */
      previous_input_source = TISCopyCurrentKeyboardInputSource();
      TISInputSourceRef pinyin_input_source =
          copy_pinyin_input_source(&pinyin_parent_input_source);
      if (previous_input_source == NULL || pinyin_input_source == NULL ||
          pinyin_parent_input_source == NULL) {
        /* Failed discovery balances every acquired CF owner before returning without selecting a mode. */
        if (previous_input_source != NULL) {
          CFRelease(previous_input_source);
        }
        if (pinyin_input_source != NULL) {
          CFRelease(pinyin_input_source);
        }
        if (pinyin_parent_input_source != NULL) {
          CFRelease(pinyin_parent_input_source);
        }
        fprintf(stderr, "required Pinyin input source is not enabled\n");
        return 1;
      }
      CFBooleanRef pinyin_parent_enabled = TISGetInputSourceProperty(
          pinyin_parent_input_source, kTISPropertyInputSourceIsEnabled);
      if (pinyin_parent_enabled != kCFBooleanTrue) {
        /* Record enablement ownership so cleanup disables only the parent this run enabled. */
        OSStatus enable_status =
            TISEnableInputSource(pinyin_parent_input_source);
        if (enable_status != noErr) {
          /* Enablement refusal ends before mode selection; release all retained source identities. */
          CFRelease(pinyin_input_source);
          CFRelease(pinyin_parent_input_source);
          CFRelease(previous_input_source);
          fprintf(stderr, "could not enable the Pinyin parent source: %d\n",
                  (int)enable_status);
          return 1;
        }
        pinyin_parent_enabled_by_probe = YES;
      }
      OSStatus selection_status = TISSelectInputSource(pinyin_input_source);
      CFRelease(pinyin_input_source);
      if (selection_status != noErr) {
        /* Selection failure attempts to restore only this run's parent enablement, then releases retained identities. */
        if (pinyin_parent_enabled_by_probe) {
          TISDisableInputSource(pinyin_parent_input_source);
        }
        CFRelease(pinyin_parent_input_source);
        CFRelease(previous_input_source);
        fprintf(stderr, "could not select the Pinyin input source: %d\n",
                (int)selection_status);
        return 1;
      }
      usleep(250000);
    }

    // Selecting an input mode can invalidate the active text-input context.
    // Refocus only after selection so the raw key events reach the requested
    // IME instead of the context that existed before the source transition.
    focus_input_surface(bounds, surface);

    for (NSUInteger index = 0; index < text.length; index += 1) {
      /* Each character has a finite down/up pair; previous successfully submitted keys are not replayed on failure. */
      unichar character = [text characterAtIndex:index];
      CGKeyCode key_code = key_code_for_character(character);
      if (key_code == UINT16_MAX) {
        /* An unsupported key stops this possibly partial sequence and attempts source restoration before returning. */
        fprintf(stderr, "probe does not support one of the requested keys\n");
        if (previous_input_source != NULL) {
          TISSelectInputSource(previous_input_source);
          CFRelease(previous_input_source);
        }
        if (pinyin_parent_input_source != NULL) {
          if (pinyin_parent_enabled_by_probe) {
            TISDisableInputSource(pinyin_parent_input_source);
          }
          CFRelease(pinyin_parent_input_source);
        }
        return 64;
      }
      if (use_pinyin_input_method) {
        /* Raw keys let the selected IME create composition; injecting Unicode here would bypass that behavior. */
        post_key(key_code, YES);
      } else {
        /* Direct-input mode preserves the supplied character instead of claiming a real IME interpretation. */
        post_character(key_code, character, YES);
      }
      usleep(12000);
      if (use_pinyin_input_method) {
        /* Release the same physical key through the raw event path. */
        post_key(key_code, NO);
      } else {
        /* Match the direct character key-down with its corresponding Unicode key-up. */
        post_character(key_code, character, NO);
      }
      usleep(12000);
    }

    if (use_pinyin_input_method) {
      /* Space requests candidate confirmation; actual synthetic-title acknowledgement is checked separately. */
      post_key(kSpaceKeyCode, YES);
      usleep(12000);
      post_key(kSpaceKeyCode, NO);

      BOOL committed_input = NO;
      for (NSUInteger poll = 0; poll < kTransitionPollLimit; poll += 1) {
        /* Only the explicit committed-title marker ends the bounded wait successfully. */
        if (input_surface_committed_input_method_text(target_pid, surface)) {
          committed_input = YES;
          break;
        }
        usleep(1000);
      }
      /* Attempt both source and owned-parent restoration before reporting commit or restoration failure. */
      OSStatus restore_status = TISSelectInputSource(previous_input_source);
      CFRelease(previous_input_source);
      OSStatus parent_restore_status = noErr;
      if (pinyin_parent_enabled_by_probe) {
        parent_restore_status =
            TISDisableInputSource(pinyin_parent_input_source);
      }
      CFRelease(pinyin_parent_input_source);
      if (!committed_input) {
        /* Missing commit remains failure even if source restoration succeeded. */
        fprintf(stderr, "Pinyin input did not commit non-ASCII text\n");
        return 1;
      }
      if (restore_status != noErr) {
        /* A committed input cannot hide a failed attempt to restore the previous system source. */
        fprintf(stderr, "could not restore the previous input source\n");
        return 1;
      }
      if (parent_restore_status != noErr) {
        /* Parent enablement cleanup is an independent requirement. */
        fprintf(stderr, "could not restore the Pinyin parent source state\n");
        return 1;
      }
    }

    if (click_row >= 0) {
      /* Apply the selected fixed launcher-row action only after the text sequence finishes. */
      CGPoint result_point = CGPointMake(
          CGRectGetMidX(bounds),
          CGRectGetMinY(bounds) + 117.0 + (58.0 * (CGFloat)click_row));
      post_mouse(kCGEventMouseMoved, result_point);
      usleep(50000);
      post_mouse(kCGEventLeftMouseDown, result_point);
      usleep(20000);
      post_mouse(kCGEventLeftMouseUp, result_point);
      usleep(1000000);
    } else if (press_enter || press_escape) {
      /* The existing escape action takes precedence if both finite action flags were supplied. */
      usleep(300000);
      CGKeyCode action_key = press_escape ? 53 : 36;
      post_key(action_key, YES);
      usleep(12000);
      post_key(action_key, NO);
    }
  }

  return 0;
}
