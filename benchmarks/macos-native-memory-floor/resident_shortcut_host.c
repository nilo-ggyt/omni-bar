#include <Carbon/Carbon.h>
#include <stdint.h>
#include <stdio.h>

enum {
  OMNI_BENCHMARK_HOT_KEY_SIGNATURE = 0x4f4d4e49,
  OMNI_BENCHMARK_HOT_KEY_IDENTIFIER = 1,
};

static OSStatus record_benchmark_hot_key(EventHandlerCallRef next_handler,
                                         EventRef event,
                                         void *context) {
  (void)next_handler;
  (void)context;

  EventHotKeyID received_hot_key = {0};
  OSStatus status = GetEventParameter(
      event, kEventParamDirectObject, typeEventHotKeyID, NULL,
      sizeof(received_hot_key), NULL, &received_hot_key);
  if (status != noErr) {
    return status;
  }

  if (received_hot_key.signature != OMNI_BENCHMARK_HOT_KEY_SIGNATURE ||
      received_hot_key.id != OMNI_BENCHMARK_HOT_KEY_IDENTIFIER) {
    return eventNotHandledErr;
  }

  return noErr;
}

int main(void) {
  EventTargetRef event_dispatcher = GetEventDispatcherTarget();
  const EventTypeSpec hot_key_event = {
      .eventClass = kEventClassKeyboard,
      .eventKind = kEventHotKeyPressed,
  };
  EventHandlerRef event_handler = NULL;
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
  status = RegisterEventHotKey(kVK_Space, cmdKey | shiftKey,
                               hot_key_identifier, event_dispatcher, 0,
                               &hot_key);
  if (status != noErr) {
    fprintf(stderr, "cannot register benchmark hot key: %d\n", (int)status);
    RemoveEventHandler(event_handler);
    return 1;
  }

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

  UnregisterEventHotKey(hot_key);
  RemoveEventHandler(event_handler);
  if (status != noErr) {
    fprintf(stderr, "benchmark event loop stopped unexpectedly: %d\n",
            (int)status);
    return 1;
  }
  return 0;
}
