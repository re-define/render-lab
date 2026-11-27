// Optional DirectStorage file -> shared D3D12 resource -> Vulkan transfer source.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#define VK_USE_PLATFORM_WIN32_KHR
#include <windows.h>
#include <wrl/client.h>
#include <d3d12.h>
#include <dxgi1_4.h>
#include <dstorage.h>
#include <vulkan/vulkan.h>
#include <memory>
#include <cstdio>
using Microsoft::WRL::ComPtr;

struct Handle {
    HANDLE value = nullptr;
    ~Handle() {
        if (value)
            CloseHandle(value);
    }
};
struct Reader {
    HMODULE library = nullptr;
    ComPtr<IDStorageFactory> factory;
    ComPtr<ID3D12Device> device;
    ComPtr<IDStorageQueue> queue;
    ComPtr<IDStorageFile> file;
    ComPtr<IDStorageStatusArray> status;
    ~Reader() {
        status.Reset();
        file.Reset();
        queue.Reset();
        factory.Reset();
        device.Reset();
        if (library)
            FreeLibrary(library);
    }
};
struct Page {
    ComPtr<ID3D12Resource> resource;
    ComPtr<ID3D12Fence> ready;
    Handle memory, fence;
};
#define CHECK_HR(expr)                                                                             \
    do {                                                                                           \
        HRESULT hr = (expr);                                                                       \
        if (FAILED(hr)) {                                                                          \
            std::fprintf(stderr, "DirectStorage %s: 0x%08lx\n", #expr,                             \
                         static_cast<unsigned long>(hr));                                          \
            return hr;                                                                             \
        }                                                                                          \
    } while (0)
extern "C" HRESULT lab_storage_open(const wchar_t *dll, const wchar_t *file, LUID luid,
                                    Reader **output) {
    auto r = std::make_unique<Reader>();
    r->library = LoadLibraryExW(dll, nullptr,
                                LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!r->library)
        return HRESULT_FROM_WIN32(GetLastError());
    auto factory = reinterpret_cast<decltype(&DStorageGetFactory)>(
        GetProcAddress(r->library, "DStorageGetFactory"));
    if (!factory)
        return E_NOINTERFACE;
    CHECK_HR(factory(IID_PPV_ARGS(&r->factory)));
    ComPtr<IDXGIFactory4> dxgi;
    ComPtr<IDXGIAdapter> adapter;
    CHECK_HR(CreateDXGIFactory1(IID_PPV_ARGS(&dxgi)));
    CHECK_HR(dxgi->EnumAdapterByLuid(luid, IID_PPV_ARGS(&adapter)));
    CHECK_HR(D3D12CreateDevice(adapter.Get(), D3D_FEATURE_LEVEL_12_0, IID_PPV_ARGS(&r->device)));
    DSTORAGE_QUEUE_DESC desc = {};
    desc.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
    desc.Capacity = DSTORAGE_MIN_QUEUE_CAPACITY;
    desc.Priority = DSTORAGE_PRIORITY_NORMAL;
    desc.Device = r->device.Get();
    CHECK_HR(r->factory->OpenFile(file, IID_PPV_ARGS(&r->file)));
    CHECK_HR(r->factory->CreateQueue(&desc, IID_PPV_ARGS(&r->queue)));
    CHECK_HR(r->factory->CreateStatusArray(1, "stream file status", IID_PPV_ARGS(&r->status)));
    *output = r.release();
    return S_OK;
}
extern "C" HRESULT lab_storage_read(Reader *r, UINT64 offset, UINT32 bytes, Page **output) {
    auto p = std::make_unique<Page>();
    D3D12_HEAP_PROPERTIES heap = {};
    heap.Type = D3D12_HEAP_TYPE_DEFAULT;
    heap.CreationNodeMask = heap.VisibleNodeMask = 1;
    D3D12_RESOURCE_DESC desc = {};
    desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    desc.Width = 65536;
    desc.Height = desc.DepthOrArraySize = desc.MipLevels = desc.SampleDesc.Count = 1;
    desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    CHECK_HR(r->device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_SHARED, &desc,
                                                D3D12_RESOURCE_STATE_COMMON, nullptr,
                                                IID_PPV_ARGS(&p->resource)));
    CHECK_HR(r->device->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(&p->ready)));
    CHECK_HR(r->device->CreateSharedHandle(p->resource.Get(), nullptr, GENERIC_ALL, nullptr,
                                           &p->memory.value));
    CHECK_HR(r->device->CreateSharedHandle(p->ready.Get(), nullptr, GENERIC_ALL, nullptr,
                                           &p->fence.value));
    Handle done;
    done.value = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    if (!done.value)
        return HRESULT_FROM_WIN32(GetLastError());
    CHECK_HR(p->ready->SetEventOnCompletion(1, done.value));
    DSTORAGE_REQUEST job = {};
    job.Options.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
    job.Options.DestinationType = DSTORAGE_REQUEST_DESTINATION_BUFFER;
    job.Source.File = {r->file.Get(), offset, bytes};
    job.Destination.Buffer = {p->resource.Get(), 0, bytes};
    r->queue->EnqueueRequest(&job);
    r->queue->EnqueueStatus(r->status.Get(), 0);
    r->queue->EnqueueSignal(p->ready.Get(), 1);
    r->queue->Submit();
    if (WaitForSingleObject(done.value, INFINITE) != WAIT_OBJECT_0)
        return E_FAIL;
    CHECK_HR(r->status->GetHResult(0));
    *output = p.release();
    return S_OK;
}
extern "C" void lab_storage_close(Reader *r) { delete r; }
extern "C" void lab_storage_drop(Page *p) { delete p; }

struct Imported {
    VkDevice device;
    VkBuffer buffer = VK_NULL_HANDLE;
    VkDeviceMemory memory = VK_NULL_HANDLE;
    VkSemaphore ready = VK_NULL_HANDLE;
    ~Imported() {
        if (ready)
            vkDestroySemaphore(device, ready, nullptr);
        if (buffer)
            vkDestroyBuffer(device, buffer, nullptr);
        if (memory)
            vkFreeMemory(device, memory, nullptr);
    }
};
#define CHECK_VK(expr)                                                                             \
    do {                                                                                           \
        VkResult status = (expr);                                                                  \
        if (status != VK_SUCCESS)                                                                  \
            return status;                                                                         \
    } while (0)
extern "C" VkResult lab_storage_import(Page *page, VkDevice device, Imported **output,
                                       VkBuffer *raw, VkSemaphore *sem) {
    auto v = std::make_unique<Imported>();
    v->device = device;
    auto properties = reinterpret_cast<PFN_vkGetMemoryWin32HandlePropertiesKHR>(
        vkGetDeviceProcAddr(device, "vkGetMemoryWin32HandlePropertiesKHR"));
    auto importFence = reinterpret_cast<PFN_vkImportSemaphoreWin32HandleKHR>(
        vkGetDeviceProcAddr(device, "vkImportSemaphoreWin32HandleKHR"));
    if (!properties || !importFence)
        return VK_ERROR_EXTENSION_NOT_PRESENT;
    VkExternalMemoryBufferCreateInfo external{VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO};
    external.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT;
    VkBufferCreateInfo buffer{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
    buffer.pNext = &external;
    buffer.size = 65536;
    buffer.usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT;
    CHECK_VK(vkCreateBuffer(device, &buffer, nullptr, &v->buffer));
    VkMemoryRequirements req{};
    vkGetBufferMemoryRequirements(device, v->buffer, &req);
    VkMemoryWin32HandlePropertiesKHR prop{VK_STRUCTURE_TYPE_MEMORY_WIN32_HANDLE_PROPERTIES_KHR};
    CHECK_VK(properties(device, VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT,
                        page->memory.value, &prop));
    const auto mask = prop.memoryTypeBits & req.memoryTypeBits;
    if (!mask)
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    VkMemoryDedicatedAllocateInfo dedicated{VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO};
    dedicated.buffer = v->buffer;
    VkImportMemoryWin32HandleInfoKHR import{VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR};
    import.handle = page->memory.value;
    import.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT;
    import.pNext = &dedicated;
    VkMemoryAllocateInfo alloc{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
    alloc.pNext = &import;
    alloc.allocationSize = req.size;
    while (!(mask & (1u << alloc.memoryTypeIndex)))
        ++alloc.memoryTypeIndex;
    CHECK_VK(vkAllocateMemory(device, &alloc, nullptr, &v->memory));
    CHECK_VK(vkBindBufferMemory(device, v->buffer, v->memory, 0));
    VkSemaphoreTypeCreateInfo type{VK_STRUCTURE_TYPE_SEMAPHORE_TYPE_CREATE_INFO};
    type.semaphoreType = VK_SEMAPHORE_TYPE_TIMELINE;
    VkSemaphoreCreateInfo create{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
    create.pNext = &type;
    CHECK_VK(vkCreateSemaphore(device, &create, nullptr, &v->ready));
    VkImportSemaphoreWin32HandleInfoKHR fence{
        VK_STRUCTURE_TYPE_IMPORT_SEMAPHORE_WIN32_HANDLE_INFO_KHR};
    fence.semaphore = v->ready;
    fence.handleType = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;
    fence.handle = page->fence.value;
    CHECK_VK(importFence(device, &fence));
    *raw = v->buffer;
    *sem = v->ready;
    *output = v.release();
    return VK_SUCCESS;
}
extern "C" void lab_storage_unimport(Imported *v) { delete v; }
