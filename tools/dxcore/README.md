# dxcore.dll for AI masking under Wine

RealityScan's AI Masking runs an ONNX model through ONNX Runtime's DirectML provider. To pick
a GPU, ONNX Runtime enumerates adapters with DXCore and asks each one
`IsAttributeSupported(DXCORE_ADAPTER_ATTRIBUTE_D3D12_GRAPHICS)`. Wine's `dxcore.dll`
(Wine 11.x, Proton, CrossOver 25) stubs that call and answers "no", so every GPU is rejected
and the model runs on the CPU: about 50–60 s per 45-megapixel image on a 24-thread laptop.

This replacement enumerates adapters through DXGI, reports the D3D12 attributes and the
usual properties (LUID, hardware IDs, memory, driver description), and also answers
`QueryInterface(IDXGIAdapter*)` with the wrapped DXGI adapter, which vkd3d-proton's
`D3D12CreateDevice` needs to select the right physical device.

Build with `make` (needs `winegcc`, which on Arch uses clang as its PE compiler), then install
into the prefix RealityScan runs in:

```bash
cp dxcore.dll "<prefix>/drive_c/windows/system32/dxcore.dll"
# through the same Wine/Proton the prefix uses:
reg.exe add 'HKCU\Software\Wine\DllOverrides' /v dxcore /t REG_SZ /d native /f
```

Based on Wine's `dlls/dxcore/dxcore.c` (LGPL 2.1 or later).
