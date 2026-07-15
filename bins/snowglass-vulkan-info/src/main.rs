use snowglass_producer_vulkan::{VulkanEnvironment, format_api_version};

fn smoke_test_device() -> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: handles are destroyed in reverse ownership order and all data
    // referenced by create structures remains live through the relevant call.
    unsafe {
        let entry = ash::Entry::load()?;
        let instance = entry.create_instance(&ash::vk::InstanceCreateInfo::default(), None)?;
        let result = (|| {
            let physical_device = instance
                .enumerate_physical_devices()?
                .into_iter()
                .next()
                .ok_or("no Vulkan physical device")?;
            let queue_family_index = instance
                .get_physical_device_queue_family_properties(physical_device)
                .iter()
                .position(|properties| properties.queue_count > 0)
                .ok_or("no Vulkan queue family")? as u32;
            let priorities = [1.0];
            let queue = ash::vk::DeviceQueueCreateInfo::default()
                .queue_family_index(queue_family_index)
                .queue_priorities(&priorities);
            let queues = [queue];
            let device_info = ash::vk::DeviceCreateInfo::default().queue_create_infos(&queues);
            let device = instance.create_device(physical_device, &device_info, None)?;
            device.destroy_device(None);
            Ok::<_, Box<dyn std::error::Error>>(())
        })();
        instance.destroy_instance(None);
        result
    }
}

fn main() {
    match VulkanEnvironment::probe() {
        Ok(environment) => {
            println!(
                "Vulkan loader: {}",
                format_api_version(environment.loader_api_version)
            );
            println!(
                "Instance extensions: {}",
                environment.instance_extensions.len()
            );
            println!("Instance layers: {}", environment.instance_layers.len());

            for (index, adapter) in environment.adapters.iter().enumerate() {
                println!("Adapter {index}: {}", adapter.name);
                println!("  API: {}", format_api_version(adapter.api_version));
                println!(
                    "  PCI: vendor={:#06x}, device={:#06x}",
                    adapter.vendor_id, adapter.device_id
                );
                println!("  LUID: {:?}", adapter.adapter_luid);
                println!(
                    "  Snowglass transport: {}",
                    if adapter.supports_capture_transport() {
                        "supported"
                    } else {
                        "missing required extensions"
                    }
                );
            }
            if let Err(error) = smoke_test_device() {
                eprintln!("Vulkan device smoke test failed: {error}");
                std::process::exit(1);
            }
            println!("Logical device creation: passed");
        }
        Err(error) => {
            eprintln!("Vulkan probe failed: {error}");
            std::process::exit(1);
        }
    }
}
