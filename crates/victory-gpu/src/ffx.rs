//! The title's FidelityFX upscaler calls answered by AMD's PC FidelityFX
//! loader for D3D12, `amd_fidelityfx_loader_dx12.dll` from the public
//! FidelityFX SDK (2.x, `Kits/FidelityFX/signedbin`), in the folder
//! `VICTORY_FFX_DIR` names.
//!
//! The console's FidelityFX API has the PC API's structure layouts. Its
//! upscaler DLL only carries the fixed 1920x1080 Quality tables: a Series X
//! configuration made to pick it (3840x2160 output) gets "maxUpscaleSize
//! must be 1920x1080" and the title stops. Here the title's descriptions go
//! to the PC loader with the console objects swapped for the host's: the
//! console backend description for the DX12 one on the host device, the
//! command list for the host list behind the title's, and every texture for
//! its host copy, moved to the state the upscaler reads (inputs) or writes
//! (output) on entry. The PC upscaler leaves them in those states.
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{Interface, PCSTR, PCWSTR};

use crate::object::This;

type ReturnCode = u32;
const OK: ReturnCode = 0;
const ERROR: ReturnCode = 1;
const ERROR_PARAMETER: ReturnCode = 6;

/// `ffxApiHeader`: a description's type and the next in its chain.
#[repr(C)]
pub struct Header {
    kind: u64,
    next: *mut Header,
}

const CREATE_UPSCALE: u64 = 0x0001_0000;
const DISPATCH_UPSCALE: u64 = 0x0001_0001;
const QUERY_GET_VERSIONS: u64 = 4;
const QUERY_JITTER_PHASE_COUNT: u64 = 0x0001_0004;
/// The console's backend description; the device is at +0x10, as in the
/// PC's DX12 one.
const CONSOLE_BACKEND: u64 = 0x0100_0002;
const BACKEND_DX12: u64 = 2;

/// `ffxCreateBackendDX12Desc`.
#[repr(C)]
struct BackendDx12 {
    header: Header,
    device: *mut c_void,
}

/// `ffxCreateContextDescUpscale`.
#[repr(C)]
#[derive(Clone, Copy)]
struct CreateUpscale {
    kind: u64,
    next: *mut Header,
    flags: u32,
    max_render: [u32; 2],
    max_upscale: [u32; 2],
    message: *const c_void,
}
const _: () = assert!(size_of::<CreateUpscale>() == 48);

/// `FfxApiResource`: the resource, its description, its state.
#[repr(C)]
#[derive(Clone, Copy)]
struct Resource {
    resource: *mut c_void,
    description: [u32; 8],
    state: u32,
}
const _: () = assert!(size_of::<Resource>() == 48);

/// `ffxDispatchDescUpscale`.
#[repr(C)]
#[derive(Clone, Copy)]
struct DispatchUpscale {
    kind: u64,
    next: *mut Header,
    command_list: *mut c_void,
    /// Color, depth, motion vectors, exposure, reactive, transparency and
    /// composition, output.
    resources: [Resource; 7],
    jitter: [f32; 2],
    motion_vector_scale: [f32; 2],
    render_size: [u32; 2],
    upscale_size: [u32; 2],
    sharpening: bool,
    sharpness: f32,
    frame_time_delta: f32,
    pre_exposure: f32,
    reset: bool,
    camera: [f32; 4],
    flags: u32,
}
const _: () = assert!(size_of::<DispatchUpscale>() == 432);
const OUTPUT: usize = 6;

/// `ffxQueryDescGetVersions`.
#[repr(C)]
#[derive(Clone, Copy)]
struct GetVersions {
    kind: u64,
    next: *mut Header,
    create_kind: u64,
    device: *mut c_void,
    count: *mut u64,
    ids: *mut u64,
    names: *mut *const i8,
}

/// FfxApiResourceState bits and the host states they stand for.
const FFX_UNORDERED_ACCESS: u32 = 1 << 1;
const FFX_COMPUTE_READ: u32 = 1 << 2;
const HOST_UNORDERED_ACCESS: u32 = 0x8;
const HOST_NON_PIXEL_SHADER_RESOURCE: u32 = 0x40;

type Farproc = unsafe extern "system" fn() -> isize;
type Create = unsafe extern "C" fn(*mut *mut c_void, *mut Header, *const c_void) -> ReturnCode;
type Destroy = unsafe extern "C" fn(*mut *mut c_void, *const c_void) -> ReturnCode;
type Configure = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> ReturnCode;
type Query = unsafe extern "C" fn(*mut *mut c_void, *mut Header) -> ReturnCode;
type Dispatch = unsafe extern "C" fn(*mut *mut c_void, *const Header) -> ReturnCode;

struct Api {
    create: Create,
    destroy: Destroy,
    configure: Configure,
    query: Query,
    dispatch: Dispatch,
}

/// Whether the title's FidelityFX calls come here: `VICTORY_FFX_DIR` set.
pub fn enabled() -> bool {
    std::env::var_os("VICTORY_FFX_DIR").is_some()
}

/// AMD's loader, loaded on first use. The upscaler it picks is loaded by
/// full path first, so the loader finds it without a change to the DLL
/// search path.
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        let dir = std::path::PathBuf::from(std::env::var_os("VICTORY_FFX_DIR")?);
        let load = |name: &str| {
            let path: Vec<u16> = dir.join(name).as_os_str().encode_wide_nul();
            // SAFETY: a nul-terminated path.
            unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) }.ok()
        };
        // `VICTORY_FFX_PRELOAD=<dll>`: a module loaded before AMD's, e.g.
        // OptiScaler, which hooks the loader as it loads and answers the
        // upscale with another upscaler (DLSS on NVIDIA).
        if let Some(pre) = std::env::var_os("VICTORY_FFX_PRELOAD") {
            let path: Vec<u16> = pre.as_os_str().encode_wide_nul();
            // SAFETY: a nul-terminated path.
            match unsafe { LoadLibraryW(PCWSTR(path.as_ptr())) } {
                Ok(_) => tracing::info!(?pre, "ffx: preloaded before AMD's loader"),
                Err(e) => tracing::error!(?pre, error = %e, "ffx: VICTORY_FFX_PRELOAD did not load"),
            }
        }
        let _ = load("amd_fidelityfx_upscaler_dx12.dll");
        let Some(loader) = load("amd_fidelityfx_loader_dx12.dll") else {
            tracing::error!(?dir, "ffx: amd_fidelityfx_loader_dx12.dll not found; set VICTORY_FFX_DIR to the SDK's signedbin");
            return None;
        };
        let get = |name: &[u8]| {
            // SAFETY: a loaded module and a nul-terminated name.
            let f = unsafe { GetProcAddress(loader, PCSTR(name.as_ptr())) };
            if f.is_none() {
                // Without it every FidelityFX call fails, and a title that
                // divides by what a query returns stops there: say so.
                let export = String::from_utf8_lossy(&name[..name.len() - 1]);
                tracing::error!(?dir, %export, "ffx: the loader does not export this; FidelityFX calls will fail");
            }
            f
        };
        // SAFETY: the SDK's exports, of these types (`ffx_api.h`).
        unsafe {
            Some(Api {
                create: std::mem::transmute::<Farproc, Create>(get(b"ffxCreateContext\0")?),
                destroy: std::mem::transmute::<Farproc, Destroy>(get(b"ffxDestroyContext\0")?),
                configure: std::mem::transmute::<Farproc, Configure>(get(b"ffxConfigure\0")?),
                query: std::mem::transmute::<Farproc, Query>(get(b"ffxQuery\0")?),
                dispatch: std::mem::transmute::<Farproc, Dispatch>(get(b"ffxDispatch\0")?),
            })
        }
    })
    .as_ref()
}

trait WideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl WideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

fn host_device() -> Option<*mut c_void> {
    crate::host::host().map(|h| h.device.as_raw())
}

/// Logs a description type the first time it is seen.
fn note(call: &str, kind: u64) {
    static SEEN: std::sync::Mutex<Vec<(String, u64)>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.iter().any(|(c, k)| c == call && *k == kind) {
        seen.push((call.to_owned(), kind));
        tracing::info!("ffx: {call} with description type {kind:#x}");
    }
}

/// The upscale description in a chain that also holds the console backend
/// description, the one this module stands in for.
///
/// # Safety
///
/// `h` is null or the start of a chain of valid descriptions.
unsafe fn upscale_in(mut h: *const Header) -> Option<CreateUpscale> {
    let (mut upscale, mut backend) = (None, false);
    while !h.is_null() {
        // SAFETY: the caller's chain.
        let kind = unsafe { (*h).kind };
        match kind {
            // SAFETY: a description of this type.
            CREATE_UPSCALE => upscale = Some(unsafe { *h.cast::<CreateUpscale>() }),
            CONSOLE_BACKEND => backend = true,
            _ => note("ffxCreateContext", kind),
        }
        // SAFETY: as above.
        h = unsafe { (*h).next };
    }
    upscale.filter(|_| backend)
}

pub extern "C" fn ffxCreateContext(
    context: *mut *mut c_void,
    desc: *mut Header,
    _memory: *const c_void,
) -> ReturnCode {
    let (Some(api), Some(device)) = (api(), host_device()) else {
        return ERROR;
    };
    if context.is_null() || desc.is_null() {
        return ERROR_PARAMETER;
    }
    // SAFETY: the title's chain of descriptions.
    let Some(mut upscale) = (unsafe { upscale_in(desc) }) else {
        return ERROR_PARAMETER;
    };
    // The console's version and provider descriptions name console builds;
    // the PC loader picks the newest upscaler the GPU runs.
    let mut dx12 = BackendDx12 {
        header: Header {
            kind: BACKEND_DX12,
            next: std::ptr::null_mut(),
        },
        device,
    };
    upscale.next = (&raw mut dx12).cast();
    // SAFETY: the SDK's call with a chain that outlives it.
    CREATE_FLAGS.store(upscale.flags, std::sync::atomic::Ordering::Relaxed);
    let r = unsafe { (api.create)(context, (&raw mut upscale).cast(), std::ptr::null()) };
    let provider = provider_name(context);
    crate::host::set_caption_note(format!(
        "AMD FidelityFX FSR {provider} {}x{}\u{2192}{}x{}",
        upscale.max_render[0], upscale.max_render[1], upscale.max_upscale[0], upscale.max_upscale[1]
    ));
    tracing::info!(
        render = ?upscale.max_render, upscale = ?upscale.max_upscale, flags = upscale.flags, result = r, provider,
        "ffx: upscale context on the host GPU"
    );
    r
}

/// The upscaler version the loader chose for a context, for the log.
fn provider_name(context: *mut *mut c_void) -> String {
    #[repr(C)]
    struct ProviderVersion {
        kind: u64,
        next: *mut Header,
        id: u64,
        name: *const i8,
    }
    let Some(api) = api() else {
        return String::new();
    };
    let mut q = ProviderVersion {
        kind: 6,
        next: std::ptr::null_mut(),
        id: 0,
        name: std::ptr::null(),
    };
    // SAFETY: the context pointer the title passed, checked for null.
    if context.is_null() || unsafe { (*context).is_null() } {
        return String::new();
    }
    // SAFETY: a context the loader created.
    if unsafe { (api.query)(context, (&raw mut q).cast()) } != OK || q.name.is_null() {
        return String::new();
    }
    // SAFETY: the loader's nul-terminated name.
    unsafe { std::ffi::CStr::from_ptr(q.name) }
        .to_string_lossy()
        .into_owned()
}

pub extern "C" fn ffxDestroyContext(
    context: *mut *mut c_void,
    _memory: *const c_void,
) -> ReturnCode {
    let Some(api) = api() else { return ERROR };
    // SAFETY: a context this module created.
    unsafe { (api.destroy)(context, std::ptr::null()) }
}

pub extern "C" fn ffxConfigure(context: *mut *mut c_void, desc: *const Header) -> ReturnCode {
    let Some(api) = api() else { return ERROR };
    if !desc.is_null() {
        // SAFETY: the title's description.
        note("ffxConfigure", unsafe { (*desc).kind });
    }
    // SAFETY: passed on as the title made it.
    unsafe { (api.configure)(context, desc) }
}

pub extern "C" fn ffxQuery(context: *mut *mut c_void, desc: *mut Header) -> ReturnCode {
    let (Some(api), Some(device)) = (api(), host_device()) else {
        return ERROR;
    };
    if desc.is_null() {
        return ERROR_PARAMETER;
    }
    // SAFETY: the title's description.
    let kind = unsafe { (*desc).kind };
    note("ffxQuery", kind);
    if kind == QUERY_GET_VERSIONS {
        // It names the device to enumerate for: the host's.
        // SAFETY: a description of this type.
        let mut q = unsafe { *desc.cast::<GetVersions>() };
        q.device = device;
        // SAFETY: the SDK's call; the title's output arrays stay its own.
        return unsafe { (api.query)(context, (&raw mut q).cast()) };
    }
    // SAFETY: a null context pointer or one this module filled.
    let contextless = context.is_null() || unsafe { (*context).is_null() };
    if !contextless {
        // SAFETY: passed on with the PC context.
        let r = unsafe { (api.query)(context, desc) };
        log_query(kind, desc, r);
        return r;
    }
    // Without a context the loader learns the backend from the chain: the
    // DX12 one is added at its end for the call, then taken off.
    let mut dx12 = BackendDx12 {
        header: Header {
            kind: BACKEND_DX12,
            next: std::ptr::null_mut(),
        },
        device,
    };
    let mut last = desc;
    // SAFETY: the title's chain.
    while !unsafe { (*last).next }.is_null() {
        // SAFETY: as above.
        last = unsafe { (*last).next };
    }
    // SAFETY: the chain's end, restored before returning.
    unsafe { (*last).next = (&raw mut dx12).cast() };
    // SAFETY: the title's description with the backend chained for the call.
    let r = unsafe { (api.query)(context, desc) };
    // SAFETY: the chain's end again, as the title made it.
    unsafe { (*last).next = std::ptr::null_mut() };
    log_query(kind, desc, r);
    r
}

/// A query's result, once per type, with the jitter phase count it gave.
fn log_query(kind: u64, desc: *mut Header, r: ReturnCode) {
    static SEEN: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.contains(&kind) {
        return;
    }
    seen.push(kind);
    #[repr(C)]
    struct JitterPhaseCount {
        kind: u64,
        next: *mut Header,
        render_width: u32,
        display_width: u32,
        out: *mut i32,
    }
    if kind == QUERY_JITTER_PHASE_COUNT {
        // SAFETY: a description of this type.
        let q = unsafe { &*desc.cast::<JitterPhaseCount>() };
        // SAFETY: the title's output, written by the call when it succeeded.
        let count = (r == OK && !q.out.is_null()).then(|| unsafe { *q.out });
        tracing::info!(
            r,
            q.render_width,
            q.display_width,
            ?count,
            "ffx: jitter phase count"
        );
    } else {
        tracing::info!(r, "ffx: query {kind:#x}");
    }
}

/// The flags the title created its upscale context with (`FFX_UPSCALE_ENABLE_*`).
static CREATE_FLAGS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const FFX_HDR: u32 = 1 << 0;
const FFX_DISPLAY_RESOLUTION_MOTION_VECTORS: u32 = 1 << 1;
const FFX_MOTION_VECTORS_JITTER_CANCELLATION: u32 = 1 << 2;
const FFX_DEPTH_INVERTED: u32 = 1 << 3;
const FFX_AUTO_EXPOSURE: u32 = 1 << 5;

/// `VslInputs` of `tools/sl_bridge/sl_bridge.cpp`.
#[repr(C)]
struct VslInputs {
    color: *mut c_void,
    color_state: u32,
    depth: *mut c_void,
    depth_state: u32,
    motion: *mut c_void,
    motion_state: u32,
    exposure: *mut c_void,
    exposure_state: u32,
    output: *mut c_void,
    output_state: u32,
    render: [u32; 2],
    out: [u32; 2],
    jitter: [f32; 2],
    mvec_scale: [f32; 2],
    camera_near: f32,
    camera_far: f32,
    camera_fov: f32,
    pre_exposure: f32,
    reset: i32,
    depth_inverted: i32,
    hdr: i32,
    motion_jittered: i32,
    auto_exposure: i32,
}

type VslInit = unsafe extern "C" fn(*const u16, *const u16, *mut c_void) -> i32;
type VslEvaluate = unsafe extern "C" fn(*mut c_void, *const VslInputs) -> i32;

/// DLSS through NVIDIA Streamline (`VICTORY_DLSS_DIR=<folder>` with `victory_sl_bridge.dll` beside
/// Streamline's signed `sl.interposer.dll`, `sl.common.dll`, `sl.dlss.dll` and `nvngx_dlss.dll`): the
/// title's upscale goes to DLSS instead of AMD's upscaler, whose context still answers creation and the
/// jitter queries. Streamline's log and the bridge's go to `VICTORY_DLSS_LOGS` (default: the folder).
struct Dlss {
    evaluate: VslEvaluate,
    /// Streamline's and DLSS's versions, for the window title.
    versions: String,
}

fn dlss() -> Option<&'static Dlss> {
    static DLSS: OnceLock<Option<Dlss>> = OnceLock::new();
    DLSS.get_or_init(|| {
        let dir = std::path::PathBuf::from(std::env::var_os("VICTORY_DLSS_DIR")?);
        let logs = std::env::var_os("VICTORY_DLSS_LOGS").map_or_else(|| dir.clone(), std::path::PathBuf::from);
        let bridge: Vec<u16> = dir.join("victory_sl_bridge.dll").as_os_str().encode_wide_nul();
        // SAFETY: a nul-terminated path.
        let module = match unsafe { LoadLibraryW(PCWSTR(bridge.as_ptr())) } {
            Ok(m) => m,
            Err(e) => {
                tracing::error!(?dir, error = %e, "dlss: victory_sl_bridge.dll did not load");
                return None;
            }
        };
        // SAFETY: the bridge's exports, of these types.
        let (init, evaluate) = unsafe {
            let init = GetProcAddress(module, PCSTR(b"vsl_init\0".as_ptr()));
            let evaluate = GetProcAddress(module, PCSTR(b"vsl_evaluate\0".as_ptr()));
            match (init, evaluate) {
                (Some(i), Some(e)) => (
                    std::mem::transmute::<Farproc, VslInit>(i),
                    std::mem::transmute::<Farproc, VslEvaluate>(e),
                ),
                _ => {
                    tracing::error!("dlss: the bridge lacks vsl_init or vsl_evaluate");
                    return None;
                }
            }
        };
        let device = host_device()?;
        let dir_w: Vec<u16> = dir.as_os_str().encode_wide_nul();
        let logs_w: Vec<u16> = logs.as_os_str().encode_wide_nul();
        // SAFETY: nul-terminated paths and the live host device.
        let r = unsafe { init(dir_w.as_ptr(), logs_w.as_ptr(), device) };
        if r != 0 {
            tracing::error!(result = r, ?logs, "dlss: Streamline did not start; the upscale stays AMD's (see victory_sl_bridge.log)");
            return None;
        }
        let mut buf = [0u8; 160];
        // SAFETY: the bridge export (optional), writing at most the buffer's length.
        let versions = unsafe { GetProcAddress(module, PCSTR(b"vsl_versions\0".as_ptr())) }
            .map(|f| {
                // SAFETY: of this type.
                let f = unsafe { std::mem::transmute::<Farproc, unsafe extern "C" fn(*mut u8, i32) -> i32>(f) };
                // SAFETY: a buffer of that length.
                let n = unsafe { f(buf.as_mut_ptr(), buf.len() as i32) }.clamp(0, buf.len() as i32) as usize;
                String::from_utf8_lossy(&buf[..n]).into_owned()
            })
            .unwrap_or_else(|| "Streamline".to_owned());
        tracing::info!(?dir, ?logs, %versions, "dlss: Streamline started, DLSS takes the title's upscale");
        Some(Dlss { evaluate, versions })
    })
    .as_ref()
}

/// Records DLSS for an upscale whose resources are host textures in the states set for it.
fn dlss_upscale(d: &DispatchUpscale, list: *mut c_void) -> Option<i32> {
    let dl = dlss()?;
    let flags = CREATE_FLAGS.load(std::sync::atomic::Ordering::Relaxed);
    let sign = |name: &str| if std::env::var(name).is_ok_and(|v| v.trim() == "-1") { -1.0 } else { 1.0 };
    let (jx, mv) = (sign("VICTORY_DLSS_JITTER_SIGN"), sign("VICTORY_DLSS_MVEC_SIGN"));
    // FidelityFX scales motion vectors into pixels; Streamline wants them in [-1, 1] of the frame.
    let mv_size = if flags & FFX_DISPLAY_RESOLUTION_MOTION_VECTORS != 0 { d.upscale_size } else { d.render_size };
    let res = |i: usize| d.resources[i].resource;
    let inputs = VslInputs {
        color: res(0),
        color_state: HOST_NON_PIXEL_SHADER_RESOURCE,
        depth: res(1),
        depth_state: HOST_NON_PIXEL_SHADER_RESOURCE,
        motion: res(2),
        motion_state: HOST_NON_PIXEL_SHADER_RESOURCE,
        exposure: res(3),
        exposure_state: HOST_NON_PIXEL_SHADER_RESOURCE,
        output: res(OUTPUT),
        output_state: HOST_UNORDERED_ACCESS,
        render: d.render_size,
        out: d.upscale_size,
        jitter: [d.jitter[0] * jx, d.jitter[1] * jx],
        mvec_scale: [
            mv * d.motion_vector_scale[0] / mv_size[0].max(1) as f32,
            mv * d.motion_vector_scale[1] / mv_size[1].max(1) as f32,
        ],
        camera_near: d.camera[0],
        camera_far: d.camera[1],
        camera_fov: d.camera[2],
        pre_exposure: d.pre_exposure,
        reset: i32::from(d.reset),
        depth_inverted: i32::from(flags & FFX_DEPTH_INVERTED != 0),
        hdr: i32::from(flags & FFX_HDR != 0),
        motion_jittered: i32::from(flags & FFX_MOTION_VECTORS_JITTER_CANCELLATION != 0),
        auto_exposure: i32::from(flags & FFX_AUTO_EXPOSURE != 0 || res(3).is_null()),
    };
    if inputs.depth.is_null() || inputs.motion.is_null() {
        return Some(-1);
    }
    // SAFETY: an open host list and live host textures in the states named.
    Some(unsafe { (dl.evaluate)(list, &raw const inputs) })
}

pub extern "C" fn ffxDispatch(context: *mut *mut c_void, desc: *const Header) -> ReturnCode {
    let Some(api) = api() else { return ERROR };
    if context.is_null() || desc.is_null() {
        return ERROR_PARAMETER;
    }
    // SAFETY: the title's description.
    let kind = unsafe { (*desc).kind };
    if kind != DISPATCH_UPSCALE {
        note("ffxDispatch", kind);
        return ERROR_PARAMETER;
    }
    // SAFETY: a description of this type.
    let mut d = unsafe { *desc.cast::<DispatchUpscale>() };
    let title_list = d.command_list as This;
    let Some(list) = crate::list::host_list(title_list) else {
        return ERROR_PARAMETER;
    };
    for (i, r) in d.resources.iter_mut().enumerate() {
        if r.resource.is_null() {
            continue;
        }
        let title = r.resource as This;
        let Some(texture) = crate::resource::host_resource(title) else {
            tracing::warn!(slot = i, "ffx: an upscale resource without a host texture");
            r.resource = std::ptr::null_mut();
            continue;
        };
        let (wanted, ffx) = if i == OUTPUT {
            (HOST_UNORDERED_ACCESS, FFX_UNORDERED_ACCESS)
        } else {
            (HOST_NON_PIXEL_SHADER_RESOURCE, FFX_COMPUTE_READ)
        };
        crate::list::ensure_state(list, title, wanted);
        if crate::host::legal_state(texture, wanted) != wanted {
            tracing::warn!(
                slot = i,
                wanted,
                "ffx: the host texture cannot take the upscaler's state"
            );
        }
        r.resource = texture.as_raw();
        r.state = ffx;
    }
    if d.resources[0].resource.is_null() || d.resources[OUTPUT].resource.is_null() {
        return ERROR_PARAMETER;
    }
    d.command_list = list.list.as_raw();
    let r = match dlss_upscale(&d, d.command_list) {
        Some(0) => {
            static FIRST_DLSS: std::sync::Once = std::sync::Once::new();
            FIRST_DLSS.call_once(|| {
                tracing::info!(render = ?d.render_size, upscale = ?d.upscale_size, jitter = ?d.jitter, mv_scale = ?d.motion_vector_scale, camera = ?d.camera, flags = CREATE_FLAGS.load(std::sync::atomic::Ordering::Relaxed), "dlss: first upscale recorded by DLSS");
                if let Some(dl) = dlss() {
                    crate::host::set_caption_note(format!(
                        "NVIDIA {} {}x{}\u{2192}{}x{}",
                        dl.versions, d.render_size[0], d.render_size[1], d.upscale_size[0], d.upscale_size[1]
                    ));
                }
            });
            OK
        }
        other => {
            if let Some(code) = other {
                static FAILED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
                if FAILED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 5 {
                    tracing::warn!(code, "dlss: the upscale failed; AMD's upscaler takes this frame");
                }
            }
            // SAFETY: the SDK's call on an open host list with live textures.
            unsafe { (api.dispatch)(context, (&raw const d).cast()) }
        }
    };
    // The upscaler bound its own heaps, root signature and pipeline.
    crate::list::forget_host_bindings(title_list);
    static FIRST: std::sync::Once = std::sync::Once::new();
    FIRST.call_once(|| {
        tracing::info!(render = ?d.render_size, upscale = ?d.upscale_size, result = r, "ffx: first upscale recorded on the host GPU");
    });
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_takes_the_upscale_description_only_beside_the_console_backend() {
        let mut backend = Header {
            kind: CONSOLE_BACKEND,
            next: std::ptr::null_mut(),
        };
        let backend = &raw mut backend;
        // The version description the title chains in between.
        let mut version = Header {
            kind: 0x1_000b,
            next: backend,
        };
        let upscale = CreateUpscale {
            kind: CREATE_UPSCALE,
            next: &raw mut version,
            flags: 41,
            max_render: [2560, 1440],
            max_upscale: [3840, 2160],
            message: std::ptr::null(),
        };
        let chain = (&raw const upscale).cast::<Header>();
        // SAFETY: a chain of our own descriptions, alive for the calls.
        let found = unsafe { upscale_in(chain) }.expect("the upscale description");
        assert_eq!(
            (found.flags, found.max_render, found.max_upscale),
            (41, [2560, 1440], [3840, 2160])
        );
        // A PC backend is someone else's chain: not ours to answer.
        // SAFETY: our own description, still alive.
        unsafe { (*backend).kind = BACKEND_DX12 };
        // SAFETY: as above.
        assert!(unsafe { upscale_in(chain) }.is_none());
        // SAFETY: an empty chain.
        assert!(unsafe { upscale_in(std::ptr::null()) }.is_none());
    }
}
