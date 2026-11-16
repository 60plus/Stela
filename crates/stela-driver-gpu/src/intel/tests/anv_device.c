/* SPDX-License-Identifier: GPL-2.0-or-later */
/* ANV on xe with no GPU: the image's route to a device, run on the host.
 * The program links ANV's objects as Mesa links its driver, Stela's libdrm
 * stand-in (compat/mesa/drm.c) in place of libdrm, and Stela's NIR options
 * hook (compat/mesa/anv_nir_options.c), and runs over the xe no-op shim
 * (intel/hosted/xe_noop_drm_shim.c) preloaded. It creates the instance and
 * the device of the one GPU the shim shows, checks what Vulkan reports of it
 * against the expected device, asks the hook for each stage's NIR options,
 * builds a compute pipeline (BRW compiles it; its statistics count the
 * instructions and code bytes), and records, submits and waits for one dispatch, which the
 * shim accepts without running it.
 *
 * Usage: anv_device DEVICE-ID integrated|discrete
 * Prints one line "ANV on xe: ..." and exits 0, or names the failed check. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <vulkan/vulkan.h>

#include <stela/host.h>

#include "anv_device_shader.h"

#define CHECK(condition)                                                                      \
	do {                                                                                  \
		if (!(condition)) {                                                           \
			fprintf(stderr, "anv_device: %s:%d: %s\n", __FILE__, __LINE__, #condition); \
			exit(1);                                                              \
		}                                                                             \
	} while (0)
#define VK(call) CHECK((call) == VK_SUCCESS)

/* Mesa's shader stages up to compute (`mesa_shader_stage`), and its count of
 * every stage (`MESA_ALL_SHADER_STAGES`), past which the hook answers none. */
#define STAGES 6
#define ALL_STAGES 15

VKAPI_ATTR PFN_vkVoidFunction VKAPI_CALL vk_icdGetInstanceProcAddr(VkInstance, const char *);
const void *stela_anv_nir_options(VkPhysicalDevice physical, unsigned stage);

static uint16_t expected_device;
/* How often the libdrm stand-in asked for the device: never would mean
 * another libdrm listed it. */
static unsigned listed;

/* What the image's Linux side answers through `stela_libc_drm_device`: the
 * one render node the shim shows, at the shim's PCI address. */
unsigned int stela_libc_drm_devices(void)
{
	return 1;
}

void stela_libc_drm_device(unsigned int index, struct stela_libc_drm_device *device)
{
	CHECK(index == 0);
	listed++;
	memset(device, 0, sizeof(*device));
	device->vendor = 0x8086;
	device->device = expected_device;
	device->slot = 2;
}

#define LOAD(instance, name) PFN_##name name = (PFN_##name)vk_icdGetInstanceProcAddr(instance, #name)

int main(int argc, char **argv)
{
	CHECK(argc == 3);
	expected_device = (uint16_t)strtoul(argv[1], NULL, 16);
	VkPhysicalDeviceType expected_type = !strcmp(argv[2], "discrete")
						     ? VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU
						     : VK_PHYSICAL_DEVICE_TYPE_INTEGRATED_GPU;

	LOAD(NULL, vkCreateInstance);
	const VkApplicationInfo application = {
		.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
		.apiVersion = VK_API_VERSION_1_3,
	};
	VkInstance instance;
	VK(vkCreateInstance(
		&(VkInstanceCreateInfo){
			.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
			.pApplicationInfo = &application,
		},
		NULL, &instance));
	LOAD(instance, vkEnumeratePhysicalDevices);
	LOAD(instance, vkGetPhysicalDeviceProperties2);
	LOAD(instance, vkGetPhysicalDeviceQueueFamilyProperties);
	LOAD(instance, vkGetPhysicalDeviceMemoryProperties);
	LOAD(instance, vkCreateDevice);
	LOAD(instance, vkGetDeviceProcAddr);
	LOAD(instance, vkDestroyInstance);

	uint32_t count = 0;
	VK(vkEnumeratePhysicalDevices(instance, &count, NULL));
	CHECK(count == 1);
	VkPhysicalDevice physical;
	VK(vkEnumeratePhysicalDevices(instance, &count, &physical));
	CHECK(listed > 0);

	VkPhysicalDeviceDriverProperties driver = {
		.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRIVER_PROPERTIES,
	};
	VkPhysicalDeviceProperties2 properties = {
		.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2,
		.pNext = &driver,
	};
	vkGetPhysicalDeviceProperties2(physical, &properties);
	CHECK(properties.properties.vendorID == 0x8086);
	CHECK(properties.properties.deviceID == expected_device);
	CHECK(properties.properties.deviceType == expected_type);
	CHECK(driver.driverID == VK_DRIVER_ID_INTEL_OPEN_SOURCE_MESA);

	/* The hook answers one set per stage, and none past Mesa's stages. */
	const void *options[STAGES];
	for (unsigned stage = 0; stage < STAGES; stage++) {
		options[stage] = stela_anv_nir_options(physical, stage);
		CHECK(options[stage]);
		for (unsigned other = 0; other < stage; other++)
			CHECK(options[other] != options[stage]);
	}
	CHECK(!stela_anv_nir_options(physical, ALL_STAGES));

	uint32_t families = 0;
	vkGetPhysicalDeviceQueueFamilyProperties(physical, &families, NULL);
	VkQueueFamilyProperties family_properties[8];
	CHECK(families && families <= 8);
	vkGetPhysicalDeviceQueueFamilyProperties(physical, &families, family_properties);
	uint32_t family = 0;
	while (family < families && !(family_properties[family].queueFlags & VK_QUEUE_COMPUTE_BIT))
		family++;
	CHECK(family < families);

	VkPhysicalDevicePipelineExecutablePropertiesFeaturesKHR executables = {
		.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PIPELINE_EXECUTABLE_PROPERTIES_FEATURES_KHR,
		.pipelineExecutableInfo = VK_TRUE,
	};
	const char *extensions[] = {VK_KHR_PIPELINE_EXECUTABLE_PROPERTIES_EXTENSION_NAME};
	VkDevice device;
	VK(vkCreateDevice(physical,
			  &(VkDeviceCreateInfo){
				  .sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
				  .pNext = &executables,
				  .queueCreateInfoCount = 1,
				  .pQueueCreateInfos =
					  &(VkDeviceQueueCreateInfo){
						  .sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
						  .queueFamilyIndex = family,
						  .queueCount = 1,
						  .pQueuePriorities = (float[]){1.0f},
					  },
				  .enabledExtensionCount = 1,
				  .ppEnabledExtensionNames = extensions,
			  },
			  NULL, &device));
#define DEVICE(name) PFN_##name name = (PFN_##name)vkGetDeviceProcAddr(device, #name)
	DEVICE(vkGetDeviceQueue);
	DEVICE(vkCreateShaderModule);
	DEVICE(vkCreateDescriptorSetLayout);
	DEVICE(vkCreatePipelineLayout);
	DEVICE(vkCreateComputePipelines);
	DEVICE(vkGetPipelineExecutablePropertiesKHR);
	DEVICE(vkGetPipelineExecutableStatisticsKHR);
	DEVICE(vkCreateBuffer);
	DEVICE(vkGetBufferMemoryRequirements);
	DEVICE(vkAllocateMemory);
	DEVICE(vkBindBufferMemory);
	DEVICE(vkMapMemory);
	DEVICE(vkCreateDescriptorPool);
	DEVICE(vkAllocateDescriptorSets);
	DEVICE(vkUpdateDescriptorSets);
	DEVICE(vkCreateCommandPool);
	DEVICE(vkAllocateCommandBuffers);
	DEVICE(vkBeginCommandBuffer);
	DEVICE(vkCmdBindPipeline);
	DEVICE(vkCmdBindDescriptorSets);
	DEVICE(vkCmdDispatch);
	DEVICE(vkEndCommandBuffer);
	DEVICE(vkCreateFence);
	DEVICE(vkQueueSubmit);
	DEVICE(vkWaitForFences);
	DEVICE(vkDeviceWaitIdle);
	DEVICE(vkDestroyFence);
	DEVICE(vkDestroyCommandPool);
	DEVICE(vkDestroyDescriptorPool);
	DEVICE(vkFreeMemory);
	DEVICE(vkDestroyBuffer);
	DEVICE(vkDestroyPipeline);
	DEVICE(vkDestroyPipelineLayout);
	DEVICE(vkDestroyDescriptorSetLayout);
	DEVICE(vkDestroyShaderModule);
	DEVICE(vkDestroyDevice);
	VkQueue queue;
	vkGetDeviceQueue(device, family, 0, &queue);

	VkShaderModule module;
	VK(vkCreateShaderModule(device,
				&(VkShaderModuleCreateInfo){
					.sType = VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
					.codeSize = sizeof(stela_anv_device_spirv),
					.pCode = stela_anv_device_spirv,
				},
				NULL, &module));
	VkDescriptorSetLayout set_layout;
	VK(vkCreateDescriptorSetLayout(
		device,
		&(VkDescriptorSetLayoutCreateInfo){
			.sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
			.bindingCount = 1,
			.pBindings =
				&(VkDescriptorSetLayoutBinding){
					.binding = 0,
					.descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
					.descriptorCount = 1,
					.stageFlags = VK_SHADER_STAGE_COMPUTE_BIT,
				},
		},
		NULL, &set_layout));
	VkPipelineLayout layout;
	VK(vkCreatePipelineLayout(device,
				  &(VkPipelineLayoutCreateInfo){
					  .sType = VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
					  .setLayoutCount = 1,
					  .pSetLayouts = &set_layout,
				  },
				  NULL, &layout));
	VkPipeline pipeline;
	VK(vkCreateComputePipelines(
		device, VK_NULL_HANDLE, 1,
		&(VkComputePipelineCreateInfo){
			.sType = VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
			.flags = VK_PIPELINE_CREATE_CAPTURE_STATISTICS_BIT_KHR,
			.stage =
				{
					.sType = VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
					.stage = VK_SHADER_STAGE_COMPUTE_BIT,
					.module = module,
					.pName = "main",
				},
			.layout = layout,
		},
		NULL, &pipeline));

	/* BRW's code: each executable's instruction count. */
	const VkPipelineInfoKHR pipeline_info = {
		.sType = VK_STRUCTURE_TYPE_PIPELINE_INFO_KHR,
		.pipeline = pipeline,
	};
	uint32_t executable_count = 0;
	VK(vkGetPipelineExecutablePropertiesKHR(device, &pipeline_info, &executable_count, NULL));
	CHECK(executable_count >= 1);
	uint64_t instructions = 0, code_bytes = 0;
	for (uint32_t executable = 0; executable < executable_count; executable++) {
		const VkPipelineExecutableInfoKHR info = {
			.sType = VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_INFO_KHR,
			.pipeline = pipeline,
			.executableIndex = executable,
		};
		VkPipelineExecutableStatisticKHR statistics[32];
		uint32_t statistic_count = 32;
		for (uint32_t i = 0; i < statistic_count; i++)
			statistics[i] = (VkPipelineExecutableStatisticKHR){
				.sType = VK_STRUCTURE_TYPE_PIPELINE_EXECUTABLE_STATISTIC_KHR,
			};
		VkResult result =
			vkGetPipelineExecutableStatisticsKHR(device, &info, &statistic_count, statistics);
		CHECK(result == VK_SUCCESS || result == VK_INCOMPLETE);
		for (uint32_t i = 0; i < statistic_count; i++) {
			if (statistics[i].format != VK_PIPELINE_EXECUTABLE_STATISTIC_FORMAT_UINT64_KHR)
				continue;
			if (!strcmp(statistics[i].name, "Instructions"))
				instructions += statistics[i].value.u64;
			else if (!strcmp(statistics[i].name, "Code size"))
				code_bytes += statistics[i].value.u64;
		}
	}
	CHECK(instructions > 0 && code_bytes > 0);

	/* One dispatch over a host-visible buffer, submitted and waited for. */
	const VkDeviceSize bytes = 64 * sizeof(uint32_t);
	VkBuffer buffer;
	VK(vkCreateBuffer(device,
			  &(VkBufferCreateInfo){
				  .sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
				  .size = bytes,
				  .usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
			  },
			  NULL, &buffer));
	VkMemoryRequirements requirements;
	vkGetBufferMemoryRequirements(device, buffer, &requirements);
	VkPhysicalDeviceMemoryProperties memory;
	vkGetPhysicalDeviceMemoryProperties(physical, &memory);
	uint32_t type = 0;
	const VkMemoryPropertyFlags visible =
		VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
	while (type < memory.memoryTypeCount &&
	       (!(requirements.memoryTypeBits & (1u << type)) ||
		(memory.memoryTypes[type].propertyFlags & visible) != visible))
		type++;
	CHECK(type < memory.memoryTypeCount);
	VkDeviceMemory allocation;
	VK(vkAllocateMemory(device,
			    &(VkMemoryAllocateInfo){
				    .sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
				    .allocationSize = requirements.size,
				    .memoryTypeIndex = type,
			    },
			    NULL, &allocation));
	VK(vkBindBufferMemory(device, buffer, allocation, 0));
	uint32_t *values;
	VK(vkMapMemory(device, allocation, 0, bytes, 0, (void **)&values));
	for (uint32_t i = 0; i < 64; i++)
		values[i] = i;

	VkDescriptorPool pool;
	VK(vkCreateDescriptorPool(device,
				  &(VkDescriptorPoolCreateInfo){
					  .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
					  .maxSets = 1,
					  .poolSizeCount = 1,
					  .pPoolSizes =
						  &(VkDescriptorPoolSize){
							  .type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
							  .descriptorCount = 1,
						  },
				  },
				  NULL, &pool));
	VkDescriptorSet set;
	VK(vkAllocateDescriptorSets(device,
				    &(VkDescriptorSetAllocateInfo){
					    .sType = VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
					    .descriptorPool = pool,
					    .descriptorSetCount = 1,
					    .pSetLayouts = &set_layout,
				    },
				    &set));
	vkUpdateDescriptorSets(device, 1,
			       &(VkWriteDescriptorSet){
				       .sType = VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
				       .dstSet = set,
				       .descriptorCount = 1,
				       .descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
				       .pBufferInfo =
					       &(VkDescriptorBufferInfo){
						       .buffer = buffer,
						       .range = bytes,
					       },
			       },
			       0, NULL);
	VkCommandPool commands;
	VK(vkCreateCommandPool(device,
			       &(VkCommandPoolCreateInfo){
				       .sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
				       .queueFamilyIndex = family,
			       },
			       NULL, &commands));
	VkCommandBuffer command_buffer;
	VK(vkAllocateCommandBuffers(device,
				    &(VkCommandBufferAllocateInfo){
					    .sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
					    .commandPool = commands,
					    .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
					    .commandBufferCount = 1,
				    },
				    &command_buffer));
	VK(vkBeginCommandBuffer(command_buffer,
				&(VkCommandBufferBeginInfo){
					.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
					.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
				}));
	vkCmdBindPipeline(command_buffer, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline);
	vkCmdBindDescriptorSets(command_buffer, VK_PIPELINE_BIND_POINT_COMPUTE, layout, 0, 1, &set,
				0, NULL);
	vkCmdDispatch(command_buffer, 1, 1, 1);
	VK(vkEndCommandBuffer(command_buffer));
	VkFence fence;
	VK(vkCreateFence(device,
			 &(VkFenceCreateInfo){
				 .sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO,
			 },
			 NULL, &fence));
	VK(vkQueueSubmit(queue, 1,
			 &(VkSubmitInfo){
				 .sType = VK_STRUCTURE_TYPE_SUBMIT_INFO,
				 .commandBufferCount = 1,
				 .pCommandBuffers = &command_buffer,
			 },
			 fence));
	VK(vkWaitForFences(device, 1, &fence, VK_TRUE, 10ull * 1000 * 1000 * 1000));
	VK(vkDeviceWaitIdle(device));

	vkDestroyFence(device, fence, NULL);
	vkDestroyCommandPool(device, commands, NULL);
	vkDestroyDescriptorPool(device, pool, NULL);
	vkDestroyBuffer(device, buffer, NULL);
	vkFreeMemory(device, allocation, NULL);
	vkDestroyPipeline(device, pipeline, NULL);
	vkDestroyPipelineLayout(device, layout, NULL);
	vkDestroyDescriptorSetLayout(device, set_layout, NULL);
	vkDestroyShaderModule(device, module, NULL);
	vkDestroyDevice(device, NULL);
	vkDestroyInstance(instance, NULL);
	printf("ANV on xe: %s (0x%04x, %s), NIR options for %u stages, compute pipeline of %llu "
	       "instructions in %llu bytes, one dispatch submitted\n",
	       properties.properties.deviceName, expected_device, argv[2], STAGES,
	       (unsigned long long)instructions, (unsigned long long)code_bytes);
	return 0;
}
