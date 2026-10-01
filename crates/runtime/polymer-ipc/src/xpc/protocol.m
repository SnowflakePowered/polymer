#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <IOSurface/IOSurfaceObjC.h>

// NSXPCInterface requires clang's extended protocol metadata. Implementations
// and reply blocks live in Rust. Proxy sends use clang because NSXPC dynamically
// forwards selectors that objc2 debug method-table validation cannot inspect.
@protocol PolymerPacket <NSObject>
- (void)ping:(void (^)(int status))reply;
- (void)sendData:(NSData *)data file:(NSFileHandle *)file
        surfaces:(NSArray<IOSurface *> *)surfaces
           event:(MTLSharedEventHandle *)event
   consumerEvent:(MTLSharedEventHandle *)consumerEvent
           reply:(void (^)(int status))reply;
- (void)call:(NSData *)data reply:(void (^)(NSData *data, int status))reply;
@end

@protocol PolymerDirectory <NSObject>
- (void)publish:(NSString *)name endpoint:(NSXPCListenerEndpoint *)endpoint
          reply:(void (^)(int status))reply;
- (void)resolve:(NSString *)name reply:(void (^)(NSXPCListenerEndpoint *endpoint))reply;
@end

Protocol *polymer_packet_protocol(void) { return @protocol(PolymerPacket); }
Protocol *polymer_directory_protocol(void) { return @protocol(PolymerDirectory); }

void polymer_send(id<PolymerPacket> proxy, NSData *data, NSFileHandle *file,
                  NSArray<IOSurface *> *surfaces, MTLSharedEventHandle *event,
                  MTLSharedEventHandle *consumerEvent, void (^reply)(int)) {
    [proxy sendData:data file:file surfaces:surfaces event:event consumerEvent:consumerEvent reply:reply];
}
void polymer_call(id<PolymerPacket> proxy, NSData *data, void (^reply)(NSData *, int)) {
    [proxy call:data reply:reply];
}
void polymer_publish(id<PolymerDirectory> proxy, NSString *name,
                     NSXPCListenerEndpoint *endpoint, void (^reply)(int)) {
    [proxy publish:name endpoint:endpoint reply:reply];
}
void polymer_resolve(id<PolymerDirectory> proxy, NSString *name,
                     void (^reply)(NSXPCListenerEndpoint *)) {
    [proxy resolve:name reply:reply];
}

void polymer_ping(id<PolymerPacket> proxy, void (^reply)(int)) { [proxy ping:reply]; }
