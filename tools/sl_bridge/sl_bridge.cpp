// victory_sl_bridge.dll: a small C interface over NVIDIA Streamline (DLSS super resolution) for VIctory's
// FidelityFX bridge (crates/victory-gpu/src/ffx.rs). Built with the official Streamline headers, so the
// structure layouts, GUIDs and versions are Streamline's own; sl.interposer.dll is loaded at run time from
// the folder given to vsl_init (signed NVIDIA binaries, never redistributed with this code).
//
// Build (x64 Native Tools prompt): see build.bat next to this file.
#include <windows.h>
#include <d3d12.h>
#include <cstdio>
#include <cstdarg>
#include <cstdint>
#include <string>

#include <sl.h>
#include <sl_consts.h>
#include <sl_dlss.h>

namespace
{
HMODULE g_interposer{};
FILE* g_log{};
bool g_ready{};
bool g_options_set{};
uint32_t g_out_w{}, g_out_h{}, g_render_w{}, g_render_h{};

PFun_slInit* p_slInit{};
PFun_slShutdown* p_slShutdown{};
PFun_slSetD3DDevice* p_slSetD3DDevice{};
PFun_slGetFeatureFunction* p_slGetFeatureFunction{};
PFun_slGetNewFrameToken* p_slGetNewFrameToken{};
PFun_slSetConstants* p_slSetConstants{};
PFun_slEvaluateFeature* p_slEvaluateFeature{};
PFun_slIsFeatureLoaded* p_slIsFeatureLoaded{};
PFun_slDLSSSetOptions* p_slDLSSSetOptions{};
PFun_slDLSSGetOptimalSettings* p_slDLSSGetOptimalSettings{};

void log(const char* fmt, ...)
{
    if (!g_log)
        return;
    SYSTEMTIME t;
    GetLocalTime(&t);
    fprintf(g_log, "%02d:%02d:%02d.%03d ", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds);
    va_list a;
    va_start(a, fmt);
    vfprintf(g_log, fmt, a);
    va_end(a);
    fputc('\n', g_log);
    fflush(g_log);
}

void sl_message(sl::LogType type, const char* msg)
{
    log("[streamline %d] %s", (int)type, msg ? msg : "");
}

template <typename T> bool get(T*& out, const char* name)
{
    out = reinterpret_cast<T*>(GetProcAddress(g_interposer, name));
    if (!out)
        log("missing export %s", name);
    return out != nullptr;
}

sl::float4x4 identity()
{
    sl::float4x4 m;
    for (uint32_t r = 0; r < 4; r++)
        m.row[r] = sl::float4(r == 0 ? 1.0f : 0.0f, r == 1 ? 1.0f : 0.0f, r == 2 ? 1.0f : 0.0f, r == 3 ? 1.0f : 0.0f);
    return m;
}

sl::DLSSMode mode_for(uint32_t render_w, uint32_t out_w)
{
    if (render_w == 0)
        return sl::DLSSMode::eMaxQuality;
    const float ratio = float(out_w) / float(render_w);
    if (ratio <= 1.05f)
        return sl::DLSSMode::eDLAA;
    if (ratio <= 1.6f)
        return sl::DLSSMode::eMaxQuality;
    if (ratio <= 1.85f)
        return sl::DLSSMode::eBalanced;
    if (ratio <= 2.5f)
        return sl::DLSSMode::eMaxPerformance;
    return sl::DLSSMode::eUltraPerformance;
}
} // namespace

extern "C" {

// The inputs of one upscale: host resources (ID3D12Resource*) and their current D3D12 states.
struct VslInputs
{
    void* color;
    uint32_t color_state;
    void* depth;
    uint32_t depth_state;
    void* motion;
    uint32_t motion_state;
    void* exposure;
    uint32_t exposure_state;
    void* output;
    uint32_t output_state;
    uint32_t render_w, render_h;
    uint32_t out_w, out_h;
    float jitter_x, jitter_y;
    float mvec_scale_x, mvec_scale_y;
    float camera_near, camera_far, camera_fov;
    float pre_exposure;
    int32_t reset;
    int32_t depth_inverted;
    int32_t hdr;
    int32_t motion_jittered;
    int32_t auto_exposure;
};

// Loads Streamline from `sl_dir` and initialises it for DLSS on `device` (an ID3D12Device*). Streamline's own
// log and this bridge's log go to `log_dir`. Returns 0 on success.
__declspec(dllexport) int vsl_init(const wchar_t* sl_dir, const wchar_t* log_dir, void* device)
{
    if (g_ready)
        return 0;
    std::wstring dir(sl_dir ? sl_dir : L".");
    std::wstring logs(log_dir ? log_dir : dir.c_str());
    CreateDirectoryW(logs.c_str(), nullptr);
    std::wstring bridge_log = logs + L"\\victory_sl_bridge.log";
    _wfopen_s(&g_log, bridge_log.c_str(), L"a");
    log("vsl_init: Streamline folder %ls, logs %ls, device %p", dir.c_str(), logs.c_str(), device);

    std::wstring interposer = dir + L"\\sl.interposer.dll";
    g_interposer = LoadLibraryExW(interposer.c_str(), nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
    if (!g_interposer)
    {
        log("sl.interposer.dll did not load (error %lu)", GetLastError());
        return 1;
    }
    if (!get(p_slInit, "slInit") || !get(p_slShutdown, "slShutdown") || !get(p_slSetD3DDevice, "slSetD3DDevice") ||
        !get(p_slGetFeatureFunction, "slGetFeatureFunction") || !get(p_slGetNewFrameToken, "slGetNewFrameToken") ||
        !get(p_slSetConstants, "slSetConstants") || !get(p_slEvaluateFeature, "slEvaluateFeature") ||
        !get(p_slIsFeatureLoaded, "slIsFeatureLoaded"))
        return 2;

    static const wchar_t* paths[1];
    paths[0] = dir.c_str();
    static std::wstring s_logs;
    s_logs = logs;
    static const sl::Feature features[] = {sl::kFeatureDLSS};
    sl::Preferences pref{};
    pref.showConsole = false;
    pref.logLevel = sl::LogLevel::eVerbose;
    pref.pathsToPlugins = paths;
    pref.numPathsToPlugins = 1;
    pref.pathToLogsAndData = s_logs.c_str();
    pref.logMessageCallback = sl_message;
    // VIctory owns its device, queues and swap chain: no proxies, no command list state tracking (the
    // bridge's caller forgets its host bindings after the upscale), no downloads.
    pref.flags = sl::PreferenceFlags::eUseManualHooking | sl::PreferenceFlags::eDisableCLStateTracking;
    pref.featuresToLoad = features;
    pref.numFeaturesToLoad = 1;
    pref.engine = sl::EngineType::eCustom;
    pref.renderAPI = sl::RenderAPI::eD3D12;
    sl::Result r = p_slInit(pref, sl::kSDKVersion);
    log("slInit: %d", (int)r);
    if (r != sl::Result::eOk)
        return 3;
    r = p_slSetD3DDevice(device);
    log("slSetD3DDevice: %d", (int)r);
    if (r != sl::Result::eOk)
        return 4;
    bool loaded = false;
    p_slIsFeatureLoaded(sl::kFeatureDLSS, loaded);
    log("DLSS feature loaded: %d", loaded ? 1 : 0);
    void* f = nullptr;
    if (p_slGetFeatureFunction(sl::kFeatureDLSS, "slDLSSSetOptions", f) == sl::Result::eOk)
        p_slDLSSSetOptions = reinterpret_cast<PFun_slDLSSSetOptions*>(f);
    f = nullptr;
    if (p_slGetFeatureFunction(sl::kFeatureDLSS, "slDLSSGetOptimalSettings", f) == sl::Result::eOk)
        p_slDLSSGetOptimalSettings = reinterpret_cast<PFun_slDLSSGetOptimalSettings*>(f);
    log("slDLSSSetOptions %p, slDLSSGetOptimalSettings %p", (void*)p_slDLSSSetOptions, (void*)p_slDLSSGetOptimalSettings);
    if (!loaded || !p_slDLSSSetOptions)
        return 5;
    g_ready = true;
    return 0;
}

// Records DLSS on `command_list` (an open ID3D12GraphicsCommandList*). Returns 0 on success.
__declspec(dllexport) int vsl_evaluate(void* command_list, const VslInputs* in)
{
    if (!g_ready || !in || !command_list)
        return 10;
    sl::ViewportHandle viewport(0);
    if (!g_options_set || in->out_w != g_out_w || in->out_h != g_out_h || in->render_w != g_render_w ||
        in->render_h != g_render_h)
    {
        sl::DLSSOptions o{};
        o.mode = mode_for(in->render_w, in->out_w);
        o.outputWidth = in->out_w;
        o.outputHeight = in->out_h;
        o.colorBuffersHDR = in->hdr ? sl::Boolean::eTrue : sl::Boolean::eFalse;
        o.useAutoExposure = (in->auto_exposure || !in->exposure) ? sl::Boolean::eTrue : sl::Boolean::eFalse;
        o.preExposure = in->pre_exposure > 0.0f ? in->pre_exposure : 1.0f;
        sl::Result r = p_slDLSSSetOptions(viewport, o);
        log("slDLSSSetOptions: %d (mode %u, render %ux%u, output %ux%u, hdr %d, auto exposure %d)", (int)r, (unsigned)o.mode,
            in->render_w, in->render_h, in->out_w, in->out_h, in->hdr, (int)o.useAutoExposure);
        if (r != sl::Result::eOk)
            return 11;
        g_options_set = true;
        g_out_w = in->out_w;
        g_out_h = in->out_h;
        g_render_w = in->render_w;
        g_render_h = in->render_h;
    }
    sl::FrameToken* token = nullptr;
    sl::Result r = p_slGetNewFrameToken(token, nullptr);
    if (r != sl::Result::eOk || !token)
    {
        log("slGetNewFrameToken: %d", (int)r);
        return 12;
    }
    sl::Constants c{};
    c.cameraViewToClip = identity();
    c.clipToCameraView = identity();
    c.clipToLensClip = identity();
    c.clipToPrevClip = identity();
    c.prevClipToClip = identity();
    c.jitterOffset = sl::float2(in->jitter_x, in->jitter_y);
    c.mvecScale = sl::float2(in->mvec_scale_x, in->mvec_scale_y);
    c.cameraPinholeOffset = sl::float2(0.0f, 0.0f);
    c.cameraPos = sl::float3(0.0f, 0.0f, 0.0f);
    c.cameraUp = sl::float3(0.0f, 1.0f, 0.0f);
    c.cameraRight = sl::float3(1.0f, 0.0f, 0.0f);
    c.cameraFwd = sl::float3(0.0f, 0.0f, 1.0f);
    c.cameraNear = in->camera_near;
    c.cameraFar = in->camera_far;
    c.cameraFOV = in->camera_fov;
    c.cameraAspectRatio = in->render_h ? float(in->render_w) / float(in->render_h) : 1.0f;
    c.depthInverted = in->depth_inverted ? sl::Boolean::eTrue : sl::Boolean::eFalse;
    c.cameraMotionIncluded = sl::Boolean::eTrue;
    c.motionVectors3D = sl::Boolean::eFalse;
    c.reset = in->reset ? sl::Boolean::eTrue : sl::Boolean::eFalse;
    c.motionVectorsJittered = in->motion_jittered ? sl::Boolean::eTrue : sl::Boolean::eFalse;
    r = p_slSetConstants(c, *token, viewport);
    if (r != sl::Result::eOk)
    {
        log("slSetConstants: %d", (int)r);
        return 13;
    }
    sl::Extent render{0, 0, in->render_w, in->render_h};
    sl::Extent out{0, 0, in->out_w, in->out_h};
    sl::Extent one{0, 0, 1, 1};
    sl::Resource color(sl::ResourceType::eTex2d, in->color, nullptr, nullptr, in->color_state);
    sl::Resource depth(sl::ResourceType::eTex2d, in->depth, nullptr, nullptr, in->depth_state);
    sl::Resource motion(sl::ResourceType::eTex2d, in->motion, nullptr, nullptr, in->motion_state);
    sl::Resource output(sl::ResourceType::eTex2d, in->output, nullptr, nullptr, in->output_state);
    sl::Resource exposure(sl::ResourceType::eTex2d, in->exposure, nullptr, nullptr, in->exposure_state);
    sl::ResourceTag t_color(&color, sl::kBufferTypeScalingInputColor, sl::ResourceLifecycle::eOnlyValidNow, &render);
    sl::ResourceTag t_depth(&depth, sl::kBufferTypeDepth, sl::ResourceLifecycle::eOnlyValidNow, &render);
    sl::ResourceTag t_motion(&motion, sl::kBufferTypeMotionVectors, sl::ResourceLifecycle::eOnlyValidNow, &render);
    sl::ResourceTag t_output(&output, sl::kBufferTypeScalingOutputColor, sl::ResourceLifecycle::eOnlyValidNow, &out);
    sl::ResourceTag t_exposure(&exposure, sl::kBufferTypeExposure, sl::ResourceLifecycle::eOnlyValidNow, &one);
    const sl::BaseStructure* inputs[6];
    uint32_t n = 0;
    inputs[n++] = &viewport;
    inputs[n++] = &t_color;
    inputs[n++] = &t_depth;
    inputs[n++] = &t_motion;
    inputs[n++] = &t_output;
    if (in->exposure && !in->auto_exposure)
        inputs[n++] = &t_exposure;
    r = p_slEvaluateFeature(sl::kFeatureDLSS, *token, inputs, n, command_list);
    static int count = 0;
    if (count < 5 || r != sl::Result::eOk)
        log("slEvaluateFeature: %d (frame %u, render %ux%u, output %ux%u, jitter %.3f %.3f, mvec scale %.5f %.5f, reset %d)",
            (int)r, (uint32_t)*token, in->render_w, in->render_h, in->out_w, in->out_h, in->jitter_x, in->jitter_y,
            in->mvec_scale_x, in->mvec_scale_y, in->reset);
    count++;
    return r == sl::Result::eOk ? 0 : 14;
}

__declspec(dllexport) void vsl_shutdown()
{
    if (g_ready && p_slShutdown)
        log("slShutdown: %d", (int)p_slShutdown());
    g_ready = false;
    if (g_log)
        fclose(g_log);
    g_log = nullptr;
}

} // extern "C"
