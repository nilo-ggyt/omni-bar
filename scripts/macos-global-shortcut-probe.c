#include <Carbon/Carbon.h>
#include <CoreGraphics/CoreGraphics.h>
#include <stdbool.h>
#include <stdio.h>
#include <unistd.h>

static int post_benchmark_key(CGKeyCode key_code, bool key_down,
                              CGEventFlags flags) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, key_down);
  if (event == NULL) {
    return -1;
  }

  CGEventSetFlags(event, flags);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
  return 0;
}

static int post_benchmark_shortcut(void) {
  const CGEventFlags command = kCGEventFlagMaskCommand;
  const CGEventFlags command_shift =
      kCGEventFlagMaskCommand | kCGEventFlagMaskShift;
  const struct {
    CGKeyCode key_code;
    bool key_down;
    CGEventFlags flags;
  } events[] = {
      {kVK_Command, true, command},
      {kVK_Shift, true, command_shift},
      {kVK_Space, true, command_shift},
      {kVK_Space, false, command_shift},
      {kVK_Shift, false, command},
      {kVK_Command, false, 0},
  };

  for (size_t index = 0; index < sizeof(events) / sizeof(events[0]);
       index += 1) {
    if (post_benchmark_key(events[index].key_code, events[index].key_down,
                           events[index].flags) != 0) {
      return -1;
    }
    usleep(5000);
  }
  return 0;
}

int main(void) {
  if (!CGPreflightPostEventAccess()) {
    fprintf(stderr, "process does not have permission to post input events\n");
    return 77;
  }

  if (post_benchmark_shortcut() != 0) {
    fprintf(stderr, "cannot create benchmark shortcut event sequence\n");
    return 1;
  }
  return 0;
}
