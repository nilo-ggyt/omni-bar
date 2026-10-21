#import <Cocoa/Cocoa.h>
#include <string.h>

// Historical standalone AppKit floor controls only. These modes neither compose Omni's Host
// nor read its settings, clipboard, plugin catalog or Markdown vault.
@interface OmniMemoryFloorLauncherPanel : NSPanel
@end

@implementation OmniMemoryFloorLauncherPanel

- (BOOL)canBecomeKeyWindow {
  // The undecorated comparison panel must still accept its explicit native input experiment.
  return YES;
}

- (void)cancelOperation:(id)sender {
  // Input-source transitions can issue cancelOperation: without a user Escape.
  // Escape is handled from the query view's actual keyDown event instead.
  (void)sender;
}

@end

@interface OmniMemoryFloorHomeCanvas : NSView
@end

@implementation OmniMemoryFloorHomeCanvas

- (void)drawRect:(NSRect)dirtyRect {
  [super drawRect:dirtyRect];
  // Keep one finite synthetic capability label set; this is painting cost, not catalog loading.
  static NSArray<NSString *> *capability_labels = nil;
  if (capability_labels == nil) {
    capability_labels = @[
      @"Markdown", @"Clipboard", @"Calculator", @"Convert", @"Apps",
      @"Settings"
    ];
  }
  NSDictionary<NSAttributedStringKey, id> *label_attributes = @{
    NSFontAttributeName : [NSFont systemFontOfSize:22.0
                                         weight:NSFontWeightRegular],
    NSForegroundColorAttributeName : NSColor.secondaryLabelColor,
  };
  CGFloat item_width = 226.0;
  // Six static labels form a fixed two-row control, with no retained per-item view graph.
  for (NSUInteger item = 0; item < capability_labels.count; item += 1) {
    NSUInteger row = item / 3;
    NSUInteger column = item % 3;
    NSRect item_frame = NSMakeRect(24.0 + (CGFloat)column * 244.0,
                                   16.0 + (CGFloat)(1 - row) * 38.0,
                                   item_width, 32.0);
    [NSColor.controlBackgroundColor setFill];
    [[NSBezierPath bezierPathWithRoundedRect:item_frame
                                     xRadius:7.0
                                     yRadius:7.0] fill];
    [capability_labels[item]
        drawAtPoint:NSMakePoint(NSMinX(item_frame) + 10.0,
                                NSMinY(item_frame) + 3.0)
     withAttributes:label_attributes];
  }
}

@end

@interface OmniMemoryFloorQueryView : NSView <NSTextInputClient>
@property(nonatomic, strong) NSMutableString *queryText;
@property(nonatomic) NSRange selectedTextRange;
@property(nonatomic) NSRange markedTextRange;
@property(nonatomic, copy)
    NSDictionary<NSAttributedStringKey, id> *queryDrawingAttributes;
@end

@implementation OmniMemoryFloorQueryView

- (instancetype)initWithFrame:(NSRect)frameRect {
  self = [super initWithFrame:frameRect];
  if (self != nil) {
    // This comparison owns a single mutable query plus UTF-16 selection/composition coordinates.
    _queryText = [[NSMutableString alloc] init];
    _selectedTextRange = NSMakeRange(0, 0);
    _markedTextRange = NSMakeRange(NSNotFound, 0);
    _queryDrawingAttributes = @{
      NSFontAttributeName : [NSFont systemFontOfSize:22.0
                                           weight:NSFontWeightRegular],
      NSForegroundColorAttributeName : NSColor.labelColor,
    };
  }
  return self;
}

- (BOOL)acceptsFirstResponder {
  // Real NSTextInputClient delivery requires this view to become the panel's first responder.
  return YES;
}

- (void)mouseDown:(NSEvent *)event {
  (void)event;
  // Clicking acquires native focus without interpreting the click as a text or selection edit.
  [self.window makeFirstResponder:self];
}

- (void)keyDown:(NSEvent *)event {
  if (event.keyCode == 53) {
    // Only this standalone control exits on a real Escape key; IME cancellation is not an exit.
    [NSApp terminate:nil];
    return;
  }
  // AppKit and the active input method interpret every other event through the public client protocol.
  [self interpretKeyEvents:@[ event ]];
}

- (NSRange)boundedReplacementRange:(NSRange)requestedRange {
  NSRange candidate = requestedRange;
  if (candidate.location == NSNotFound) {
    // The input protocol's unspecified range targets composition first, otherwise the current selection.
    candidate = self.markedTextRange.location == NSNotFound
                    ? self.selectedTextRange
                    : self.markedTextRange;
  }
  if (candidate.location > self.queryText.length) {
    // This historical comparison clamps stale ranges; it is not the product's input-authority contract.
    candidate.location = self.queryText.length;
    candidate.length = 0;
  } else if (candidate.length > self.queryText.length - candidate.location) {
    candidate.length = self.queryText.length - candidate.location;
  }
  return candidate;
}

- (NSString *)plainTextFromInput:(id)input {
  // Accept only the two public text payload forms; attributed drawing metadata is not persisted.
  if ([input isKindOfClass:NSAttributedString.class]) {
    return ((NSAttributedString *)input).string;
  }
  if ([input isKindOfClass:NSString.class]) {
    return (NSString *)input;
  }
  return @"";
}

- (void)insertText:(id)input replacementRange:(NSRange)replacementRange {
  // Commit into this control's query and collapse selection after the actual inserted UTF-16 units.
  NSString *inserted_text = [self plainTextFromInput:input];
  NSRange bounded_range = [self boundedReplacementRange:replacementRange];
  [self.queryText replaceCharactersInRange:bounded_range
                                withString:inserted_text];
  self.selectedTextRange =
      NSMakeRange(bounded_range.location + inserted_text.length, 0);
  self.markedTextRange = NSMakeRange(NSNotFound, 0);
  // The synthetic title marks non-ASCII commitment only, not actual candidate placement or product acceptance.
  if (inserted_text.length > 0 &&
      ![inserted_text canBeConvertedToEncoding:NSASCIIStringEncoding]) {
    self.window.title = @"Omni IME committed";
  }
  self.needsDisplay = YES;
}

- (void)doCommandBySelector:(SEL)selector {
  if (selector == @selector(deleteBackward:)) {
    if (self.selectedTextRange.length > 0) {
      // A selected range is deleted as one user operation before considering a preceding grapheme.
      [self.queryText deleteCharactersInRange:self.selectedTextRange];
      self.selectedTextRange = NSMakeRange(self.selectedTextRange.location, 0);
    } else if (self.selectedTextRange.location > 0) {
      // Backspace removes a complete composed character, keeping surrogate pairs and combining marks intact.
      NSRange previous_character = [self.queryText
          rangeOfComposedCharacterSequenceAtIndex:self.selectedTextRange.location -
                                                   1];
      [self.queryText deleteCharactersInRange:previous_character];
      self.selectedTextRange = NSMakeRange(previous_character.location, 0);
    }
  } else if (selector == @selector(moveLeft:)) {
    // These historical movement controls use UTF-16 positions; they are not product grapheme-navigation evidence.
    if (self.selectedTextRange.location > 0) {
      self.selectedTextRange =
          NSMakeRange(self.selectedTextRange.location - 1, 0);
    }
  } else if (selector == @selector(moveRight:)) {
    if (NSMaxRange(self.selectedTextRange) < self.queryText.length) {
      self.selectedTextRange = NSMakeRange(NSMaxRange(self.selectedTextRange) + 1,
                                           0);
    }
  } else if (selector == @selector(cancelOperation:)) {
    // IME composition owns this command. User Escape is handled in keyDown:.
  }
  self.markedTextRange = NSMakeRange(NSNotFound, 0);
  // Finish this simple control's composition display after handling or declining the command.
  self.needsDisplay = YES;
}

- (void)setMarkedText:(id)input
         selectedRange:(NSRange)selectedRange
       replacementRange:(NSRange)replacementRange {
  // Replace only the current composition target, then clamp the input method's relative selection.
  NSString *marked_text = [self plainTextFromInput:input];
  NSRange bounded_range = [self boundedReplacementRange:replacementRange];
  [self.queryText replaceCharactersInRange:bounded_range withString:marked_text];
  self.markedTextRange = NSMakeRange(bounded_range.location, marked_text.length);
  NSUInteger selected_location =
      MIN(selectedRange.location, marked_text.length) + bounded_range.location;
  NSUInteger selected_length =
      MIN(selectedRange.length,
          bounded_range.location + marked_text.length - selected_location);
  self.selectedTextRange =
      NSMakeRange(selected_location, selected_length);
  self.needsDisplay = YES;
}

- (void)unmarkText {
  // Clear provisional ownership while preserving committed query characters and current selection.
  self.markedTextRange = NSMakeRange(NSNotFound, 0);
  self.needsDisplay = YES;
}

- (NSRange)selectedRange {
  // Expose this view's UTF-16 selection directly to the public text-input protocol.
  return self.selectedTextRange;
}

- (NSRange)markedRange {
  // NSNotFound distinguishes absence of composition from a valid empty marked range.
  return self.markedTextRange;
}

- (BOOL)hasMarkedText {
  // Composition presence follows its declared location, not the displayed string's length.
  return self.markedTextRange.location != NSNotFound;
}

- (NSAttributedString *)attributedSubstringForProposedRange:(NSRange)range
                                                actualRange:
                                                    (NSRangePointer)actualRange {
  // Return only the clamped requested slice; report the same actual UTF-16 extent to the input method.
  NSRange bounded_range = [self boundedReplacementRange:range];
  if (actualRange != NULL) {
    *actualRange = bounded_range;
  }
  NSString *substring = [self.queryText substringWithRange:bounded_range];
  return [[NSAttributedString alloc]
      initWithString:substring
          attributes:self.queryDrawingAttributes];
}

- (NSArray<NSAttributedStringKey> *)validAttributesForMarkedText {
  // This drawing-only comparison accepts text without interpreting input-method styling metadata.
  return @[];
}

- (NSRect)firstRectForCharacterRange:(NSRange)range
                          actualRange:(NSRangePointer)actualRange {
  // Convert the fixed comparison caret through view, window and screen coordinates. This
  // intentionally simplified anchor cannot certify the product's measured glyph/candidate geometry.
  NSRange bounded_range = [self boundedReplacementRange:range];
  if (actualRange != NULL) {
    *actualRange = bounded_range;
  }
  NSRect local_cursor = NSMakeRect(14.0, 13.0, 1.0, 28.0);
  NSRect window_cursor = [self convertRect:local_cursor toView:nil];
  return [self.window convertRectToScreen:window_cursor];
}

- (NSUInteger)characterIndexForPoint:(NSPoint)point {
  (void)point;
  // This floor control has no hit-test layout; clicks select the end rather than proving product navigation.
  return self.queryText.length;
}

- (void)drawRect:(NSRect)dirtyRect {
  [super drawRect:dirtyRect];
  // Draw the one retained query or placeholder directly, without a hidden NSTextView owner.
  [NSColor.controlBackgroundColor setFill];
  [[NSBezierPath bezierPathWithRoundedRect:self.bounds xRadius:10.0 yRadius:10.0]
      fill];

  NSString *displayed_text = self.queryText.length == 0
                                 ? @"Search apps and commands"
                                 : self.queryText;
  NSDictionary<NSAttributedStringKey, id> *drawing_attributes =
      self.queryText.length == 0
          ? @{
              NSFontAttributeName : self.queryDrawingAttributes[NSFontAttributeName],
              NSForegroundColorAttributeName : NSColor.placeholderTextColor,
            }
          : self.queryDrawingAttributes;
  [displayed_text drawAtPoint:NSMakePoint(14.0, 13.0)
               withAttributes:drawing_attributes];
}

@end

typedef NS_ENUM(NSUInteger, OmniMemoryFloorLauncherMode) {
  OmniMemoryFloorLauncherModeInputReady,
  OmniMemoryFloorLauncherModeLightweightInput,
  OmniMemoryFloorLauncherModeCompactHome,
  OmniMemoryFloorLauncherModeExpandedLightweightInput,
  OmniMemoryFloorLauncherModeInputUnfocused,
  OmniMemoryFloorLauncherModeWindowOnly,
  OmniMemoryFloorLauncherModeApplicationOnly,
};

@interface OmniMemoryFloorLauncherDelegate : NSObject <NSApplicationDelegate>
@property(nonatomic, strong) OmniMemoryFloorLauncherPanel *launcherPanel;
@property(nonatomic) OmniMemoryFloorLauncherMode launcherMode;
@end

@implementation OmniMemoryFloorLauncherDelegate

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
  (void)notification;

  if (self.launcherMode == OmniMemoryFloorLauncherModeApplicationOnly) {
    // Measure AppKit residency alone, deliberately constructing no window or input graph.
    return;
  }

  // Each explicit mode retains one fixed geometry; this is not a responsive product layout implementation.
  CGFloat panel_height =
      self.launcherMode == OmniMemoryFloorLauncherModeExpandedLightweightInput
          ? 495.0
          : self.launcherMode == OmniMemoryFloorLauncherModeCompactHome ? 168.0
                                                                        : 116.0;
  const NSRect panel_frame = NSMakeRect(0.0, 0.0, 760.0, panel_height);
  self.launcherPanel = [[OmniMemoryFloorLauncherPanel alloc]
      initWithContentRect:panel_frame
                styleMask:NSWindowStyleMaskBorderless
                  backing:NSBackingStoreBuffered
                    defer:NO];
  self.launcherPanel.level = NSFloatingWindowLevel;
  self.launcherPanel.opaque = YES;
  self.launcherPanel.backgroundColor = NSColor.windowBackgroundColor;
  self.launcherPanel.collectionBehavior =
      NSWindowCollectionBehaviorCanJoinAllSpaces |
      NSWindowCollectionBehaviorFullScreenAuxiliary;
  self.launcherPanel.releasedWhenClosed = NO;

  NSView *content_view = self.launcherPanel.contentView;
  if (self.launcherMode == OmniMemoryFloorLauncherModeCompactHome) {
    // Replace only this comparison's content view with its direct-drawing capability canvas.
    content_view =
        [[OmniMemoryFloorHomeCanvas alloc] initWithFrame:panel_frame];
    self.launcherPanel.contentView = content_view;
  }
  NSTextField *query_field = nil;
  OmniMemoryFloorQueryView *lightweight_query = nil;
  if (self.launcherMode == OmniMemoryFloorLauncherModeInputReady ||
      self.launcherMode == OmniMemoryFloorLauncherModeInputUnfocused) {
    // Standard-field modes preserve AppKit's real field-editor allocation, with focus chosen below.
    query_field = [[NSTextField alloc]
        initWithFrame:NSMakeRect(24.0, 32.0, 712.0, 52.0)];
    query_field.placeholderString = @"Search apps and commands";
    query_field.font = [NSFont systemFontOfSize:22.0
                                        weight:NSFontWeightRegular];
    query_field.bezelStyle = NSTextFieldRoundedBezel;
    query_field.focusRingType = NSFocusRingTypeNone;
    [content_view addSubview:query_field];
  } else if (self.launcherMode == OmniMemoryFloorLauncherModeLightweightInput ||
             self.launcherMode == OmniMemoryFloorLauncherModeCompactHome ||
             self.launcherMode ==
                 OmniMemoryFloorLauncherModeExpandedLightweightInput) {
    // Lightweight modes instead own this file's small text-input client; they are separate controls.
    CGFloat query_y =
        self.launcherMode == OmniMemoryFloorLauncherModeCompactHome
            ? 100.0
            : panel_height - 84.0;
    lightweight_query = [[OmniMemoryFloorQueryView alloc]
        initWithFrame:NSMakeRect(24.0, query_y, 712.0, 52.0)];
    [content_view addSubview:lightweight_query];
  }

  // Publish the completed graph before native show/focus can synchronously call into the input client.
  [self.launcherPanel center];
  [self.launcherPanel makeKeyAndOrderFront:nil];
  if (self.launcherMode == OmniMemoryFloorLauncherModeInputReady) {
    [self.launcherPanel makeFirstResponder:query_field];
  } else if (lightweight_query != nil) {
    [self.launcherPanel makeFirstResponder:lightweight_query];
    (void)lightweight_query.inputContext;
  }
  [NSApp activateIgnoringOtherApps:YES];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:
    (NSApplication *)sender {
  (void)sender;
  // Input methods own transient candidate windows. Their teardown must not be
  // mistaken for destruction of the business Launcher surface.
  return NO;
}

@end

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    // Select one named comparison explicitly; unknown arguments never silently choose another workload.
    OmniMemoryFloorLauncherMode launcher_mode =
        OmniMemoryFloorLauncherModeInputReady;
    if (argc == 2 && strcmp(argv[1], "--window-only") == 0) {
      launcher_mode = OmniMemoryFloorLauncherModeWindowOnly;
    } else if (argc == 2 && strcmp(argv[1], "--lightweight-input") == 0) {
      launcher_mode = OmniMemoryFloorLauncherModeLightweightInput;
    } else if (argc == 2 && strcmp(argv[1], "--compact-home") == 0) {
      launcher_mode = OmniMemoryFloorLauncherModeCompactHome;
    } else if (argc == 2 &&
               strcmp(argv[1], "--expanded-lightweight-input") == 0) {
      launcher_mode = OmniMemoryFloorLauncherModeExpandedLightweightInput;
    } else if (argc == 2 && strcmp(argv[1], "--input-unfocused") == 0) {
      launcher_mode = OmniMemoryFloorLauncherModeInputUnfocused;
    } else if (argc == 2 && strcmp(argv[1], "--application-only") == 0) {
      launcher_mode = OmniMemoryFloorLauncherModeApplicationOnly;
    } else if (argc != 1) {
      fprintf(stderr,
              "usage: %s [--lightweight-input | --compact-home | "
              "--expanded-lightweight-input | --input-unfocused | "
              "--window-only | --application-only]\n",
              argv[0]);
      return 64;
    }

    // The local delegate owns the single standalone accessory application's complete comparison graph.
    NSApplication *application = NSApplication.sharedApplication;
    application.activationPolicy = NSApplicationActivationPolicyAccessory;
    OmniMemoryFloorLauncherDelegate *delegate =
        [[OmniMemoryFloorLauncherDelegate alloc] init];
    delegate.launcherMode = launcher_mode;
    application.delegate = delegate;
    [application run];
  }
  return 0;
}
