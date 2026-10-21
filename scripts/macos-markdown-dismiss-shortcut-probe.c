#include <CoreGraphics/CoreGraphics.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

/* These fixed macOS virtual key codes select existing dismiss actions; no new application shortcut is registered. */
static const CGKeyCode kEscapeKeyCode = 53;
static const CGKeyCode kWKeyCode = 13;

/* Submit one synthetic event and release its Core Graphics owner immediately after posting. */
static bool post_key(CGKeyCode key_code, bool key_down, CGEventFlags flags) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, key_down);
  if (event == NULL) {
    /* Allocation failure submits nothing and must prevent a complete-sequence claim. */
    return false;
  }
  CGEventSetFlags(event, flags);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
  return true;
}

/* Post one explicitly selected existing dismiss chord, without starting or locating a target application. */
int main(int argc, const char *argv[]) {
  if (argc != 2 ||
      (strcmp(argv[1], "escape") != 0 && strcmp(argv[1], "command-w") != 0)) {
    /* Reject unknown actions before checking permissions or submitting any keyboard input. */
    fprintf(stderr, "usage: %s <escape|command-w>\n", argv[0]);
    return 64;
  }
  if (!CGPreflightPostEventAccess()) {
    /* Existing permission is required; this diagnostic never requests a prompt or changes system settings. */
    fprintf(stderr, "process does not have permission to post input events\n");
    return 77;
  }

  CGKeyCode key_code = strcmp(argv[1], "escape") == 0 ? kEscapeKeyCode : kWKeyCode;
  CGEventFlags flags =
      strcmp(argv[1], "command-w") == 0 ? kCGEventFlagMaskCommand : 0;
  if (!post_key(key_code, true, flags)) {
    /* A failed key-down allocation leaves no submitted chord to complete. */
    fprintf(stderr, "could not create key-down event\n");
    return 1;
  }
  /* Space the pair for native dispatch; delay does not certify physical input or application receipt. */
  usleep(12000);
  if (!post_key(key_code, false, flags)) {
    /* Preserve partial-submission failure; successful key-down alone cannot prove that the key was released. */
    fprintf(stderr, "could not create key-up event\n");
    return 1;
  }
  return 0;
}
