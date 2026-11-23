//! Linux x64 のスパイク。`libdbgshim.so` でコンソールを起動し、`STOP` の行で止める。
//!
//! `ICorDebug` の vtable は Windows の COM と同じ並び（先頭が `QueryInterface`）として呼ぶ。
//! ここが違っていれば、`Initialize` かブレークポイントのコールバックで落ちる。

mod line;

use line::stop_line;
use std::env;
use std::ffi::c_void;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;
use std::time::Duration;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

impl std::fmt::Debug for Guid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let d = &self.data4;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.data1, self.data2, self.data3, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        )
    }
}

const IID_UNKNOWN: Guid = Guid { data1: 0, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
const IID_CALLBACK: Guid = Guid { data1: 0x3d6f5f60, data2: 0x7538, data3: 0x11d3, data4: [0x8d, 0x5b, 0x00, 0x10, 0x4b, 0x35, 0xe7, 0xef] };
const IID_CALLBACK2: Guid = Guid { data1: 0x250E5EEA, data2: 0xDB5C, data3: 0x4C76, data4: [0xB6, 0xF3, 0x8C, 0x46, 0xF1, 0x2E, 0x32, 0x03] };
const IID_CALLBACK3: Guid = Guid { data1: 0x264EA0FC, data2: 0x2591, data3: 0x49AA, data4: [0x86, 0x8E, 0x83, 0x5E, 0x65, 0x15, 0x32, 0x3F] };
const IID_CALLBACK4: Guid = Guid { data1: 0x322911AE, data2: 0x16A5, data3: 0x49BA, data4: [0x84, 0xA3, 0xED, 0x69, 0x67, 0x81, 0x38, 0xA3] };
const E_NOINTERFACE: i32 = 0x8000_4002_u32 as i32;

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct ICorDebugVtbl {
    base: IUnknownVtbl,
    initialize: unsafe extern "system" fn(*mut c_void) -> i32,
    terminate: usize,
    set_managed_handler: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    set_unmanaged_handler: usize,
    create_process: usize,
    debug_active_process: unsafe extern "system" fn(*mut c_void, u32, i32, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ICorDebugControllerVtbl {
    base: IUnknownVtbl,
    stop: usize,
    continue_: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    is_running: usize,
    has_queued_callbacks: usize,
    enumerate_threads: usize,
    set_all_threads_debug_state: usize,
    detach: usize,
    terminate: unsafe extern "system" fn(*mut c_void, u32) -> i32,
}

#[repr(C)]
struct ICorDebugModuleVtbl {
    base: IUnknownVtbl,
    get_process: usize,
    get_base_address: usize,
    get_assembly: usize,
    get_name: unsafe extern "system" fn(*mut c_void, u32, *mut u32, *mut u16) -> i32,
    enable_jit_debugging: usize,
    enable_class_load_callbacks: usize,
    get_function_from_token: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ICorDebugFunctionVtbl {
    base: IUnknownVtbl,
    get_module: usize,
    get_class: usize,
    get_token: usize,
    get_il_code: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ICorDebugCodeVtbl {
    base: IUnknownVtbl,
    is_il: usize,
    get_function: usize,
    get_address: usize,
    get_size: usize,
    create_breakpoint: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct ICorDebugBreakpointVtbl {
    base: IUnknownVtbl,
    activate: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    is_active: usize,
    get_function: usize,
    get_offset: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
}

#[repr(C)]
struct CallbackVtbl {
    base: IUnknownVtbl,
    breakpoint: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i32,
    step_complete: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, u32) -> i32,
    break_: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    exception: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, i32) -> i32,
    eval_complete: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i32,
    eval_exception: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i32,
    create_process: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    exit_process: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    create_thread: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    exit_thread: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    load_module: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    unload_module: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    load_class: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    unload_class: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    debugger_error: unsafe extern "system" fn(*mut c_void, *mut c_void, i32, u32) -> i32,
    log_message: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, i32, *mut u16, *mut u16) -> i32,
    log_switch: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, i32, u32, *mut u16, *mut u16) -> i32,
    create_app_domain: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    exit_app_domain: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    load_assembly: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    unload_assembly: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    control_c_trap: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    name_change: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
    update_module_symbols: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i32,
    edit_and_continue_remap: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, i32) -> i32,
    breakpoint_set_error: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, u32) -> i32,
}

unsafe extern "C" {
    fn dlopen(filename: *const i8, flags: i32) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const i8) -> *mut c_void;
    fn dlerror() -> *const i8;
    fn kill(pid: i32, sig: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

const RTLD_NOW: i32 = 2;
const RTLD_GLOBAL: i32 = 0x00100;
const SIGKILL: i32 = 9;

type CreateProcessForLaunch = unsafe extern "system" fn(*mut u16, i32, *mut c_void, *const u16, *mut u32, *mut *mut c_void) -> i32;
type ResumeProcess = unsafe extern "system" fn(*mut c_void) -> i32;
type CloseResumeHandle = unsafe extern "system" fn(*mut c_void) -> i32;
type RegisterForRuntimeStartup3 = unsafe extern "system" fn(u32, *const u16, *mut c_void, StartupCallback, *mut c_void, *mut *mut c_void) -> i32;
type UnregisterForRuntimeStartup = unsafe extern "system" fn(*mut c_void) -> i32;
type StartupCallback = unsafe extern "system" fn(*mut c_void, *mut c_void, i32);

#[allow(dead_code)]
enum Ev {
    Startup(*mut c_void),
    StartupFailed(i32),
    LoadModule { controller: *mut c_void, module: *mut c_void },
    Breakpoint { controller: *mut c_void, breakpoint: *mut c_void },
    ExitProcess,
    ProcessGone,
    Other { controller: *mut c_void, name: &'static str },
}

unsafe impl Send for Ev {}

static TX: OnceLock<Sender<Ev>> = OnceLock::new();

fn post(ev: Ev) {
    if let Some(tx) = TX.get() {
        let _ = tx.send(ev);
    }
}

unsafe extern "system" fn query_interface(_this: *mut c_void, riid: *const Guid, out: *mut *mut c_void) -> i32 {
    if riid.is_null() {
        *out = std::ptr::null_mut();
        return E_NOINTERFACE;
    }
    let id = *riid;
    if id == IID_UNKNOWN || id == IID_CALLBACK {
        *out = &CALLBACK as *const Callback as *mut c_void;
        0
    } else if id == IID_CALLBACK2 {
        *out = &CALLBACK2 as *const Callback2 as *mut c_void;
        0
    } else if id == IID_CALLBACK3 {
        *out = &CALLBACK3 as *const Callback3 as *mut c_void;
        0
    } else if id == IID_CALLBACK4 {
        *out = &CALLBACK4 as *const Callback4 as *mut c_void;
        0
    } else {
        eprintln!("QueryInterface unsupported {id:?}");
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(_this: *mut c_void) -> u32 {
    2
}

unsafe extern "system" fn release(_this: *mut c_void) -> u32 {
    1
}

const UNKNOWN: IUnknownVtbl = IUnknownVtbl { query_interface, add_ref, release };

macro_rules! other {
    ($name:ident ( $($arg:ident : $ty:ty),* ) ) => {
        unsafe extern "system" fn $name(_this: *mut c_void, controller: *mut c_void $(, $arg: $ty)*) -> i32 {
            $(let _ = $arg;)*
            post(Ev::Other { controller, name: stringify!($name) });
            0
        }
    };
}

unsafe extern "system" fn breakpoint(_this: *mut c_void, controller: *mut c_void, _thread: *mut c_void, breakpoint: *mut c_void) -> i32 {
    post(Ev::Breakpoint { controller, breakpoint });
    0
}
other!(step_complete(thread: *mut c_void, stepper: *mut c_void, reason: u32));
other!(break_(thread: *mut c_void));
other!(exception(thread: *mut c_void, unhandled: i32));
other!(eval_complete(thread: *mut c_void, eval: *mut c_void));
other!(eval_exception(thread: *mut c_void, eval: *mut c_void));
other!(create_process());
unsafe extern "system" fn exit_process(_this: *mut c_void, _process: *mut c_void) -> i32 {
    post(Ev::ExitProcess);
    0
}
other!(create_thread(thread: *mut c_void));
other!(exit_thread(thread: *mut c_void));
unsafe extern "system" fn load_module(_this: *mut c_void, controller: *mut c_void, module: *mut c_void) -> i32 {
    post(Ev::LoadModule { controller, module });
    0
}
other!(unload_module(module: *mut c_void));
other!(load_class(class: *mut c_void));
other!(unload_class(class: *mut c_void));
other!(debugger_error(hr: i32, code: u32));
other!(log_message(thread: *mut c_void, level: i32, switch_name: *mut u16, message: *mut u16));
other!(log_switch(thread: *mut c_void, level: i32, reason: u32, switch_name: *mut u16, parent_name: *mut u16));
other!(create_app_domain(appdomain: *mut c_void));
other!(exit_app_domain(appdomain: *mut c_void));
other!(load_assembly(assembly: *mut c_void));
other!(unload_assembly(assembly: *mut c_void));
other!(control_c_trap());
other!(name_change(thread: *mut c_void));
other!(update_module_symbols(module: *mut c_void, stream: *mut c_void));
other!(edit_and_continue_remap(thread: *mut c_void, function: *mut c_void, accurate: i32));
other!(breakpoint_set_error(thread: *mut c_void, breakpoint: *mut c_void, error: u32));

#[repr(C)]
struct Callback2Vtbl {
    base: IUnknownVtbl,
    remap_opportunity: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, *mut c_void, u32) -> i32,
    create_connection: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut u16) -> i32,
    change_connection: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> i32,
    destroy_connection: unsafe extern "system" fn(*mut c_void, *mut c_void, u32) -> i32,
    exception2: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void, u32, u32, u32) -> i32,
    exception_unwind: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, u32, u32) -> i32,
    remap_complete: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i32,
    mda_notification: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> i32,
}

other!(remap_opportunity(thread: *mut c_void, old_function: *mut c_void, new_function: *mut c_void, old_offset: u32));
other!(create_connection(connection: u32, name: *mut u16));
other!(change_connection(connection: u32));
other!(destroy_connection(connection: u32));
other!(exception2(thread: *mut c_void, frame: *mut c_void, offset: u32, event_type: u32, flags: u32));
other!(exception_unwind(thread: *mut c_void, event_type: u32, flags: u32));
other!(remap_complete(thread: *mut c_void, function: *mut c_void));
other!(mda_notification(thread: *mut c_void, mda: *mut c_void));

#[repr(C)]
struct Callback2 {
    vtable: &'static Callback2Vtbl,
}

static CALLBACK2: Callback2 = Callback2 {
    vtable: &Callback2Vtbl {
        base: UNKNOWN,
        remap_opportunity,
        create_connection,
        change_connection,
        destroy_connection,
        exception2,
        exception_unwind,
        remap_complete,
        mda_notification,
    },
};

#[repr(C)]
struct Callback3Vtbl {
    base: IUnknownVtbl,
    custom_notification: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
}

unsafe extern "system" fn custom_notification(_this: *mut c_void, _thread: *mut c_void, appdomain: *mut c_void) -> i32 {
    post(Ev::Other { controller: appdomain, name: "custom_notification" });
    0
}

#[repr(C)]
struct Callback3 {
    vtable: &'static Callback3Vtbl,
}

static CALLBACK3: Callback3 = Callback3 { vtable: &Callback3Vtbl { base: UNKNOWN, custom_notification } };

#[repr(C)]
struct Callback4Vtbl {
    base: IUnknownVtbl,
    before_gc: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    after_gc: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    data_breakpoint: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut u8, u32) -> i32,
}

other!(before_gc());
other!(after_gc());
unsafe extern "system" fn data_breakpoint(_this: *mut c_void, process: *mut c_void, _thread: *mut c_void, _context: *mut u8, _size: u32) -> i32 {
    post(Ev::Other { controller: process, name: "data_breakpoint" });
    0
}

#[repr(C)]
struct Callback4 {
    vtable: &'static Callback4Vtbl,
}

static CALLBACK4: Callback4 = Callback4 {
    vtable: &Callback4Vtbl { base: UNKNOWN, before_gc, after_gc, data_breakpoint },
};

#[repr(C)]
struct Callback {
    vtable: &'static CallbackVtbl,
}

static CALLBACK: Callback = Callback {
    vtable: &CallbackVtbl {
        base: UNKNOWN,
        breakpoint,
        step_complete,
        break_,
        exception,
        eval_complete,
        eval_exception,
        create_process,
        exit_process,
        create_thread,
        exit_thread,
        load_module,
        unload_module,
        load_class,
        unload_class,
        debugger_error,
        log_message,
        log_switch,
        create_app_domain,
        exit_app_domain,
        load_assembly,
        unload_assembly,
        control_c_trap,
        name_change,
        update_module_symbols,
        edit_and_continue_remap,
        breakpoint_set_error,
    },
};

unsafe extern "system" fn on_startup(cordb: *mut c_void, _parameter: *mut c_void, hr: i32) {
    if hr < 0 || cordb.is_null() {
        post(Ev::StartupFailed(hr));
        return;
    }
    let unknown = &*(*(cordb as *const *const IUnknownVtbl));
    (unknown.add_ref)(cordb);
    post(Ev::Startup(cordb));
}

struct Shim {
    create_process_for_launch: CreateProcessForLaunch,
    resume_process: ResumeProcess,
    close_resume_handle: CloseResumeHandle,
    register_for_runtime_startup: RegisterForRuntimeStartup3,
    unregister_for_runtime_startup: UnregisterForRuntimeStartup,
}

fn load_shim(path: &str) -> Result<Shim, String> {
    let c = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
    let module = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
    if module.is_null() {
        let message = unsafe { std::ffi::CStr::from_ptr(dlerror()) }.to_string_lossy();
        return Err(format!("dlopen {path}: {message}"));
    }
    let find = |name: &str| -> Result<*mut c_void, String> {
        let c = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
        let address = unsafe { dlsym(module, c.as_ptr()) };
        if address.is_null() {
            Err(format!("libdbgshim.so has no {name}"))
        } else {
            Ok(address)
        }
    };
    unsafe {
        Ok(Shim {
            create_process_for_launch: std::mem::transmute(find("CreateProcessForLaunch")?),
            resume_process: std::mem::transmute(find("ResumeProcess")?),
            close_resume_handle: std::mem::transmute(find("CloseResumeHandle")?),
            register_for_runtime_startup: std::mem::transmute(find("RegisterForRuntimeStartup3")?),
            unregister_for_runtime_startup: std::mem::transmute(find("UnregisterForRuntimeStartup")?),
        })
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn hr(what: &str, code: i32) -> Result<(), String> {
    if code >= 0 {
        Ok(())
    } else {
        Err(format!("{what} failed (0x{code:08X})"))
    }
}

fn module_name(module: *mut c_void) -> String {
    let mut buffer = [0u16; 2048];
    let mut count = 0u32;
    let code = unsafe {
        let v = &*(*(module as *const *const ICorDebugModuleVtbl));
        (v.get_name)(module, buffer.len() as u32, &mut count, buffer.as_mut_ptr())
    };
    if code < 0 {
        return format!("(GetName 0x{code:08X})");
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

fn place_breakpoint(module: *mut c_void, token: u32, il_offset: u32) -> Result<(), String> {
    unsafe {
        let module_v = &*(*(module as *const *const ICorDebugModuleVtbl));
        let mut function = std::ptr::null_mut();
        hr("GetFunctionFromToken", (module_v.get_function_from_token)(module, token, &mut function))?;
        let function_v = &*(*(function as *const *const ICorDebugFunctionVtbl));
        let mut code = std::ptr::null_mut();
        hr("GetILCode", (function_v.get_il_code)(function, &mut code))?;
        let code_v = &*(*(code as *const *const ICorDebugCodeVtbl));
        let mut breakpoint = std::ptr::null_mut();
        hr("CreateBreakpoint", (code_v.create_breakpoint)(code, il_offset, &mut breakpoint))?;
        let breakpoint_v = &*(*(breakpoint as *const *const ICorDebugBreakpointVtbl));
        hr("Activate", (breakpoint_v.activate)(breakpoint, 1))?;
    }
    Ok(())
}

fn continue_controller(controller: *mut c_void) {
    if controller.is_null() {
        return;
    }
    let code = unsafe {
        let v = &*(*(controller as *const *const ICorDebugControllerVtbl));
        (v.continue_)(controller, 0)
    };
    if code < 0 {
        eprintln!("Continue failed (0x{code:08X})");
    }
}

fn terminate_process(process: *mut c_void) {
    if process.is_null() {
        return;
    }
    let code = unsafe {
        let v = &*(*(process as *const *const ICorDebugControllerVtbl));
        (v.terminate)(process, 0)
    };
    if code < 0 {
        eprintln!("Terminate failed (0x{code:08X})");
    }
}

struct Args {
    shim: PathBuf,
    dotnet: PathBuf,
    dll: PathBuf,
}

struct Reap(u32);
impl Drop for Reap {
    fn drop(&mut self) {
        unsafe { kill(self.0 as i32, SIGKILL) };
    }
}

fn args() -> Result<Args, String> {
    let mut shim = None;
    let mut dotnet = None;
    let mut dll = None;
    let mut rest = env::args().skip(1);
    while let Some(flag) = rest.next() {
        let value = rest.next().ok_or_else(|| format!("{flag} needs a path"))?;
        match flag.as_str() {
            "--shim" => shim = Some(PathBuf::from(value)),
            "--dotnet" => dotnet = Some(PathBuf::from(value)),
            "--dll" => dll = Some(PathBuf::from(value)),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        shim: shim.ok_or("--shim is required")?,
        dotnet: dotnet.ok_or("--dotnet is required")?,
        dll: dll.ok_or("--dll is required")?,
    })
}

fn offset_of(breakpoint: *mut c_void) -> Result<u32, String> {
    let mut offset = 0u32;
    let code = unsafe {
        let v = &*(*(breakpoint as *const *const ICorDebugBreakpointVtbl));
        (v.get_offset)(breakpoint, &mut offset)
    };
    hr("GetOffset", code)?;
    Ok(offset)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("spike failed: {message}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<(), String> {
    let args = args()?;
    // 既定の ReadyToRun のまま IL のブレークポイントが届くかも、このスパイクで見る。環境は子に引き継がせる。
    env::set_var("DOTNET_ReadyToRun", "0");
    let (tx, rx) = mpsc::channel();
    TX.set(tx).map_err(|_| "event channel was already set")?;

    let shim = load_shim(&args.shim.to_string_lossy())?;
    let command = format!("{} {}", args.dotnet.display(), args.dll.display());
    eprintln!("launch {command}");
    let mut command_wide = wide(&command);
    let cwd = args.dll.parent().ok_or("the dll has no directory")?;
    let cwd_wide = wide(&cwd.to_string_lossy());
    let (mut pid, mut resume) = (0u32, std::ptr::null_mut());
    unsafe {
        hr(
            "CreateProcessForLaunch",
            (shim.create_process_for_launch)(command_wide.as_mut_ptr(), 1, std::ptr::null_mut(), cwd_wide.as_ptr(), &mut pid, &mut resume),
        )?;
    }
    eprintln!("pid {pid}");
    let _reap = Reap(pid);
    let pid_for_watch = pid;
    std::thread::spawn(move || {
        let mut status = 0i32;
        unsafe { waitpid(pid_for_watch as i32, &mut status, 0) };
        post(Ev::ProcessGone);
    });
    let mut registration = std::ptr::null_mut();
    let registered = unsafe {
        (shim.register_for_runtime_startup)(pid, std::ptr::null(), std::ptr::null_mut(), on_startup, std::ptr::null_mut(), &mut registration)
    };
    unsafe {
        let _ = (shim.resume_process)(resume);
        let _ = (shim.close_resume_handle)(resume);
    }
    hr("RegisterForRuntimeStartup3", registered)?;

    let cordb = match recv(&rx, "the runtime")? {
        Ev::Startup(cordb) => cordb,
        Ev::StartupFailed(code) => return Err(format!("runtime startup failed (0x{code:08X})")),
        Ev::ProcessGone => return Err("the process exited before the runtime started".into()),
        _ => return Err("unexpected event before the runtime started".into()),
    };
    if !registration.is_null() {
        unsafe { (shim.unregister_for_runtime_startup)(registration) };
    }
    eprintln!("ICorDebug received");
    let process = unsafe {
        let v = &*(*(cordb as *const *const ICorDebugVtbl));
        hr("Initialize", (v.initialize)(cordb))?;
        hr("SetManagedHandler", (v.set_managed_handler)(cordb, &CALLBACK as *const Callback as *mut c_void))?;
        let mut process = std::ptr::null_mut();
        hr("DebugActiveProcess", (v.debug_active_process)(cordb, pid, 0, &mut process))?;
        process
    };
    eprintln!("DebugActiveProcess ok");

    let mut expected: Option<(u32, u32, String)> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let ev = rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).map_err(|_| "timed out waiting for the breakpoint")?;
        match ev {
            Ev::LoadModule { controller, module } => {
                let name = module_name(module);
                eprintln!("LoadModule {name}");
                if expected.is_none() && name.ends_with("Spike.dll") {
                    match stop_line(std::path::Path::new(&name)) {
                        Ok(stop) => {
                            place_breakpoint(module, stop.token, stop.il_offset)?;
                            eprintln!("breakpoint at {}:{} il={}", stop.file, stop.line, stop.il_offset);
                            expected = Some((stop.il_offset, stop.line, stop.file));
                        }
                        Err(error) => eprintln!("stop_line: {error}"),
                    }
                }
                continue_controller(controller);
            }
            Ev::Breakpoint { breakpoint, .. } => {
                let offset = offset_of(breakpoint)?;
                let (want, line_no, file) = expected.ok_or("stopped at a breakpoint we did not set")?;
                if offset != want {
                    return Err(format!("stopped at IL {offset}, expected {want}"));
                }
                terminate_process(process);
                println!("stopped at {file}:{line_no} il={offset}");
                return Ok(());
            }
            Ev::ExitProcess | Ev::ProcessGone => return Err("the process ended before the breakpoint".into()),
            Ev::Other { controller, name } => {
                eprintln!("{name}");
                continue_controller(controller);
            }
            Ev::Startup(_) | Ev::StartupFailed(_) => return Err("a second runtime startup arrived".into()),
        }
    }
}

fn recv(rx: &Receiver<Ev>, what: &str) -> Result<Ev, String> {
    rx.recv_timeout(Duration::from_secs(30)).map_err(|_| format!("timed out waiting for {what}"))
}
