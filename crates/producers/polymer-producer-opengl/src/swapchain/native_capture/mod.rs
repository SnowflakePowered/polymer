mod renderbuffer;
use super::*;
use polymer_ipc::StreamReady;
mod readback;
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Transport {
    Pbo,
    Nv,
    Ext,
}
use crate::interop::{GL_RENDERBUFFER, glBindTexture, glDeleteTextures, glGenTextures};
use polymer_ipc::{LocalSocketListener, Message, frame::FrameSubscriber};
use polymer_protocol::{ConsumerApi, Handshake, PROTOCOL_VERSION};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Graphics::{Direct3D11::*, Dxgi::IDXGIKeyedMutex},
    },
    core::Interface,
};

pub(super) struct NativeCaptureFixture;
impl NativeCaptureFixture {
    /// The native WGL fixture supplies a current context and keeps it current
    /// until all PBO, framebuffer, and texture resources have been released.
    pub(super) unsafe fn capture(
        window: isize,
        transport: Transport,
        use_renderbuffer: bool,
        use_backbuffer: bool,
    ) {
        let name = format!("polymer-gl-capture-{}", std::process::id());
        let listener = LocalSocketListener::bind(&name).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let (done, completion) = std::sync::mpsc::channel();
        let viewer = std::thread::spawn(move || {
            let mut socket = listener.accept().unwrap();
            socket
                .send(Message::Hello(Handshake {
                    protocol_version: PROTOCOL_VERSION,
                    process_id: std::process::id(),
                    preferred_consumer: ConsumerApi::Unspecified,
                }))
                .unwrap();
            let Message::MapOffer(offer) = socket.receive().unwrap() else {
                panic!("missing frame channel")
            };
            let mut frames = FrameSubscriber::from_offer(offer).unwrap();
            let Message::StreamReady(ready) = socket.receive().unwrap() else {
                panic!("missing image export")
            };
            socket.send(Message::StreamAccepted).unwrap();
            assert!(matches!(socket.receive().unwrap(), Message::FrameAvailable));
            sender
                .send((ready, frames.latest().unwrap().unwrap()))
                .unwrap();
            completion.recv().unwrap();
        });
        let mut connection = Some(ProducerSession::connect(&name).unwrap());
        let mut capture = if transport == Transport::Pbo {
            NativeCapture::Pbo(
                unsafe {
                    PboCapture::new(SurfaceId(7), window, 16, 12, 64, 64, true, &mut connection)
                }
                .unwrap(),
            )
        } else if transport == Transport::Nv {
            NativeCapture::Nv(
                unsafe {
                    NvCapture::new(SurfaceId(7), window, 16, 12, 64, 64, true, &mut connection)
                }
                .unwrap(),
            )
        } else {
            NativeCapture::Ext(Box::new(
                unsafe {
                    ExtCapture::new(SurfaceId(7), window, 16, 12, 64, 64, true, &mut connection)
                }
                .unwrap(),
            ))
        };
        let mut pixels = vec![[0u8, 0, 255, 255]; 64 * 64];
        for y in 8..20 {
            for x in 4..20 {
                pixels[y * 64 + x] = if y < 14 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                };
            }
        }
        let mut texture = 0;
        unsafe {
            glGenTextures(1, &mut texture);
            glBindTexture(GL_TEXTURE_2D, texture);
            windows_sys::Win32::Graphics::OpenGL::glTexImage2D(
                GL_TEXTURE_2D,
                0,
                0x8058,
                64,
                64,
                0,
                0x1908,
                0x1401,
                pixels.as_ptr().cast(),
            );
        }
        unsafe { crate::tracking::Tracking::verify_binding_revalidation(texture) };
        let renderbuffer =
            use_renderbuffer.then(|| unsafe { renderbuffer::Renderbuffer::from_texture(texture) });
        let source = DrawCandidate {
            mip: 0,
            incarnation: 1,
            orientation: (false, false),
            source_origin: (4, 8),
            allocation_extent: Some((64, 64)),
            texture: renderbuffer
                .as_ref()
                .map_or(texture, |resource| resource.name),
            target: if use_renderbuffer {
                GL_RENDERBUFFER
            } else {
                GL_TEXTURE_2D
            },
            layer: 0,
            width: 16,
            height: 12,
            bounds: QuadBounds {
                x: 2,
                y: 3,
                width: 16,
                height: 12,
            },
            window_bounds: true,
            score: 192,
        };
        let mut source = renderbuffer
            .as_ref()
            .map_or(source, |resource| unsafe { resource.selected_source() });
        if use_backbuffer {
            let bind = unsafe {
                crate::interop::proc::<crate::interop::GlBindFramebuffer>(b"glBindFramebuffer\0")
            }
            .unwrap();
            unsafe {
                bind(GL_READ_FRAMEBUFFER, 0);
                bind(GL_DRAW_FRAMEBUFFER, 0);
                windows_sys::Win32::Graphics::OpenGL::glEnable(GL_SCISSOR_TEST);
                windows_sys::Win32::Graphics::OpenGL::glScissor(4, 8, 16, 6);
                windows_sys::Win32::Graphics::OpenGL::glClearColor(1.0, 0.0, 0.0, 1.0);
                windows_sys::Win32::Graphics::OpenGL::glClear(GL_COLOR_BUFFER_BIT);
                windows_sys::Win32::Graphics::OpenGL::glScissor(4, 14, 16, 6);
                windows_sys::Win32::Graphics::OpenGL::glClearColor(0.0, 1.0, 0.0, 1.0);
                windows_sys::Win32::Graphics::OpenGL::glClear(GL_COLOR_BUFFER_BIT);
                windows_sys::Win32::Graphics::OpenGL::glDisable(GL_SCISSOR_TEST);
            }
            source = DrawCandidate {
                source_origin: (4, 8),
                width: 16,
                height: 12,
                bounds: QuadBounds {
                    x: 4,
                    y: 8,
                    width: 16,
                    height: 12,
                },
                ..DrawCandidate::backbuffer(64, 64, window as u64)
            };
        }
        let start = performance_counter().unwrap();
        let initial_bounds = source.bounds;
        let initial_placement = DrawCandidate::frame_placement(
            Some(&source),
            polymer_presentation::Extent2D {
                width: 64,
                height: 64,
            },
        );
        let mut submitted = false;
        let mut publication_started = 0;
        for _ in 0..6 {
            publication_started = performance_counter().unwrap();
            let result = match &mut capture {
                NativeCapture::Pbo(capture) => unsafe {
                    capture.copy_frame(StreamId(7), Some(&source))
                },
                NativeCapture::Nv(capture) => unsafe {
                    capture.copy_frame(StreamId(7), Some(&source))
                },
                NativeCapture::Ext(capture) => unsafe {
                    capture.copy_frame(StreamId(7), Some(&source))
                },
            };
            if result.unwrap().is_some() {
                submitted = true;
                break;
            }
            source.bounds.x += 1;
            source.orientation = (true, true);
            if let NativeCapture::Pbo(capture) = &mut capture {
                capture.window_width += 8;
                capture.window_height += 8;
            }
        }
        assert!(submitted, "capture did not publish a completed frame");
        let (ready, frame) = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(
            (frame.render_width, frame.render_height, frame.frame_number),
            (16, 12, 1)
        );
        assert!(frame.timestamp_qpc >= start);
        if transport == Transport::Pbo {
            assert_eq!(
                (frame.presentation_width, frame.presentation_height),
                (64, 64)
            );
            assert_eq!(frame.presentation_transform, initial_placement.transform);
            assert_ne!(source.bounds.x, initial_bounds.x);
            assert_eq!(
                frame.capture_x, initial_bounds.x as u32,
                "delayed PBO readback lost its capture-time placement"
            );
            assert!(
                frame.timestamp_qpc < publication_started,
                "delayed PBO readback lost its capture-time timestamp"
            );
        }
        assert!(ready.bottom_up);
        {
            // Keep the first published image consumer-owned, fill the remaining
            // outputs, and verify that unavailable capacity submits no frame.
            for _ in 1..ready.slot_count {
                assert!(
                    unsafe {
                        match &mut capture {
                            NativeCapture::Nv(capture) => {
                                capture.copy_frame(StreamId(7), Some(&source))
                            }
                            NativeCapture::Ext(capture) => {
                                capture.copy_frame(StreamId(7), Some(&source))
                            }
                            NativeCapture::Pbo(capture) => {
                                capture.copy_frame(StreamId(7), Some(&source))
                            }
                        }
                    }
                    .unwrap()
                    .is_some()
                );
            }
            assert!(
                unsafe {
                    match &mut capture {
                        NativeCapture::Nv(capture) => {
                            capture.copy_frame(StreamId(7), Some(&source))
                        }
                        NativeCapture::Ext(capture) => {
                            capture.copy_frame(StreamId(7), Some(&source))
                        }
                        NativeCapture::Pbo(capture) => {
                            capture.copy_frame(StreamId(7), Some(&source))
                        }
                    }
                }
                .unwrap()
                .is_none()
            );
        }
        match &capture {
            NativeCapture::Ext(capture) => unsafe {
                // The application normally flushes at the real SwapBuffers,
                // after the capture prefix has returned.
                windows_sys::Win32::Graphics::OpenGL::glFlush();
                Self::verify_external(&capture._device, &ready, &frame);
            },
            _ => {
                let ring = match &capture {
                    NativeCapture::Pbo(capture) => &capture.transport.ring,
                    NativeCapture::Nv(capture) => &capture.transport.ring,
                    _ => unreachable!(),
                };
                let device: ID3D11Device = unsafe { ring.slots()[0].texture.GetDevice() }.unwrap();
                unsafe { Self::verify_pixels(&device, &ready, frame.slot as usize) };
            }
        }
        let retired_external = if let NativeCapture::Ext(capture) = &mut capture {
            assert!(
                unsafe { capture.copy_frame(StreamId(7), Some(&source)) }
                    .unwrap()
                    .is_some(),
                "consumer release did not make its slot available"
            );
            // Round-robin reuse follows the first slot's consumer release.
            // Preserve the receiver's descriptor and native device, then import
            // the newly submitted frame after producer object destruction.
            let mut retired_frame = frame;
            retired_frame.slot = 0;
            retired_frame.sync_value = capture.sync_value;
            unsafe { windows_sys::Win32::Graphics::OpenGL::glFlush() };
            Some((capture._device.clone(), retired_frame))
        } else {
            None
        };
        drop(capture);
        if let Some((device, frame)) = retired_external {
            unsafe { Self::verify_external(&device, &ready, &frame) };
        }
        for handle in ready.image_handles[..ready.slot_count as usize]
            .iter()
            .copied()
            .chain([
                ready.timeline_semaphore_handle,
                ready.consumer_semaphore_handle,
            ])
            .filter(|handle| *handle != 0)
        {
            unsafe { CloseHandle(HANDLE(handle as *mut _)) }.unwrap();
        }
        done.send(()).unwrap();
        viewer.join().unwrap();
        unsafe { glDeleteTextures(1, &texture) };
    }

    unsafe fn verify_pixels(device: &ID3D11Device, ready: &StreamReady, slot: usize) {
        let device1: ID3D11Device1 = device.cast().unwrap();
        let image: ID3D11Texture2D =
            unsafe { device1.OpenSharedResource1(HANDLE(ready.image_handles[slot] as *mut _)) }
                .unwrap();
        let mutex: IDXGIKeyedMutex = image.cast().unwrap();
        unsafe { mutex.AcquireSync(1, 2000) }.unwrap();
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { image.GetDesc(&mut desc) };
        desc.Usage = D3D11_USAGE_STAGING;
        desc.BindFlags = 0;
        desc.MiscFlags = 0;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        let mut staging = None;
        unsafe { device.CreateTexture2D(&desc, None, Some(&mut staging)) }.unwrap();
        let staging = staging.unwrap();
        let context = unsafe { device.GetImmediateContext() }.unwrap();
        unsafe { context.CopyResource(&staging, &image) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.unwrap();
        for y in 0..desc.Height as usize {
            let row = unsafe {
                std::slice::from_raw_parts(
                    mapped.pData.cast::<u8>().add(y * mapped.RowPitch as usize),
                    desc.Width as usize * 4,
                )
            };
            assert!(
                row.chunks_exact(4).all(|pixel| pixel
                    == if y < 6 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 255, 0, 255]
                    }),
                "captured pixels include allocation padding or the wrong crop"
            );
        }
        unsafe {
            context.Unmap(&staging, 0);
            mutex.ReleaseSync(0).unwrap();
        }
    }
}
