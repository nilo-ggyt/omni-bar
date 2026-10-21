#import <Cocoa/Cocoa.h>

// Historical whole-string TextKit comparison only; the product never links this executable.
// Its deliberate full-document allocation is a control, not the mmap/viewport implementation.
// Parse only a finite synthetic workload size, never a user document pathname.
static NSUInteger markdown_byte_budget(const char *argument) {
  if (argument == NULL || argument[0] == '\0') {
    return 0;
  }

  // Reject malformed or oversized input before allocating the comparison's retained string.
  char *end = NULL;
  unsigned long long parsed = strtoull(argument, &end, 10);
  if (end == argument || end == NULL || end[0] != '\0' ||
      parsed > 16ULL * 1024ULL * 1024ULL) {
    return 0;
  }
  return (NSUInteger)parsed;
}

// Build ASCII-only synthetic prose, for which NSString code units and UTF-8 bytes coincide.
static NSString *representative_markdown(NSUInteger target_bytes) {
  if (target_bytes == 0) {
    return @"# Omni Markdown\n\nStart writing.\n";
  }

  static NSString *const prose_sentence =
      @"A compact Markdown paragraph exercises headings, emphasis, links, "
       "inline code, Unicode text, and lists while keeping a realistic paragraph "
       "density for a long-form note. ";
  NSMutableString *markdown_section =
      [[NSMutableString alloc] initWithString:@"## Heading\n\n"];
  // Repeat a fixed semantic mix so size comparisons do not depend on a user's private content.
  for (NSUInteger sentence = 0; sentence < 6; sentence += 1) {
    [markdown_section appendString:prose_sentence];
  }
  [markdown_section
      appendString:@"\n\n- First item\n- Second item\n\n> A short quotation.\n\n"];
  NSMutableString *document =
      [[NSMutableString alloc] initWithCapacity:target_bytes];
  // This comparison intentionally retains the whole string; it cannot prove bounded product reads.
  while (document.length + markdown_section.length <= target_bytes) {
    [document appendString:markdown_section];
  }
  // Fill only the final ASCII remainder instead of overshooting the declared synthetic size.
  if (document.length < target_bytes) {
    [document appendString:[markdown_section
                               substringToIndex:target_bytes - document.length]];
  }
  return [document copy];
}

@interface OmniMemoryFloorMarkdownWindow : NSWindow
@end

@implementation OmniMemoryFloorMarkdownWindow

- (BOOL)canBecomeKeyWindow {
  // The undecorated comparison still needs real AppKit input focus for manual native diagnostics.
  return YES;
}

- (BOOL)canBecomeMainWindow {
  // Permit ordinary activation without adding title-bar buttons to this standalone window.
  return YES;
}

@end

@interface OmniMemoryFloorMarkdownTextView : NSTextView
@end

@implementation OmniMemoryFloorMarkdownTextView

- (void)insertText:(id)input replacementRange:(NSRange)replacementRange {
  // Inspect only this synthetic insertion; leave committed editing to the actual NSTextView.
  NSString *inserted_text =
      [input isKindOfClass:NSAttributedString.class]
          ? ((NSAttributedString *)input).string
          : ([input isKindOfClass:NSString.class] ? (NSString *)input : @"");
  [super insertText:input replacementRange:replacementRange];
  // A non-ASCII commit marks this control's title, not an input-method or candidate-geometry proof.
  if (inserted_text.length > 0 &&
      ![inserted_text canBeConvertedToEncoding:NSASCIIStringEncoding]) {
    self.window.title = @"Omni Markdown IME committed";
  }
}

@end

@interface OmniMemoryFloorMarkdownDelegate : NSObject <NSApplicationDelegate>
@property(nonatomic, strong) NSWindow *markdownWindow;
@property(nonatomic, copy) NSString *initialDocument;
@end

@implementation OmniMemoryFloorMarkdownDelegate

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
  (void)notification;

  // The delegate retains the one comparison window until its owning application process exits.
  const NSRect window_frame = NSMakeRect(0.0, 0.0, 1180.0, 760.0);
  self.markdownWindow = [[OmniMemoryFloorMarkdownWindow alloc]
      initWithContentRect:window_frame
                styleMask:NSWindowStyleMaskBorderless
                  backing:NSBackingStoreBuffered
                    defer:NO];
  self.markdownWindow.releasedWhenClosed = NO;

  // Native scrolling and width tracking expose actual TextKit layout costs, without a custom viewport.
  NSScrollView *scroll_view =
      [[NSScrollView alloc] initWithFrame:self.markdownWindow.contentView.bounds];
  scroll_view.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
  scroll_view.hasVerticalScroller = YES;
  scroll_view.hasHorizontalScroller = NO;
  scroll_view.drawsBackground = YES;

  NSTextView *editor =
      [[OmniMemoryFloorMarkdownTextView alloc] initUsingTextLayoutManager:YES];
  editor.frame = scroll_view.contentView.bounds;
  editor.autoresizingMask = NSViewWidthSizable;
  editor.minSize = NSMakeSize(0.0, scroll_view.contentView.bounds.size.height);
  editor.maxSize = NSMakeSize(CGFLOAT_MAX, CGFLOAT_MAX);
  editor.verticallyResizable = YES;
  editor.horizontallyResizable = NO;
  editor.textContainer.widthTracksTextView = YES;
  editor.textContainerInset = NSMakeSize(36.0, 32.0);
  editor.font = [NSFont monospacedSystemFontOfSize:15.0
                                           weight:NSFontWeightRegular];
  // Disable automatic text transformations to keep synthetic bytes stable across native edits.
  editor.richText = NO;
  editor.automaticQuoteSubstitutionEnabled = NO;
  editor.automaticDashSubstitutionEnabled = NO;
  editor.automaticTextReplacementEnabled = NO;
  editor.automaticSpellingCorrectionEnabled = NO;
  editor.automaticLinkDetectionEnabled = NO;
  editor.automaticDataDetectionEnabled = NO;
  editor.automaticTextCompletionEnabled = NO;
  editor.continuousSpellCheckingEnabled = NO;
  editor.grammarCheckingEnabled = NO;
  editor.textLayoutManager.limitsLayoutForSuspiciousContents = YES;
  // Transfer the whole-string control's initial claim to TextKit and release the delegate copy.
  editor.string = self.initialDocument;
  self.initialDocument = nil;

  // Publish the retained content graph before requesting focus or creating the native input context.
  scroll_view.documentView = editor;
  self.markdownWindow.contentView = scroll_view;
  [self.markdownWindow center];
  [self.markdownWindow makeKeyAndOrderFront:nil];
  [self.markdownWindow makeFirstResponder:editor];
  (void)editor.inputContext;
  [NSApp activateIgnoringOtherApps:YES];
}

- (BOOL)applicationShouldTerminateAfterLastWindowClosed:
    (NSApplication *)sender {
  (void)sender;
  // Keep the original manual control resident; this return is not product close-release evidence.
  return NO;
}

@end

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    // Only the explicit comparison executable owns this accessory application and synthetic text.
    NSUInteger target_bytes =
        markdown_byte_budget(argc >= 2 ? argv[1] : NULL);
    NSApplication *application = NSApplication.sharedApplication;
    application.activationPolicy = NSApplicationActivationPolicyAccessory;
    OmniMemoryFloorMarkdownDelegate *delegate =
        [[OmniMemoryFloorMarkdownDelegate alloc] init];
    delegate.initialDocument = representative_markdown(target_bytes);
    application.delegate = delegate;
    [application run];
  }
  return 0;
}
