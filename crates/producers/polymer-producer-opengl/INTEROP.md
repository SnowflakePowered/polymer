# OpenGL pbuffer swapchains

OpenGL requires `renderMode: "swapchain"` and a fixed positive resolution for each requested surface. The hook installer, runtime, and pluginhost reject `intercept` for OpenGL. Only GDI retains that mode.

`WGL_ARB_pbuffer` provides an independent, driver-managed default framebuffer at the configured extent. Polymer uses the application's pixel format and context, redirects `wglMakeCurrent` and the ARB/EXT separate-read variants to the pbuffer DC, and routes presentation to that DC. Framebuffer zero remains native; application FBOs, shaders, resource bindings, and rendering calls are not intercepted.

Surface claims use the shared fixed-assignment registry with window lifetime identities and optional title patterns. Contexts using the same window/pixel format share its pbuffer. Bootstrap activation resolves the ICD's extensions; subsequent bindings use the existing pbuffer, including after unbinding. The original draw/read DC identities remain visible through WGL queries. Internal operations bypass those translations.

The HWND is initialized to the configured dimensions once to update application size caches. `GetClientRect` reports the fixed extent for a bound presentation window. Physical window resizing neither reallocates the pbuffer nor replaces the shared-image ring. Applications deriving their output size entirely from other toolkit metadata may need an equivalent drawable-size integration.

At `SwapBuffers` or main-plane `wglSwapLayerBuffers`, the producer blits the pbuffer's back buffer (front buffer for single-buffered formats) into one available EXT shared image, signals readiness, and swaps the pbuffer. The original HWND is not presented. A distinct application read DC is temporarily rebound to the presenting drawable and then restored.

The ring uses three shared D3D12 resources, `GL_EXT_memory_object`, `GL_EXT_memory_object_win32`, `GL_EXT_semaphore`, and `GL_EXT_semaphore_win32`. Framebuffer completeness is checked before publication. Consumer-release fences govern image reuse. A busy ring skips publication while the native pbuffer swap continues. There is one GPU blit per published frame, with no NV intermediate or CPU readback fallback.

Presentation restores read/draw FBO bindings, the default read selector, scissor enable, and framebuffer sRGB enable. Image creation restores texture binding. Pixel rows remain bottom-up and consumers apply the orientation metadata. GL ring cleanup executes on its owning context. The pbuffer remains alive until the last associated application context is deleted.

The selected pixel format must support pbuffer rendering; exact allocation failure rejects binding rather than substituting a smaller surface or window capture. Display-mode-induced pbuffer loss is detected; the application must recreate its context to recover. Ordinary HWND resizing does not constitute pbuffer loss.

The ignored hook regression `native_pbuffer_survives_resize_rebind_and_separate_read_dc` exercises real WGL detours, two drawable sizes, framebuffer pixels beyond a shrunken HWND, repeated unbind/rebind, separate read/draw DCs, and context deletion. Run it alone with `cargo test -p polymer-hook-opengl32 --lib -- --ignored --test-threads=1`.

Specification: https://registry.khronos.org/OpenGL/extensions/ARB/WGL_ARB_pbuffer.txt
