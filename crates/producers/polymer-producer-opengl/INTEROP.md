# OpenGL presentation capture

OpenGL uses `renderMode: "intercept"`. Immediately before native `SwapBuffers` (or the main plane of `wglSwapLayerBuffers`), Polymer blits the current window framebuffer into one of three shared D3D12 resources imported as GL textures.

Required extensions: `GL_EXT_memory_object`, `GL_EXT_memory_object_win32`, `GL_EXT_semaphore`, and `GL_EXT_semaphore_win32`. Support and framebuffer completeness are checked before publishing a stream. Missing support reports an initialization error; there is no NV interop intermediate or CPU/PBO readback fallback.

The backbuffer is selected explicitly (front buffer for single-buffered contexts). Capture restores the read/draw framebuffer bindings, the default framebuffer's read selector, scissor enable, and framebuffer sRGB enable. Image creation restores texture binding. Pixel rows remain bottom-up and the consumer applies the existing orientation metadata.

Ready and consumer-release fences are imported as GL semaphores. The producer checks slot availability, reacquires external image access with a semaphore wait, performs one blit, and signals readiness. If all slots are busy, it skips capture for that swap. The native application swap still runs.

Selection operates on presenting windows, using configured indices and optional title patterns. It never observes draw calls, textures, renderbuffers, copies, or internal FBOs. Only swap and context-deletion entry points are hooked. Context-affine resources are retired on their owning context; window resize creates a new capture generation.

The previous NV/NT-handle registration investigation and keyed-mutex transports are retired. Native D3D11 and GDI also use shared fences exclusively.
