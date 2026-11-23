#include <vulkan/vulkan.h>
#include <nvsdk_ngx_helpers_vk.h>
#include <cstdint>
#include <new>

namespace {
constexpr auto project_id = "fe132ed1-fb50-4dd8-8c2f-3b87d72586fe";
constexpr auto engine_version = "0.1.0";

struct Discovery {
    const wchar_t* paths[1];
    NVSDK_NGX_FeatureCommonInfo common{};
    NVSDK_NGX_FeatureDiscoveryInfo feature{};

    Discovery(const wchar_t* runtime, const wchar_t* logs) : paths{runtime} {
        common.PathListInfo = {paths, 1};
        feature.SDKVersion = NVSDK_NGX_Version_API;
        feature.FeatureID = NVSDK_NGX_Feature_SuperSampling;
        feature.Identifier.IdentifierType = NVSDK_NGX_Application_Identifier_Type_Project_Id;
        feature.Identifier.v.ProjectDesc = {project_id, NVSDK_NGX_ENGINE_TYPE_CUSTOM, engine_version};
        feature.ApplicationDataPath = logs;
        feature.FeatureInfo = &common;
    }
};

struct Ngx {
    VkDevice device;
    Discovery discovery;
    NVSDK_NGX_Parameter* params = nullptr;
    NVSDK_NGX_Handle* feature = nullptr;
};

struct Image {
    VkImage image;
    VkImageView view;
    uint32_t format;
    uint32_t aspect;
    uint32_t width;
    uint32_t height;

    NVSDK_NGX_Resource_VK resource(bool writable) const {
        const VkImageSubresourceRange range{VkImageAspectFlags(aspect), 0, 1, 0, 1};
        return NVSDK_NGX_Create_ImageView_Resource_VK(view, image, range, VkFormat(format), width, height, writable);
    }
};
}

extern "C" uint32_t lab_ngx_extensions(
    VkInstance instance, VkPhysicalDevice physical, const wchar_t* runtime, const wchar_t* logs,
    uint32_t* count, VkExtensionProperties** properties)
{
    Discovery discovery(runtime, logs);
    if (!instance) {
        return NVSDK_NGX_VULKAN_GetFeatureInstanceExtensionRequirements(&discovery.feature, count, properties);
    }
    return NVSDK_NGX_VULKAN_GetFeatureDeviceExtensionRequirements(instance, physical, &discovery.feature, count, properties);
}

extern "C" uint32_t lab_ngx_open(
    VkInstance instance, VkPhysicalDevice physical, VkDevice device,
    PFN_vkGetInstanceProcAddr get_instance, PFN_vkGetDeviceProcAddr get_device,
    const wchar_t* runtime, const wchar_t* logs, Ngx** output)
{
    auto context = new (std::nothrow) Ngx{device, Discovery(runtime, logs)};
    if (!context) return NVSDK_NGX_Result_FAIL_OutOfGPUMemory;
    auto result = NVSDK_NGX_VULKAN_Init_with_ProjectID(
        project_id, NVSDK_NGX_ENGINE_TYPE_CUSTOM, engine_version, logs, instance, physical,
        device, get_instance, get_device, &context->discovery.common);
    if (NVSDK_NGX_FAILED(result)) {
        delete context;
        return result;
    }
    // An initialized session always belongs to the caller, including capability failures.
    *output = context;
    result = NVSDK_NGX_VULKAN_GetCapabilityParameters(&context->params);
    if (NVSDK_NGX_SUCCEED(result)) {
        int available = 0;
        result = NVSDK_NGX_Parameter_GetI(context->params, NVSDK_NGX_Parameter_SuperSampling_Available, &available);
        if (NVSDK_NGX_SUCCEED(result) && !available) result = NVSDK_NGX_Result_FAIL_FeatureNotSupported;
    }
    return result;
}

extern "C" void lab_ngx_release_feature(Ngx* context) {
    if (context->feature) {
        NVSDK_NGX_VULKAN_ReleaseFeature(context->feature);
        context->feature = nullptr;
    }
}

extern "C" void lab_ngx_close(Ngx* context) {
    lab_ngx_release_feature(context);
    if (context->params) NVSDK_NGX_VULKAN_DestroyParameters(context->params);
    NVSDK_NGX_VULKAN_Shutdown1(context->device);
    delete context;
}

extern "C" uint32_t lab_ngx_evaluate(
    Ngx* context, VkCommandBuffer cmd, const Image* images, uint32_t hdr, uint32_t reset,
    const float* jitter)
{
    const auto& [input, depth, motion, output] = *reinterpret_cast<const Image(*)[4]>(images);
    if (!context->feature) {
        const auto mode = input.width == output.width && input.height == output.height
            ? NVSDK_NGX_PerfQuality_Value_DLAA : NVSDK_NGX_PerfQuality_Value_MaxQuality;
        NVSDK_NGX_DLSS_Create_Params create{};
        create.Feature = {input.width, input.height, output.width, output.height, mode};
        create.InFeatureCreateFlags = NVSDK_NGX_DLSS_Feature_Flags_MVLowRes;
        if (hdr) {
            create.InFeatureCreateFlags |= NVSDK_NGX_DLSS_Feature_Flags_IsHDR | NVSDK_NGX_DLSS_Feature_Flags_AutoExposure;
        }
        auto result = NGX_VULKAN_CREATE_DLSS_EXT1(context->device, cmd, 1, 1, &context->feature, context->params, &create);
        if (NVSDK_NGX_FAILED(result)) return result;
    }
    auto color_resource = input.resource(false);
    auto depth_resource = depth.resource(false);
    auto motion_resource = motion.resource(false);
    auto output_resource = output.resource(true);
    NVSDK_NGX_VK_DLSS_Eval_Params eval{};
    eval.Feature.pInColor = &color_resource;
    eval.Feature.pInOutput = &output_resource;
    eval.pInDepth = &depth_resource;
    eval.pInMotionVectors = &motion_resource;
    eval.InRenderSubrectDimensions = {input.width, input.height};
    eval.InJitterOffsetX = jitter[0];
    eval.InJitterOffsetY = jitter[1];
    eval.InMVScaleX = 1.f;
    eval.InMVScaleY = 1.f;
    eval.InPreExposure = 1.f;
    eval.InExposureScale = 1.f;
    eval.InReset = reset ? 1 : 0;
    return NGX_VULKAN_EVALUATE_DLSS_EXT(cmd, context->feature, context->params, &eval);
}
