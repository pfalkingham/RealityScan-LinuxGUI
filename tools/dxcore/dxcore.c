/*
 * dxcore.dll replacement for running RealityScan's AI masking (ONNX Runtime + DirectML) under
 * Wine/Proton. Wine's own dxcore (as of 11.19 / Proton-CachyOS 11) enumerates adapters but
 * stubs IDXCoreAdapter::IsAttributeSupported, so ONNX Runtime's DirectML provider rejects
 * every GPU and runs the model on the CPU.
 *
 * Adapters are enumerated through DXGI (DXVK under Proton, wined3d elsewhere), the D3D12
 * attributes are reported as supported, and the adapter object also answers
 * QueryInterface(IDXGIAdapter*) with the wrapped DXGI adapter, which is what vkd3d-proton's
 * D3D12CreateDevice needs to pick the right physical device.
 *
 * Based on Wine's dlls/dxcore/dxcore.c (LGPL 2.1+, Copyright (C) 2023 Mohamad Al-Jaf).
 */

#include <stdarg.h>
#include <stdlib.h>
#include <string.h>

#define COBJMACROS
#include "initguid.h"
#include "windows.h"
#include "dxgi1_6.h"
#include "dxcore.h"

#include "wine/debug.h"

WINE_DEFAULT_DEBUG_CHANNEL(dxcore);
/* Outside the Wine tree the short names aren't defined. */
#define TRACE WINE_TRACE
#define FIXME WINE_FIXME
#define ERR WINE_ERR
#define debugstr_guid wine_dbgstr_guid

DEFINE_GUID(DXCORE_ADAPTER_ATTRIBUTE_D3D12_GENERIC_ML, 0xb71b0d41, 0x1088, 0x422f, 0xa2, 0x7c, 0x02, 0x50, 0xb7, 0xd3, 0xa9, 0x88);
DEFINE_GUID(IID_IDXGIDevice_local, 0x54ec77fa, 0x1377, 0x44e6, 0x8c, 0x32, 0x88, 0xfd, 0x5f, 0x44, 0xc8, 0x4c);

typedef HRESULT (WINAPI *PFN_CreateDXGIFactory1)(REFIID riid, void **factory);

static IDXGIFactory1 *create_dxgi_factory(void)
{
    static PFN_CreateDXGIFactory1 create;
    IDXGIFactory1 *factory = NULL;
    HRESULT hr;

    if (!create)
    {
        HMODULE dxgi = LoadLibraryW(L"dxgi.dll");
        if (!dxgi)
        {
            ERR("dxgi.dll not found.\n");
            return NULL;
        }
        create = (PFN_CreateDXGIFactory1)GetProcAddress(dxgi, "CreateDXGIFactory1");
        if (!create)
        {
            ERR("CreateDXGIFactory1 not found.\n");
            return NULL;
        }
    }
    if (FAILED(hr = create(&IID_IDXGIFactory1, (void **)&factory)))
    {
        ERR("CreateDXGIFactory1 failed, hr %#lx.\n", hr);
        return NULL;
    }
    return factory;
}

struct dxcore_adapter
{
    IDXCoreAdapter IDXCoreAdapter_iface;
    LONG refcount;

    IDXGIAdapter1 *dxgi;
    DXGI_ADAPTER_DESC1 desc;
    char description[256];
    LARGE_INTEGER driver_version;
};

static inline struct dxcore_adapter *impl_from_IDXCoreAdapter(IDXCoreAdapter *iface)
{
    return CONTAINING_RECORD(iface, struct dxcore_adapter, IDXCoreAdapter_iface);
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_QueryInterface(IDXCoreAdapter *iface, REFIID riid, void **out)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);

    TRACE("iface %p, riid %s, out %p.\n", iface, debugstr_guid(riid), out);

    if (IsEqualGUID(riid, &IID_IDXCoreAdapter) || IsEqualGUID(riid, &IID_IUnknown))
    {
        *out = &adapter->IDXCoreAdapter_iface;
        IUnknown_AddRef((IUnknown *)*out);
        return S_OK;
    }

    /* D3D12CreateDevice (vkd3d-proton) asks the adapter it is given for IDXGIAdapter. */
    if (IsEqualGUID(riid, &IID_IDXGIAdapter) || IsEqualGUID(riid, &IID_IDXGIAdapter1)
            || IsEqualGUID(riid, &IID_IDXGIAdapter2) || IsEqualGUID(riid, &IID_IDXGIAdapter3)
            || IsEqualGUID(riid, &IID_IDXGIAdapter4) || IsEqualGUID(riid, &IID_IDXGIObject))
    {
        TRACE("handing out the DXGI adapter for %s.\n", debugstr_guid(riid));
        return IDXGIAdapter1_QueryInterface(adapter->dxgi, riid, out);
    }

    FIXME("%s not implemented, returning E_NOINTERFACE.\n", debugstr_guid(riid));
    *out = NULL;
    return E_NOINTERFACE;
}

static ULONG STDMETHODCALLTYPE dxcore_adapter_AddRef(IDXCoreAdapter *iface)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);
    ULONG refcount = InterlockedIncrement(&adapter->refcount);
    TRACE("%p increasing refcount to %lu.\n", iface, refcount);
    return refcount;
}

static ULONG STDMETHODCALLTYPE dxcore_adapter_Release(IDXCoreAdapter *iface)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);
    ULONG refcount = InterlockedDecrement(&adapter->refcount);
    TRACE("%p decreasing refcount to %lu.\n", iface, refcount);
    if (!refcount)
    {
        if (adapter->dxgi)
            IDXGIAdapter1_Release(adapter->dxgi);
        free(adapter);
    }
    return refcount;
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_IsValid(IDXCoreAdapter *iface)
{
    TRACE("iface %p.\n", iface);
    return TRUE;
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_IsAttributeSupported(IDXCoreAdapter *iface, REFGUID attribute)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);
    BOOL hardware = !(adapter->desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE);
    BOOL supported;

    if (IsEqualGUID(attribute, &DXCORE_ADAPTER_ATTRIBUTE_D3D12_GRAPHICS)
            || IsEqualGUID(attribute, &DXCORE_ADAPTER_ATTRIBUTE_D3D12_CORE_COMPUTE)
            || IsEqualGUID(attribute, &DXCORE_ADAPTER_ATTRIBUTE_D3D11_GRAPHICS))
        supported = hardware;
    else
        supported = FALSE; /* GENERIC_ML (NPUs) and anything newer */

    TRACE("iface %p, attribute %s -> %d.\n", iface, debugstr_guid(attribute), supported);
    return supported;
}

static HRESULT dxcore_adapter_get_property_size(struct dxcore_adapter *adapter,
        DXCoreAdapterProperty property, size_t *size)
{
    switch (property)
    {
        case InstanceLuid: *size = sizeof(LUID); return S_OK;
        case DriverVersion: *size = sizeof(LARGE_INTEGER); return S_OK;
        case DriverDescription: *size = strlen(adapter->description) + 1; return S_OK;
        case HardwareID: *size = sizeof(DXCoreHardwareID); return S_OK;
        case HardwareIDParts: *size = sizeof(DXCoreHardwareIDParts); return S_OK;
        case DedicatedAdapterMemory:
        case DedicatedSystemMemory:
        case SharedSystemMemory: *size = sizeof(UINT64); return S_OK;
        case AcgCompatible:
        case IsHardware:
        case IsIntegrated:
        case IsDetachable: *size = sizeof(BYTE); return S_OK;
        default:
            FIXME("Property %u not implemented.\n", property);
            return DXGI_ERROR_INVALID_CALL;
    }
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_IsPropertySupported(IDXCoreAdapter *iface, DXCoreAdapterProperty property)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);
    size_t size;
    BOOL supported = SUCCEEDED(dxcore_adapter_get_property_size(adapter, property, &size));
    TRACE("iface %p, property %u -> %d.\n", iface, property, supported);
    return supported;
}

/* DXGI gives no integrated/discrete flag; a carve-out of a gigabyte or less is an iGPU. */
static BOOL adapter_is_integrated(const struct dxcore_adapter *adapter)
{
    return adapter->desc.DedicatedVideoMemory <= (1024ull * 1024 * 1024);
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_GetProperty(IDXCoreAdapter *iface, DXCoreAdapterProperty property,
        size_t buffer_size, void *buffer)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);
    size_t size;
    HRESULT hr;

    TRACE("iface %p, property %u, buffer_size %Iu, buffer %p.\n", iface, property, buffer_size, buffer);

    if (!buffer)
        return E_POINTER;
    if (FAILED(hr = dxcore_adapter_get_property_size(adapter, property, &size)))
        return hr;
    if (property == DriverDescription && buffer_size)
        *(char *)buffer = 0;
    if (buffer_size < size)
        return E_INVALIDARG;

    switch (property)
    {
        case InstanceLuid:
            *(LUID *)buffer = adapter->desc.AdapterLuid;
            break;
        case DriverVersion:
            *(LARGE_INTEGER *)buffer = adapter->driver_version;
            break;
        case DriverDescription:
            memcpy(buffer, adapter->description, size);
            break;
        case HardwareID:
        {
            DXCoreHardwareID *id = buffer;
            id->vendorID = adapter->desc.VendorId;
            id->deviceID = adapter->desc.DeviceId;
            id->subSysID = adapter->desc.SubSysId;
            id->revision = adapter->desc.Revision;
            break;
        }
        case HardwareIDParts:
        {
            DXCoreHardwareIDParts *id = buffer;
            id->vendorID = adapter->desc.VendorId;
            id->deviceID = adapter->desc.DeviceId;
            id->subSystemID = adapter->desc.SubSysId >> 16;
            id->subVendorID = adapter->desc.SubSysId & 0xffff;
            id->revisionID = adapter->desc.Revision;
            break;
        }
        case DedicatedAdapterMemory:
            *(UINT64 *)buffer = adapter->desc.DedicatedVideoMemory;
            break;
        case DedicatedSystemMemory:
            *(UINT64 *)buffer = adapter->desc.DedicatedSystemMemory;
            break;
        case SharedSystemMemory:
            *(UINT64 *)buffer = adapter->desc.SharedSystemMemory;
            break;
        case AcgCompatible:
        case IsDetachable:
            *(BYTE *)buffer = 0;
            break;
        case IsHardware:
            *(BYTE *)buffer = !(adapter->desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE);
            break;
        case IsIntegrated:
            *(BYTE *)buffer = adapter_is_integrated(adapter);
            break;
        default:
            break;
    }
    return S_OK;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_GetPropertySize(IDXCoreAdapter *iface, DXCoreAdapterProperty property,
        size_t *buffer_size)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);
    TRACE("iface %p, property %u, buffer_size %p.\n", iface, property, buffer_size);
    if (!buffer_size)
        return E_POINTER;
    return dxcore_adapter_get_property_size(adapter, property, buffer_size);
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_IsQueryStateSupported(IDXCoreAdapter *iface, DXCoreAdapterState property)
{
    TRACE("iface %p, property %u.\n", iface, property);
    return property == AdapterMemoryBudget;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_QueryState(IDXCoreAdapter *iface, DXCoreAdapterState state, size_t state_details_size,
        const void *state_details, size_t buffer_size, void *buffer)
{
    struct dxcore_adapter *adapter = impl_from_IDXCoreAdapter(iface);

    TRACE("iface %p, state %u, state_details_size %Iu, state_details %p, buffer_size %Iu, buffer %p.\n",
            iface, state, state_details_size, state_details, buffer_size, buffer);

    if (state == AdapterMemoryBudget && buffer && buffer_size >= sizeof(DXCoreAdapterMemoryBudget))
    {
        DXCoreAdapterMemoryBudget *budget = buffer;
        IDXGIAdapter3 *adapter3;
        DXGI_QUERY_VIDEO_MEMORY_INFO info;
        DXGI_MEMORY_SEGMENT_GROUP group = DXGI_MEMORY_SEGMENT_GROUP_LOCAL;

        if (state_details && state_details_size >= sizeof(DXCoreAdapterMemoryBudgetNodeSegmentGroup))
        {
            const DXCoreAdapterMemoryBudgetNodeSegmentGroup *seg = state_details;
            group = seg->segmentGroup == NonLocal ? DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL : DXGI_MEMORY_SEGMENT_GROUP_LOCAL;
        }
        memset(budget, 0, sizeof(*budget));
        if (SUCCEEDED(IDXGIAdapter1_QueryInterface(adapter->dxgi, &IID_IDXGIAdapter3, (void **)&adapter3)))
        {
            if (SUCCEEDED(IDXGIAdapter3_QueryVideoMemoryInfo(adapter3, 0, group, &info)))
            {
                budget->budget = info.Budget;
                budget->currentUsage = info.CurrentUsage;
                budget->availableForReservation = info.AvailableForReservation;
                budget->currentReservation = info.CurrentReservation;
            }
            IDXGIAdapter3_Release(adapter3);
        }
        if (!budget->budget)
            budget->budget = adapter->desc.DedicatedVideoMemory;
        return S_OK;
    }
    return E_NOTIMPL;
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_IsSetStateSupported(IDXCoreAdapter *iface, DXCoreAdapterState property)
{
    TRACE("iface %p, property %u.\n", iface, property);
    return FALSE;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_SetState(IDXCoreAdapter *iface, DXCoreAdapterState state, size_t state_details_size,
        const void *state_details, size_t buffer_size, const void *buffer)
{
    FIXME("iface %p, state %u stub!\n", iface, state);
    return E_NOTIMPL;
}

static HRESULT STDMETHODCALLTYPE DXCoreCreateAdapterFactory_impl(REFIID riid, void **out);

static HRESULT STDMETHODCALLTYPE dxcore_adapter_GetFactory(IDXCoreAdapter *iface, REFIID riid, void **out)
{
    TRACE("iface %p, riid %s, out %p.\n", iface, debugstr_guid(riid), out);
    return DXCoreCreateAdapterFactory_impl(riid, out);
}

static struct IDXCoreAdapterVtbl dxcore_adapter_vtbl =
{
    dxcore_adapter_QueryInterface,
    dxcore_adapter_AddRef,
    dxcore_adapter_Release,
    dxcore_adapter_IsValid,
    dxcore_adapter_IsAttributeSupported,
    dxcore_adapter_IsPropertySupported,
    dxcore_adapter_GetProperty,
    dxcore_adapter_GetPropertySize,
    dxcore_adapter_IsQueryStateSupported,
    dxcore_adapter_QueryState,
    dxcore_adapter_IsSetStateSupported,
    dxcore_adapter_SetState,
    dxcore_adapter_GetFactory,
};

static HRESULT dxcore_adapter_create(IDXGIAdapter1 *dxgi, struct dxcore_adapter **ret)
{
    struct dxcore_adapter *adapter;
    HRESULT hr;

    if (!(adapter = calloc(1, sizeof(*adapter))))
        return E_OUTOFMEMORY;
    adapter->IDXCoreAdapter_iface.lpVtbl = &dxcore_adapter_vtbl;
    adapter->refcount = 1;
    adapter->dxgi = dxgi;
    IDXGIAdapter1_AddRef(dxgi);
    if (FAILED(hr = IDXGIAdapter1_GetDesc1(dxgi, &adapter->desc)))
    {
        IDXCoreAdapter_Release(&adapter->IDXCoreAdapter_iface);
        return hr;
    }
    WideCharToMultiByte(CP_UTF8, 0, adapter->desc.Description, -1, adapter->description,
            sizeof(adapter->description) - 1, NULL, NULL);
    if (FAILED(IDXGIAdapter1_CheckInterfaceSupport(dxgi, &IID_IDXGIDevice_local, &adapter->driver_version)))
        adapter->driver_version.QuadPart = 0;

    TRACE("Created adapter %p: %s, vendor %#x device %#x, %I64u MiB dedicated, hardware %d.\n", adapter,
            adapter->description, adapter->desc.VendorId, adapter->desc.DeviceId,
            adapter->desc.DedicatedVideoMemory / (1024 * 1024), !(adapter->desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE));
    *ret = adapter;
    return S_OK;
}

struct dxcore_adapter_list
{
    IDXCoreAdapterList IDXCoreAdapterList_iface;
    LONG refcount;
    struct dxcore_adapter **adapters;
    uint32_t adapter_count;
};

static inline struct dxcore_adapter_list *impl_from_IDXCoreAdapterList(IDXCoreAdapterList *iface)
{
    return CONTAINING_RECORD(iface, struct dxcore_adapter_list, IDXCoreAdapterList_iface);
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_list_QueryInterface(IDXCoreAdapterList *iface, REFIID riid, void **out)
{
    struct dxcore_adapter_list *list = impl_from_IDXCoreAdapterList(iface);
    TRACE("iface %p, riid %s, out %p.\n", iface, debugstr_guid(riid), out);
    if (IsEqualGUID(riid, &IID_IDXCoreAdapterList) || IsEqualGUID(riid, &IID_IUnknown))
    {
        *out = &list->IDXCoreAdapterList_iface;
        IUnknown_AddRef((IUnknown *)*out);
        return S_OK;
    }
    FIXME("%s not implemented, returning E_NOINTERFACE.\n", debugstr_guid(riid));
    *out = NULL;
    return E_NOINTERFACE;
}

static ULONG STDMETHODCALLTYPE dxcore_adapter_list_AddRef(IDXCoreAdapterList *iface)
{
    struct dxcore_adapter_list *list = impl_from_IDXCoreAdapterList(iface);
    ULONG refcount = InterlockedIncrement(&list->refcount);
    TRACE("%p increasing refcount to %lu.\n", iface, refcount);
    return refcount;
}

static ULONG STDMETHODCALLTYPE dxcore_adapter_list_Release(IDXCoreAdapterList *iface)
{
    struct dxcore_adapter_list *list = impl_from_IDXCoreAdapterList(iface);
    ULONG refcount = InterlockedDecrement(&list->refcount);
    TRACE("%p decreasing refcount to %lu.\n", iface, refcount);
    if (!refcount)
    {
        for (UINT i = 0; i < list->adapter_count; i++)
            if (list->adapters[i])
                IDXCoreAdapter_Release(&list->adapters[i]->IDXCoreAdapter_iface);
        free(list->adapters);
        free(list);
    }
    return refcount;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_list_GetAdapter(IDXCoreAdapterList *iface, uint32_t index,
        REFIID riid, void **out)
{
    struct dxcore_adapter_list *list = impl_from_IDXCoreAdapterList(iface);
    TRACE("iface %p, index %u, riid %s, out %p.\n", iface, index, debugstr_guid(riid), out);
    if (!out)
        return E_POINTER;
    if (index >= list->adapter_count)
    {
        *out = NULL;
        return E_INVALIDARG;
    }
    return IDXCoreAdapter_QueryInterface(&list->adapters[index]->IDXCoreAdapter_iface, riid, out);
}

static uint32_t STDMETHODCALLTYPE dxcore_adapter_list_GetAdapterCount(IDXCoreAdapterList *iface)
{
    struct dxcore_adapter_list *list = impl_from_IDXCoreAdapterList(iface);
    TRACE("iface %p -> %u.\n", iface, list->adapter_count);
    return list->adapter_count;
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_list_IsStale(IDXCoreAdapterList *iface)
{
    TRACE("iface %p.\n", iface);
    return FALSE;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_list_GetFactory(IDXCoreAdapterList *iface, REFIID riid, void **out)
{
    TRACE("iface %p, riid %s, out %p.\n", iface, debugstr_guid(riid), out);
    return DXCoreCreateAdapterFactory_impl(riid, out);
}

/* Discrete, big-memory adapters first for HighPerformance; integrated first for MinimumPower. */
static int sort_high_performance(const void *a, const void *b)
{
    const struct dxcore_adapter *x = *(struct dxcore_adapter *const *)a, *y = *(struct dxcore_adapter *const *)b;
    if (adapter_is_integrated(x) != adapter_is_integrated(y))
        return adapter_is_integrated(x) ? 1 : -1;
    if (x->desc.DedicatedVideoMemory != y->desc.DedicatedVideoMemory)
        return x->desc.DedicatedVideoMemory < y->desc.DedicatedVideoMemory ? 1 : -1;
    return 0;
}

static int sort_minimum_power(const void *a, const void *b)
{
    return -sort_high_performance(a, b);
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_list_Sort(IDXCoreAdapterList *iface, uint32_t num_preferences,
        const DXCoreAdapterPreference *preferences)
{
    struct dxcore_adapter_list *list = impl_from_IDXCoreAdapterList(iface);

    TRACE("iface %p, num_preferences %u, preferences %p.\n", iface, num_preferences, preferences);

    if (!num_preferences || !preferences)
        return E_INVALIDARG;
    for (uint32_t i = 0; i < num_preferences; i++)
    {
        if (preferences[i] == HighPerformance)
            qsort(list->adapters, list->adapter_count, sizeof(*list->adapters), sort_high_performance);
        else if (preferences[i] == MinimumPower)
            qsort(list->adapters, list->adapter_count, sizeof(*list->adapters), sort_minimum_power);
        /* Hardware: software adapters are never listed. */
    }
    return S_OK;
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_list_IsAdapterPreferenceSupported(IDXCoreAdapterList *iface,
        DXCoreAdapterPreference preference)
{
    TRACE("iface %p, preference %u.\n", iface, preference);
    return preference == Hardware || preference == MinimumPower || preference == HighPerformance;
}

static struct IDXCoreAdapterListVtbl dxcore_adapter_list_vtbl =
{
    dxcore_adapter_list_QueryInterface,
    dxcore_adapter_list_AddRef,
    dxcore_adapter_list_Release,
    dxcore_adapter_list_GetAdapter,
    dxcore_adapter_list_GetAdapterCount,
    dxcore_adapter_list_IsStale,
    dxcore_adapter_list_GetFactory,
    dxcore_adapter_list_Sort,
    dxcore_adapter_list_IsAdapterPreferenceSupported,
};

struct dxcore_adapter_factory
{
    IDXCoreAdapterFactory IDXCoreAdapterFactory_iface;
    LONG refcount;
};

static inline struct dxcore_adapter_factory *impl_from_IDXCoreAdapterFactory(IDXCoreAdapterFactory *iface)
{
    return CONTAINING_RECORD(iface, struct dxcore_adapter_factory, IDXCoreAdapterFactory_iface);
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_factory_QueryInterface(IDXCoreAdapterFactory *iface, REFIID riid, void **out)
{
    struct dxcore_adapter_factory *factory = impl_from_IDXCoreAdapterFactory(iface);
    TRACE("iface %p, riid %s, out %p.\n", iface, debugstr_guid(riid), out);
    if (IsEqualGUID(riid, &IID_IDXCoreAdapterFactory) || IsEqualGUID(riid, &IID_IUnknown))
    {
        *out = &factory->IDXCoreAdapterFactory_iface;
        IUnknown_AddRef((IUnknown *)*out);
        return S_OK;
    }
    FIXME("%s not implemented, returning E_NOINTERFACE.\n", debugstr_guid(riid));
    *out = NULL;
    return E_NOINTERFACE;
}

static ULONG STDMETHODCALLTYPE dxcore_adapter_factory_AddRef(IDXCoreAdapterFactory *iface)
{
    struct dxcore_adapter_factory *factory = impl_from_IDXCoreAdapterFactory(iface);
    ULONG refcount = InterlockedIncrement(&factory->refcount);
    TRACE("%p increasing refcount to %lu.\n", iface, refcount);
    return refcount;
}

static ULONG STDMETHODCALLTYPE dxcore_adapter_factory_Release(IDXCoreAdapterFactory *iface)
{
    struct dxcore_adapter_factory *factory = impl_from_IDXCoreAdapterFactory(iface);
    ULONG refcount = InterlockedDecrement(&factory->refcount);
    TRACE("%p decreasing refcount to %lu.\n", iface, refcount);
    return refcount;
}

/* Every hardware DXGI adapter; software ones (WARP-alikes) are skipped. */
static HRESULT get_adapters(struct dxcore_adapter_list *list)
{
    IDXGIFactory1 *factory = create_dxgi_factory();
    IDXGIAdapter1 *dxgi;
    uint32_t capacity = 0;
    HRESULT hr = S_OK;

    if (!factory)
        return E_FAIL;

    for (UINT i = 0; IDXGIFactory1_EnumAdapters1(factory, i, &dxgi) == S_OK; i++)
    {
        DXGI_ADAPTER_DESC1 desc;
        struct dxcore_adapter *adapter;

        if (FAILED(IDXGIAdapter1_GetDesc1(dxgi, &desc)) || (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE))
        {
            IDXGIAdapter1_Release(dxgi);
            continue;
        }
        if (list->adapter_count == capacity)
        {
            struct dxcore_adapter **grown;
            capacity = capacity ? capacity * 2 : 4;
            if (!(grown = realloc(list->adapters, capacity * sizeof(*grown))))
            {
                IDXGIAdapter1_Release(dxgi);
                hr = E_OUTOFMEMORY;
                break;
            }
            list->adapters = grown;
        }
        hr = dxcore_adapter_create(dxgi, &adapter);
        IDXGIAdapter1_Release(dxgi);
        if (FAILED(hr))
            break;
        list->adapters[list->adapter_count++] = adapter;
    }
    IDXGIFactory1_Release(factory);
    return hr;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_factory_CreateAdapterList(IDXCoreAdapterFactory *iface, uint32_t num_attributes,
        const GUID *filter_attributes, REFIID riid, void **out)
{
    struct dxcore_adapter_list *list;
    HRESULT hr;

    TRACE("iface %p, num_attributes %u, filter_attributes %p, riid %s, out %p.\n", iface, num_attributes,
            filter_attributes, debugstr_guid(riid), out);

    if (!out)
        return E_POINTER;
    *out = NULL;
    if (!num_attributes || !filter_attributes)
        return E_INVALIDARG;
    for (uint32_t i = 0; i < num_attributes; i++)
    {
        TRACE("filter attribute %s.\n", debugstr_guid(&filter_attributes[i]));
        if (IsEqualGUID(&filter_attributes[i], &DXCORE_ADAPTER_ATTRIBUTE_D3D12_GENERIC_ML))
        {
            /* NPUs only: we have none. */
            if (!(list = calloc(1, sizeof(*list))))
                return E_OUTOFMEMORY;
            list->IDXCoreAdapterList_iface.lpVtbl = &dxcore_adapter_list_vtbl;
            list->refcount = 1;
            hr = IDXCoreAdapterList_QueryInterface(&list->IDXCoreAdapterList_iface, riid, out);
            IDXCoreAdapterList_Release(&list->IDXCoreAdapterList_iface);
            return hr;
        }
    }

    if (!(list = calloc(1, sizeof(*list))))
        return E_OUTOFMEMORY;
    list->IDXCoreAdapterList_iface.lpVtbl = &dxcore_adapter_list_vtbl;
    list->refcount = 1;
    if (FAILED(hr = get_adapters(list)))
    {
        IDXCoreAdapterList_Release(&list->IDXCoreAdapterList_iface);
        return hr;
    }
    hr = IDXCoreAdapterList_QueryInterface(&list->IDXCoreAdapterList_iface, riid, out);
    IDXCoreAdapterList_Release(&list->IDXCoreAdapterList_iface);
    TRACE("created IDXCoreAdapterList %p with %u adapters.\n", *out, list->adapter_count);
    return hr;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_factory_GetAdapterByLuid(IDXCoreAdapterFactory *iface, REFLUID adapter_luid,
        REFIID riid, void **out)
{
    IDXGIFactory1 *factory;
    IDXGIAdapter1 *dxgi;
    HRESULT hr = E_INVALIDARG;

    TRACE("iface %p, adapter_luid %p, riid %s, out %p.\n", iface, adapter_luid, debugstr_guid(riid), out);

    if (!out)
        return E_POINTER;
    *out = NULL;
    if (!(factory = create_dxgi_factory()))
        return E_FAIL;
    for (UINT i = 0; IDXGIFactory1_EnumAdapters1(factory, i, &dxgi) == S_OK; i++)
    {
        DXGI_ADAPTER_DESC1 desc;
        if (SUCCEEDED(IDXGIAdapter1_GetDesc1(dxgi, &desc)) && !memcmp(adapter_luid, &desc.AdapterLuid, sizeof(LUID)))
        {
            struct dxcore_adapter *adapter;
            if (SUCCEEDED(hr = dxcore_adapter_create(dxgi, &adapter)))
            {
                hr = IDXCoreAdapter_QueryInterface(&adapter->IDXCoreAdapter_iface, riid, out);
                IDXCoreAdapter_Release(&adapter->IDXCoreAdapter_iface);
            }
            IDXGIAdapter1_Release(dxgi);
            break;
        }
        IDXGIAdapter1_Release(dxgi);
    }
    IDXGIFactory1_Release(factory);
    return hr;
}

static BOOL STDMETHODCALLTYPE dxcore_adapter_factory_IsNotificationTypeSupported(IDXCoreAdapterFactory *iface, DXCoreNotificationType type)
{
    TRACE("iface %p, type %u.\n", iface, type);
    return FALSE;
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_factory_RegisterEventNotification(IDXCoreAdapterFactory *iface, IUnknown *dxcore_object,
        DXCoreNotificationType type, PFN_DXCORE_NOTIFICATION_CALLBACK callback, void *callback_context, uint32_t *event_cookie)
{
    FIXME("iface %p, dxcore_object %p, type %u, callback %p, callback_context %p, event_cookie %p stub!\n", iface, dxcore_object, type, callback,
            callback_context, event_cookie);
    if (event_cookie)
        *event_cookie = 1;
    return S_OK; /* never fires; keeps callers that insist on registering happy */
}

static HRESULT STDMETHODCALLTYPE dxcore_adapter_factory_UnregisterEventNotification(IDXCoreAdapterFactory *iface, uint32_t event_cookie)
{
    TRACE("iface %p, event_cookie %u.\n", iface, event_cookie);
    return S_OK;
}

static struct IDXCoreAdapterFactoryVtbl dxcore_adapter_factory_vtbl =
{
    dxcore_adapter_factory_QueryInterface,
    dxcore_adapter_factory_AddRef,
    dxcore_adapter_factory_Release,
    dxcore_adapter_factory_CreateAdapterList,
    dxcore_adapter_factory_GetAdapterByLuid,
    dxcore_adapter_factory_IsNotificationTypeSupported,
    dxcore_adapter_factory_RegisterEventNotification,
    dxcore_adapter_factory_UnregisterEventNotification,
};

static struct dxcore_adapter_factory dxcore_adapter_factory =
{
    .IDXCoreAdapterFactory_iface.lpVtbl = &dxcore_adapter_factory_vtbl,
    .refcount = 0,
};

static HRESULT STDMETHODCALLTYPE DXCoreCreateAdapterFactory_impl(REFIID riid, void **out)
{
    HRESULT hr;
    if (!out)
        return E_POINTER;
    hr = IDXCoreAdapterFactory_QueryInterface(&dxcore_adapter_factory.IDXCoreAdapterFactory_iface, riid, out);
    if (SUCCEEDED(hr))
        TRACE("returning factory %p.\n", *out);
    return hr;
}

HRESULT WINAPI DXCoreCreateAdapterFactory(REFIID riid, void **out)
{
    TRACE("riid %s, out %p.\n", debugstr_guid(riid), out);
    return DXCoreCreateAdapterFactory_impl(riid, out);
}

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, void *reserved)
{
    if (reason == DLL_PROCESS_ATTACH)
        DisableThreadLibraryCalls(instance);
    return TRUE;
}
