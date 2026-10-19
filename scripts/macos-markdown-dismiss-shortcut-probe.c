#include <CoreGraphics/CoreGraphics.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static const CGKeyCode kEscapeKeyCode = 53;
static const CGKeyCode kWKeyCode = 13;

static bool post_key(CGKeyCode key_code, bool key_down, CGEventFlags flags) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, key_down);
  if (event == NULL) {
    return false;
  }
  CGEventSetFlags(event, flags);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
  return true;
}

int main(int argc, const char *argv[]) {
  if (argc != 2 ||
      (strcmp(argv[1], "escape") != 0 && strcmp(argv[1], "command-w") != 0)) {
    fprintf(stderr, "usage: %s <escape|command-w>\n", argv[0]);
    return 64;
  }
  if (!CGPreflightPostEventAccess()) {
    fprintf(stderr, "process does not have permission to post input events\n");
    return 77;
  }

  CGKeyCode key_code = strcmp(argv[1], "escape") == 0 ? kEscapeKeyCode : kWKeyCode;
  CGEventFlags flags =
      strcmp(argv[1], "command-w") == 0 ? kCGEventFlagMaskCommand : 0;
  if (!post_key(key_code, true, flags)) {
    fprintf(stderr, "could not create key-down event\n");
    return 1;
  }
  usleep(12000);
  if (!post_key(key_code, false, flags)) {
    fprintf(stderr, "could not create key-up event\n");
    return 1;
  }
  return 0;
}
