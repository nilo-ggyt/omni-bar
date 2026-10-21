#include <Carbon/Carbon.h>
#include <stdint.h>
#include <stdio.h>

/* This standalone floor control owns only a Carbon registration; it is never linked into Omni. */
enum {
  OMNI_BENCHMARK_HOT_KEY_SIGNATURE = 0x4f4d4e49,
  OMNI_BENCHMARK_HOT_KEY_IDENTIFIER = 1,
};

/* Reject events outside this registration rather than attributing another application's shortcut. */
static OSStatus record_benchmark_hot_key(EventHandlerCallRef next_handler,
                                         EventRef event,
                                         void *context) {
  (void)next_handler;
  (void)context;

  /* Carbon initializes the typed payload synchronously; a failed extraction proves no delivery. */
  EventHotKeyID received_hot_key = {0};
  OSStatus status = GetEventParameter(
      event, kEventParamDirectObject, typeEventHotKeyID, NULL,
      sizeof(received_hot_key), NULL, &received_hot_key);
  if (status != noErr) {
    return status;
  }

  /* Both fields belong to this process's registration, not the product shortcut preference. */
  if (received_hot_key.signature != OMNI_BENCHMARK_HOT_KEY_SIGNATURE ||
      received_hot_key.id != OMNI_BENCHMARK_HOT_KEY_IDENTIFIER) {
    return eventNotHandledErr;
  }

  return noErr;
}

/* The control retains its handler and hot key until its actual event loop returns. */
int main(void) {
  EventTargetRef event_dispatcher = GetEventDispatcherTarget();
  const EventTypeSpec hot_key_event = {
      .eventClass = kEventClassKeyboard,
      .eventKind = kEventHotKeyPressed,
  };
  EventHandlerRef event_handler = NULL;
  /* The dispatcher is the actual notification target, matching the native Carbon delivery path. */
  OSStatus status = InstallEventHandler(
      event_dispatcher,
      NewEventHandlerUPP(record_benchmark_hot_key), 1, &hot_key_event, NULL,
      &event_handler);
  if (status != noErr) {
    fprintf(stderr, "cannot install benchmark hot-key handler: %d\n",
            (int)status);
    return 1;
  }

  const EventHotKeyID hot_key_identifier = {
      .signature = OMNI_BENCHMARK_HOT_KEY_SIGNATURE,
      .id = OMNI_BENCHMARK_HOT_KEY_IDENTIFIER,
  };
  EventHotKeyRef hot_key = NULL;
  /* This isolated comparison executable has a fixed key; running it never edits Omni's settings. */
  status = RegisterEventHotKey(kVK_Space, cmdKey | shiftKey,
                               hot_key_identifier, event_dispatcher, 0,
                               &hot_key);
  if (status != noErr) {
    fprintf(stderr, "cannot register benchmark hot key: %d\n", (int)status);
    /* Registration failed after handler creation, so return that already acquired resource here. */
    RemoveEventHandler(event_handler);
    return 1;
  }

  /* Receive one retained event at a time; release it even if downstream dispatch rejects it. */
  for (;;) {
    EventRef received_event = NULL;
    status = ReceiveNextEvent(1, &hot_key_event, kEventDurationForever, true,
                              &received_event);
    if (status != noErr || received_event == NULL) {
      break;
    }
    status = SendEventToEventTarget(received_event, event_dispatcher);
    ReleaseEvent(received_event);
    if (status != noErr && status != eventNotHandledErr) {
      break;
    }
  }

  /* Release registration before handler, reversing acquisition and leaving no callable context. */
  UnregisterEventHotKey(hot_key);
  RemoveEventHandler(event_handler);
  if (status != noErr) {
    fprintf(stderr, "benchmark event loop stopped unexpectedly: %d\n",
            (int)status);
    return 1;
  }
  return 0;
}
