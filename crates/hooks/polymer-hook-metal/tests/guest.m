#import <Foundation/Foundation.h>
#import <Metal/Metal.h>
#import <QuartzCore/CAMetalLayer.h>
#include <stdatomic.h>
#include <unistd.h>

int main(void) { @autoreleasepool {
    id<MTLDevice> device = MTLCreateSystemDefaultDevice();
    id<MTLCommandQueue> queue = [device newCommandQueue];
    CAMetalLayer *layer = [CAMetalLayer layer];
    layer.device = device; layer.pixelFormat = MTLPixelFormatBGRA8Unorm;
    layer.drawableSize = CGSizeMake(64, 64); layer.maximumDrawableCount = 3;
    __block atomic_int presented = 0;
    for (int frame = 1; frame <= 36; ++frame) { @autoreleasepool {
        id<CAMetalDrawable> drawable = [layer nextDrawable];
        if (!drawable) return 2;
        if (frame == 12) {
            layer.drawableSize = CGSizeMake(128, 96);
            if (layer.drawableSize.width != 64 || layer.drawableSize.height != 64) return 5;
        }
        if (drawable.texture.width != 64 || drawable.texture.height != 64) return 3;
        [drawable addPresentedHandler:^(id<MTLDrawable> value) { atomic_fetch_add(&presented, 1); }];
        id<MTLCommandBuffer> commands = [queue commandBuffer];
        MTLRenderPassDescriptor *pass = [MTLRenderPassDescriptor renderPassDescriptor];
        pass.colorAttachments[0].texture = drawable.texture;
        pass.colorAttachments[0].loadAction = MTLLoadActionClear;
        pass.colorAttachments[0].storeAction = MTLStoreActionStore;
        pass.colorAttachments[0].clearColor = MTLClearColorMake(frame / 64.0, 0.25, 0.5, 1);
        id<MTLRenderCommandEncoder> encoder = [commands renderCommandEncoderWithDescriptor:pass];
        [encoder endEncoding];
        switch (frame % 3) {
            case 0: [commands presentDrawable:drawable]; break;
            case 1: { [commands addScheduledHandler:^(id<MTLCommandBuffer> _) { [drawable present]; }]; break; }
            case 2: [commands presentDrawable:drawable afterMinimumDuration:0]; break;
        }
        [commands commit]; [commands waitUntilCompleted];
        // Deliberately retain the current drawable after GPU completion.
        usleep(2000);
    }}
    for (int i = 0; i < 500 && atomic_load(&presented) != 36; ++i) usleep(10000);
    return atomic_load(&presented) == 36 ? 0 : 4;
}}
