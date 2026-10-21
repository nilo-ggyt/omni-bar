#include <Carbon/Carbon.h>
#include <CoreGraphics/CoreGraphics.h>
#include <stdbool.h>
#include <stdio.h>
#include <unistd.h>

/* Post one explicitly synthetic keyboard event, releasing the +1 Core Graphics event after submission. */
static int post_benchmark_key(CGKeyCode key_code, bool key_down,
                              CGEventFlags flags) {
  CGEventRef event = CGEventCreateKeyboardEvent(NULL, key_code, key_down);
  if (event == NULL) {
    /* Failed creation submits no event; callers must not report the sequence as complete. */
    return -1;
  }

  CGEventSetFlags(event, flags);
  CGEventPost(kCGHIDEventTap, event);
  CFRelease(event);
  return 0;
}

/* Exercise only the fixed benchmark chord; this tool does not read or change the product's configured shortcut. */
static int post_benchmark_shortcut(void) {
  const CGEventFlags command = kCGEventFlagMaskCommand;
  const CGEventFlags command_shift =
      kCGEventFlagMaskCommand | kCGEventFlagMaskShift;
  const struct {
    CGKeyCode key_code;
    bool key_down;
    CGEventFlags flags;
  } events[] = {
      /* Explicit modifier transitions bracket Space, with corresponding releases in the same finite sequence. */
      {kVK_Command, true, command},
      {kVK_Shift, true, command_shift},
      {kVK_Space, true, command_shift},
      {kVK_Space, false, command_shift},
      {kVK_Shift, false, command},
      {kVK_Command, false, 0},
  };

  for (size_t index = 0; index < sizeof(events) / sizeof(events[0]);
       index += 1) {
    /* Synthetic submission establishes neither hardware input nor the application's eventual callback delivery. */
    if (post_benchmark_key(events[index].key_code, events[index].key_down,
                           events[index].flags) != 0) {
      /* Report partial-sequence failure; a failed allocation cannot certify that later key-up events were submitted. */
      return -1;
    }
    /* Preserve event order with fixed spacing; elapsed sleep is not evidence that the target received input. */
    usleep(5000);
  }
  return 0;
}

/* Require existing posting permission without opening a prompt or registering any global product shortcut. */
int main(void) {
  if (!CGPreflightPostEventAccess()) {
    /* Permission refusal ends before event creation, leaving application and system settings untouched. */
    fprintf(stderr, "process does not have permission to post input events\n");
    return 77;
  }

  if (post_benchmark_shortcut() != 0) {
    /* A partly submitted chord remains a failed observation, not a successful launcher-open result. */
    fprintf(stderr, "cannot create benchmark shortcut event sequence\n");
    return 1;
  }
  return 0;
}
