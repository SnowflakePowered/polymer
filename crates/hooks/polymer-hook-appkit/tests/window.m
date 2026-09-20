#import <Cocoa/Cocoa.h>
#include <dlfcn.h>
#include <unistd.h>
int main(void) { @autoreleasepool {
  NSApplication *app = [NSApplication sharedApplication];
  [app setActivationPolicy:NSApplicationActivationPolicyRegular];
  NSWindow *window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,120,80) styleMask:NSWindowStyleMaskTitled backing:NSBackingStoreBuffered defer:NO];
  [window setReleasedWhenClosed:NO];
  [window makeKeyAndOrderFront:nil];
  [app finishLaunching];
  [[NSRunLoop currentRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.2]];
  [app updateWindows];
  [window orderFrontRegardless];
  [app updateWindows];
  void *lib = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_NOW);
  int (*cid)(void) = dlsym(lib,"CGSMainConnectionID");
  CFArrayRef (*spaces)(int,int,CFArrayRef) = dlsym(lib,"CGSCopySpacesForWindows");
  CFArrayRef (*managed)(int) = dlsym(lib,"CGSCopyManagedDisplaySpaces");
  NSArray *members = CFBridgingRelease(spaces(cid(),7,(__bridge CFArrayRef)@[@(window.windowNumber)]));
  NSArray *displays = CFBridgingRelease(managed(cid()));
  BOOL reachable = NO;
  for (NSDictionary *display in displays) for (NSDictionary *space in display[@"Spaces"]) if ([members containsObject:space[@"id64"]]) reachable=YES;
  NSArray *onscreen = CFBridgingRelease(CGWindowListCopyWindowInfo(kCGWindowListOptionOnScreenOnly,0));
  int visible=0; for (NSDictionary *w in onscreen) if ([w[(id)kCGWindowOwnerPID] intValue]==getpid()) visible++;
  BOOL pass = members.count<=1 && !reachable && !visible && app.activationPolicy==NSApplicationActivationPolicyAccessory;
  printf("private=%d onscreen=%d accessory=%d ordered=%d\n", !reachable,visible,app.activationPolicy==NSApplicationActivationPolicyAccessory,window.visible);
  [window close];
  return pass?0:1;
} }
