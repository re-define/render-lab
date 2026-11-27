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
#include <vector>
#include <algorithm>
#include <map>
#include <mutex>
#include <cstring>
#include <cstdio>
using Microsoft::WRL::ComPtr;

struct Handle {
    HANDLE value = nullptr;
    ~Handle() {
        if (value)
            CloseHandle(value);
    }
};
// Count actual shared D3D12 slabs once, including slots between DirectStorage and Vulkan.
struct Budget {
    UINT64 used = 0, limit = 16ull << 20;
};
static std::mutex budget_lock;
static std::map<UINT64, Budget> budgets;
extern "C" UINT64 lab_storage_budget(UINT64 adapter, UINT64 limit) {
    std::lock_guard<std::mutex> lock(budget_lock);
    auto &budget = budgets[adapter];
    budget.limit = limit;
    return budget.used;
}
struct Slab {
    ComPtr<ID3D12Resource> resource;
    Handle memory;
    UINT64 bytes = 0, adapter = 0;
    ~Slab() {
        resource.Reset();
        if (bytes) {
            std::lock_guard<std::mutex> lock(budget_lock);
            budgets[adapter].used -= bytes;
        }
    }
};
struct Timeline {
    ComPtr<ID3D12Fence> fence;
    Handle shared;
};
struct Library {
    HMODULE handle = nullptr;
    ~Library() {
        if (handle)
            FreeLibrary(handle);
    }
};
struct Reader {
    std::shared_ptr<Library> library = std::make_shared<Library>();
    ComPtr<IDStorageFactory> factory;
    ComPtr<ID3D12Device> device;
    ComPtr<IDStorageQueue> queue;
    ComPtr<IDStorageFile> file;
    std::shared_ptr<Timeline> timeline = std::make_shared<Timeline>();
    std::vector<std::shared_ptr<Slab>> slabs;
    UINT64 next = 0, adapter = 0;
    ~Reader() {
        slabs.clear();
        timeline.reset();
        file.Reset();
        queue.Reset();
        factory.Reset();
        device.Reset();
    }
};
struct Batch {
    std::shared_ptr<Library> library;
    std::shared_ptr<Slab> slab;
    std::shared_ptr<Timeline> timeline;
    ComPtr<IDStorageStatusArray> status;
    UINT64 point = 0;
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
    std::memcpy(&r->adapter, &luid, sizeof(luid));
    r->library->handle = LoadLibraryExW(
        dll, nullptr, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!r->library->handle)
        return HRESULT_FROM_WIN32(GetLastError());
    auto factory = reinterpret_cast<decltype(&DStorageGetFactory)>(
        GetProcAddress(r->library->handle, "DStorageGetFactory"));
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
    CHECK_HR(r->device->CreateFence(0, D3D12_FENCE_FLAG_SHARED, IID_PPV_ARGS(&r->timeline->fence)));
    CHECK_HR(r->device->CreateSharedHandle(r->timeline->fence.Get(), nullptr, GENERIC_ALL, nullptr,
                                           &r->timeline->shared.value));
    *output = r.release();
    return S_OK;
}
static HRESULT acquire_slab(Reader *r, UINT64 bytes, std::shared_ptr<Slab> &out) {
    for (const auto &slot : r->slabs) {
        if (slot.use_count() == 1 && slot->bytes >= bytes) {
            out = slot;
            return S_OK;
        }
    }
    auto p = std::make_shared<Slab>();
    {
        std::lock_guard<std::mutex> lock(budget_lock);
        auto &budget = budgets[r->adapter];
        if (budget.used > budget.limit || bytes > budget.limit - budget.used)
            return E_OUTOFMEMORY;
        budget.used += bytes;
        p->adapter = r->adapter;
        p->bytes = bytes;
    }
    D3D12_HEAP_PROPERTIES heap = {};
    heap.Type = D3D12_HEAP_TYPE_DEFAULT;
    heap.CreationNodeMask = heap.VisibleNodeMask = 1;
    D3D12_RESOURCE_DESC desc = {};
    desc.Dimension = D3D12_RESOURCE_DIMENSION_BUFFER;
    desc.Width = bytes;
    desc.Height = desc.DepthOrArraySize = desc.MipLevels = desc.SampleDesc.Count = 1;
    desc.Layout = D3D12_TEXTURE_LAYOUT_ROW_MAJOR;
    CHECK_HR(r->device->CreateCommittedResource(&heap, D3D12_HEAP_FLAG_SHARED, &desc,
                                                D3D12_RESOURCE_STATE_COMMON, nullptr,
                                                IID_PPV_ARGS(&p->resource)));
    CHECK_HR(r->device->CreateSharedHandle(p->resource.Get(), nullptr, GENERIC_ALL, nullptr,
                                           &p->memory.value));
    r->slabs.push_back(p);
    out = std::move(p);
    return S_OK;
}
extern "C" HRESULT lab_storage_submit(Reader *r, const UINT64 *offsets, const UINT32 *sizes,
                                      UINT32 count, Batch **output, UINT64 *point,
                                      UINT64 *capacity) {
    if (!count || count > 64)
        return E_INVALIDARG;
    for (UINT32 i = 0; i < count; i++)
        if (!sizes[i] || sizes[i] > 65536)
            return E_INVALIDARG;
    auto b = std::make_unique<Batch>();
    CHECK_HR(acquire_slab(r, UINT64(count) * 65536, b->slab));
    CHECK_HR(r->factory->CreateStatusArray(1, "stream batch", IID_PPV_ARGS(&b->status)));
    b->library = r->library;
    b->timeline = r->timeline;
    b->point = ++r->next;
    for (UINT32 i = 0; i < count;) {
        // Packed, adjacent disk pages share one request. Short tails keep separate offsets.
        const auto first = i;
        auto bytes = sizes[i++];
        while (i < count && sizes[i - 1] == 65536 && offsets[i] == offsets[first] + bytes) {
            bytes += sizes[i++];
        }
        DSTORAGE_REQUEST job = {};
        job.Options.SourceType = DSTORAGE_REQUEST_SOURCE_FILE;
        job.Options.DestinationType = DSTORAGE_REQUEST_DESTINATION_BUFFER;
        job.Source.File = {r->file.Get(), offsets[first], bytes};
        job.Destination.Buffer = {b->slab->resource.Get(), UINT64(first) * 65536, bytes};
        r->queue->EnqueueRequest(&job);
    }
    r->queue->EnqueueStatus(b->status.Get(), 0);
    r->queue->EnqueueSignal(b->timeline->fence.Get(), b->point);
    r->queue->Submit();
    *point = b->point;
    *capacity = b->slab->bytes;
    *output = b.release();
    return S_OK;
}
extern "C" HRESULT lab_storage_poll(Batch *b) {
    auto value = b->timeline->fence->GetCompletedValue();
    if (value == UINT64_MAX)
        return DXGI_ERROR_DEVICE_REMOVED;
    if (value < b->point)
        return S_FALSE;
    return b->status->GetHResult(0);
}
extern "C" void lab_storage_trim(Reader *r) { r->slabs.clear(); }
extern "C" void lab_storage_close(Reader *r) { delete r; }
extern "C" void lab_storage_drop(Batch *p) { delete p; }

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
extern "C" VkResult lab_storage_import(Batch *batch, VkDevice device, Imported **output,
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
    buffer.size = batch->slab->bytes;
    buffer.usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT;
    CHECK_VK(vkCreateBuffer(device, &buffer, nullptr, &v->buffer));
    VkMemoryRequirements req{};
    vkGetBufferMemoryRequirements(device, v->buffer, &req);
    VkMemoryWin32HandlePropertiesKHR prop{VK_STRUCTURE_TYPE_MEMORY_WIN32_HANDLE_PROPERTIES_KHR};
    CHECK_VK(properties(device, VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D12_RESOURCE_BIT,
                        batch->slab->memory.value, &prop));
    const auto mask = prop.memoryTypeBits & req.memoryTypeBits;
    if (!mask)
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    VkMemoryDedicatedAllocateInfo dedicated{VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO};
    dedicated.buffer = v->buffer;
    VkImportMemoryWin32HandleInfoKHR import{VK_STRUCTURE_TYPE_IMPORT_MEMORY_WIN32_HANDLE_INFO_KHR};
    import.handle = batch->slab->memory.value;
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
    fence.handle = batch->timeline->shared.value;
    CHECK_VK(importFence(device, &fence));
    *raw = v->buffer;
    *sem = v->ready;
    *output = v.release();
    return VK_SUCCESS;
}
extern "C" void lab_storage_unimport(Imported *v) { delete v; }
