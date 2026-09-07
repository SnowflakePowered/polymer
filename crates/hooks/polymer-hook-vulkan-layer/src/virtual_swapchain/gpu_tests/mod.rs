use super::*;
mod driver;
mod viewer;
use driver::Driver;
use std::time::Duration;

#[test]
#[ignore = "requires Windows Vulkan/D3D12 external-memory drivers"]
fn native_wsi_round_trip() {
    if std::env::var_os("POLYMER_VULKAN_WSI_TEST_CHILD").is_none() {
        for (api, profile) in [
            ("vulkan", "concurrent"),
            ("d3d12", "concurrent"),
            ("vulkan", "exclusive"),
            ("d3d12", "exclusive"),
        ] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "virtual_swapchain::gpu_tests::native_wsi_round_trip", "--ignored", "--nocapture"])
            .env("POLYMER_VULKAN_WSI_TEST_CHILD", api)
            .env("POLYMER_VULKAN_WSI_TEST_PROFILE", profile)
            .env("POLYMER_SESSION", format!("polymer-vulkan-wsi-{}", std::process::id()))
            .env("POLYMER_RENDER_CONFIGURATION", r#"{"renderMode":"swapchain","surfaces":[{"index":0,"pattern":"Virtual GPU 0","resolution":[16,16]},{"index":1,"pattern":"Virtual GPU 1","resolution":[16,16]}]}"#)
            .status().unwrap();
            assert!(status.success(), "native Vulkan WSI child failed");
        }
        return;
    }
    let session = std::env::var("POLYMER_SESSION").unwrap();
    let viewer = viewer::Viewer::new(&session, 3);
    let driver = Driver::new();
    let surface = driver.surface("Virtual GPU 0");
    let swapchain = driver.swapchain(surface, vk::SwapchainKHR::null());
    let image_count = 3;
    let mut images = vec![vk::Image::null(); image_count];
    let mut count = image_count as u32;
    unsafe {
        assert_eq!(
            wsi::images(driver.device(), swapchain, &mut count, images.as_mut_ptr()),
            vk::Result::SUCCESS
        );
    }
    let fence = unsafe {
        driver
            .native
            .create_fence(&Default::default(), None)
            .unwrap()
    };
    let mut acquired = Vec::new();
    for _ in 0..image_count {
        let mut index = u32::MAX;
        unsafe {
            assert_eq!(
                wsi::vkAcquireNextImageKHR(
                    driver.device(),
                    swapchain,
                    0,
                    vk::Semaphore::null(),
                    fence,
                    &mut index
                ),
                vk::Result::SUCCESS
            );
            driver
                .native
                .wait_for_fences(&[fence], true, 2_000_000_000)
                .unwrap();
            driver.native.reset_fences(&[fence]).unwrap();
        }
        assert!(!acquired.contains(&index));
        acquired.push(index);
    }
    let mut untouched = 0xfeed;
    unsafe {
        assert_eq!(
            wsi::vkAcquireNextImageKHR(
                driver.device(),
                swapchain,
                0,
                vk::Semaphore::null(),
                fence,
                &mut untouched
            ),
            vk::Result::NOT_READY
        );
        assert_eq!(untouched, 0xfeed);
        assert_eq!(
            wsi::vkAcquireNextImageKHR(
                driver.device(),
                swapchain,
                1_000_000,
                vk::Semaphore::null(),
                fence,
                &mut untouched
            ),
            vk::Result::TIMEOUT
        );
        assert_eq!(untouched, 0xfeed);
    }
    // Present in the reverse of acquire order, then rotate through the real
    // indices again. The viewer checks GPU pixels before acknowledging reuse.
    for (frame, index) in acquired.iter().rev().copied().enumerate() {
        driver.paint(images[index as usize], (frame % 4) as u8);
        driver.present(swapchain, index);
        viewer.frame();
    }
    for frame in 0..6 {
        let mut index = u32::MAX;
        unsafe {
            assert_eq!(
                wsi::vkAcquireNextImageKHR(
                    driver.device(),
                    swapchain,
                    2_000_000_000,
                    vk::Semaphore::null(),
                    fence,
                    &mut index
                ),
                vk::Result::SUCCESS
            );
            driver
                .native
                .wait_for_fences(&[fence], true, 2_000_000_000)
                .unwrap();
            driver.native.reset_fences(&[fence]).unwrap();
        }
        driver.paint(images[index as usize], (frame % 4) as u8);
        driver.present(swapchain, index);
        viewer.frame();
    }
    let second = driver.swapchain(driver.surface("Virtual GPU 1"), vk::SwapchainKHR::null());
    let second_images = SWAPCHAINS.get(&second).unwrap().images.clone();
    let (native, native_images) = driver.native_swapchain(driver.surface("Native GPU"));
    let handles = [second, native, swapchain];
    let rings = [&second_images, &native_images, &images];
    let mut indices = [u32::MAX; 3];
    let mut semaphores = Vec::new();
    for (ordinal, (&handle, ring)) in handles.iter().zip(rings).enumerate() {
        unsafe {
            let acquire = driver
                .native
                .create_semaphore(&Default::default(), None)
                .unwrap();
            let rendered = driver
                .native
                .create_semaphore(&Default::default(), None)
                .unwrap();
            assert_eq!(
                wsi::vkAcquireNextImageKHR(
                    driver.device(),
                    handle,
                    2_000_000_000,
                    acquire,
                    vk::Fence::null(),
                    &mut indices[ordinal]
                ),
                vk::Result::SUCCESS
            );
            driver.paint_synced(
                ring[indices[ordinal] as usize],
                2,
                if handle == native {
                    vk::ImageLayout::UNDEFINED
                } else {
                    vk::ImageLayout::PRESENT_SRC_KHR
                },
                acquire,
                rendered,
            );
            semaphores.push((acquire, rendered));
        }
    }
    let waits = semaphores
        .iter()
        .map(|(_, rendered)| *rendered)
        .collect::<Vec<_>>();
    let mut results = [vk::Result::ERROR_UNKNOWN; 3];
    let info = vk::PresentInfoKHR::default()
        .wait_semaphores(&waits)
        .swapchains(&handles)
        .image_indices(&indices)
        .results(&mut results);
    unsafe {
        assert_eq!(
            present::present(driver.owner.acquire_queue.handle, &info),
            vk::Result::SUCCESS
        );
    }
    assert_eq!(results, [vk::Result::SUCCESS; 3]);
    viewer.frame();
    viewer.frame();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        driver.owner.poll_retired();
        if driver.owner.native_presents.lock().is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native presentation fence did not finish"
        );
        std::thread::yield_now();
    }
    unsafe {
        driver
            .native
            .queue_wait_idle(driver.owner.acquire_queue.handle)
            .unwrap();
        for (acquire, rendered) in semaphores {
            driver.native.destroy_semaphore(acquire, None);
            driver.native.destroy_semaphore(rendered, None);
        }
        wsi::destroy(driver.device(), native, std::ptr::null());
        wsi::destroy(driver.device(), second, std::ptr::null());
    }
    // An old generation rejects new acquisition but can finish an image that
    // the application already owns, without publishing into the replacement.
    let mut old_index = u32::MAX;
    unsafe {
        assert_eq!(
            wsi::vkAcquireNextImageKHR(
                driver.device(),
                swapchain,
                2_000_000_000,
                vk::Semaphore::null(),
                fence,
                &mut old_index
            ),
            vk::Result::SUCCESS
        );
        driver
            .native
            .wait_for_fences(&[fence], true, 2_000_000_000)
            .unwrap();
        driver.native.reset_fences(&[fence]).unwrap();
    }
    let replacement = driver.swapchain(surface, swapchain);
    unsafe {
        assert_eq!(
            wsi::vkAcquireNextImageKHR(
                driver.device(),
                swapchain,
                0,
                vk::Semaphore::null(),
                fence,
                &mut untouched
            ),
            vk::Result::ERROR_OUT_OF_DATE_KHR
        );
    }
    driver.paint(images[old_index as usize], 3);
    driver.present(swapchain, old_index);
    let mut index = u32::MAX;
    unsafe {
        assert_eq!(
            wsi::vkAcquireNextImage2KHR(
                driver.device(),
                &vk::AcquireNextImageInfoKHR::default()
                    .swapchain(replacement)
                    .timeout(2_000_000_000)
                    .fence(fence)
                    .device_mask(1),
                &mut index
            ),
            vk::Result::SUCCESS
        );
        driver
            .native
            .wait_for_fences(&[fence], true, 2_000_000_000)
            .unwrap();
        driver.native.reset_fences(&[fence]).unwrap();
    }
    let replacement_image = SWAPCHAINS.get(&replacement).unwrap().images[index as usize];
    driver.paint(replacement_image, 2);
    driver.present(replacement, index);
    viewer.frame();
    unsafe {
        wsi::destroy(driver.device(), replacement, std::ptr::null());
    }
    unsafe {
        driver.native.destroy_fence(fence, None);
        wsi::destroy(driver.device(), swapchain, std::ptr::null());
    }
    viewer.finish();
    driver.owner.poll_retired();
    assert!(driver.owner.retired.lock().is_empty());
}
