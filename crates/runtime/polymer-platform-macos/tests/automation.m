#import <Cocoa/Cocoa.h>
static BOOL received = NO;
static BOOL keyDown = NO, keyReleased = NO;
@interface InputView : NSView
@end
@implementation InputView
- (BOOL)acceptsFirstMouse:(NSEvent *)event { return YES; }
- (void)mouseDown:(NSEvent *)event {}
- (void)mouseUp:(NSEvent *)event {
  NSPoint p = [self convertPoint:event.locationInWindow fromView:nil];
  received = NSPointInRect(p, self.bounds);
}
@end
int main(void) { @autoreleasepool {
  NSApplication *app = NSApplication.sharedApplication;
  [app setActivationPolicy:NSApplicationActivationPolicyRegular];
  NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(100,100,320,240)
    styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
  window.releasedWhenClosed = NO;
  window.title = @"Polymer automation fixture";
  window.contentView = [[InputView alloc] initWithFrame:NSMakeRect(0,0,320,240)];
  [window makeKeyAndOrderFront:nil];
  [app finishLaunching];
  NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:15];
  while (!(received && keyReleased) && deadline.timeIntervalSinceNow > 0) {
    [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
    [app updateWindows];
    BOOL held = CGEventSourceKeyState(kCGEventSourceStateHIDSystemState, 0x6d);
    if (held) keyDown = YES;
    if (keyDown && !held) keyReleased = YES;
  }
  [window close];
  return received && keyReleased ? 0 : 1;
} }
