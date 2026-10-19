#import <Cocoa/Cocoa.h>

static NSUInteger markdown_byte_budget(const char *argument) {
  if (argument == NULL || argument[0] == '\0') {
    return 0;
  }

  char *end = NULL;
  unsigned long long parsed = strtoull(argument, &end, 10);
  if (end == argument || end == NULL || end[0] != '\0' ||
      parsed > 16ULL * 1024ULL * 1024ULL) {
    return 0;
  }
  return (NSUInteger)parsed;
}

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
  for (NSUInteger sentence = 0; sentence < 6; sentence += 1) {
    [markdown_section appendString:prose_sentence];
  }
  [markdown_section
      appendString:@"\n\n- First item\n- Second item\n\n> A short quotation.\n\n"];
  NSMutableString *document =
      [[NSMutableString alloc] initWithCapacity:target_bytes];
  while (document.length + markdown_section.length <= target_bytes) {
    [document appendString:markdown_section];
  }
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
  return YES;
}

- (BOOL)canBecomeMainWindow {
  return YES;
}

@end

@interface OmniMemoryFloorMarkdownTextView : NSTextView
@end

@implementation OmniMemoryFloorMarkdownTextView

- (void)insertText:(id)input replacementRange:(NSRange)replacementRange {
  NSString *inserted_text =
      [input isKindOfClass:NSAttributedString.class]
          ? ((NSAttributedString *)input).string
          : ([input isKindOfClass:NSString.class] ? (NSString *)input : @"");
  [super insertText:input replacementRange:replacementRange];
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

  const NSRect window_frame = NSMakeRect(0.0, 0.0, 1180.0, 760.0);
  self.markdownWindow = [[OmniMemoryFloorMarkdownWindow alloc]
      initWithContentRect:window_frame
                styleMask:NSWindowStyleMaskBorderless
                  backing:NSBackingStoreBuffered
                    defer:NO];
  self.markdownWindow.releasedWhenClosed = NO;

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
  editor.string = self.initialDocument;
  self.initialDocument = nil;

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
  return NO;
}

@end

int main(int argc, const char *argv[]) {
  @autoreleasepool {
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
