// SPDX-License-Identifier: GPL-3.0-or-later
//! Initial live guest GPU backend. All unsupported calls stop with a concrete
//! error instead of falling back to host D3D12 or acknowledging fake draws.
use std::sync::{Mutex,OnceLock,atomic::{AtomicBool,Ordering}};
use crate::object::{self,This};
use vortex_vulkan::live::{ImageSpec,Live,Reply,Task};
static ENABLED:AtomicBool=AtomicBool::new(false);
static WORKER:OnceLock<Result<Live,String>>=OnceLock::new();
static SUBMIT:Mutex<()>=Mutex::new(());
static RESOURCES:Mutex<std::collections::BTreeSet<usize>>=Mutex::new(std::collections::BTreeSet::new());

pub fn enable(){ENABLED.store(true,Ordering::Release);}
pub(crate) fn enabled()->bool{ENABLED.load(Ordering::Acquire)}
pub(crate) fn stop(message:impl std::fmt::Display)->!{
    eprintln!("VULKAN_RUNTIME_BLOCKER: {message}; no D3D12 fallback");
    tracing::error!(%message,"VULKAN_RUNTIME_BLOCKER");
    std::process::exit(2)
}
pub(crate) fn initialize(){
    match WORKER.get_or_init(Live::start){
        Ok(_)=>{},
        Err(e)=>stop(e),
    }
    static ONCE:std::sync::Once=std::sync::Once::new();
    ONCE.call_once(||println!("VULKAN_GUEST_DEVICE_READY: real AMD device and queue"));
}
fn call(task:Task)->Reply{
    match WORKER.get().expect("guest device initialized"){
        Ok(worker)=>worker.call(task).unwrap_or_else(|e|stop(e)),
        Err(e)=>stop(e),
    }
}
pub(crate) fn register(key:usize){
    RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).insert(key);
}
pub(crate) fn registered(resource:This)->bool{
    !resource.is_null()&&RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).contains(&(resource as usize))
}
pub(crate) fn forget(key:usize){
    RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).remove(&key);
    if matches!(WORKER.get(),Some(Ok(_))){let _=call(Task::Forget(key));}
}

pub(crate) struct Retained(usize);
impl Retained{
    pub(crate) fn new(resource:This)->Self{
        if !registered(resource){stop("invalid or retired guest resource");}
        object::add_ref(resource);Self(resource as usize)
    }
    fn get(&self)->This{self.0 as This}
}
impl Drop for Retained{
    fn drop(&mut self){crate::resource::release_live(self.get());}
}

pub(crate) enum Op{
    Copy{dst:Retained,src:Retained,dst_offset:u64,src_offset:u64,len:u64},
    ClearColor{resource:Retained,spec:ImageSpec,rgba:[f32;4]},
    ClearDepth{resource:Retained,spec:ImageSpec,z:f32},
    CopyImage{src:Retained,dst:Retained,from:ImageSpec,to:ImageSpec},
}
#[derive(Default)]
pub(crate) struct Recording{closed:bool,ops:Vec<Op>}
impl Recording{
    pub(crate) fn push(&mut self,op:Op){if self.closed{stop("recording into closed guest list");}self.ops.push(op);}
    pub(crate) fn close(&mut self){if self.closed{stop("guest list already closed");}self.closed=true;}
    pub(crate) fn reset(&mut self){self.ops.clear();self.closed=false;}
    pub(crate) fn execute(&self){
        if !self.closed{stop("submitting open guest list");}
        for op in &self.ops{
            match op{
                Op::Copy{dst,src,dst_offset,src_offset,len}=>copy(dst.get(),src.get(),*dst_offset,*src_offset,*len),
                Op::ClearColor{resource,spec,rgba}=>{
                    if resource.0!=spec.key{stop("color resource identity mismatch");}
                    let _=call(Task::ClearColor{image:*spec,rgba:*rgba});
                }
                Op::ClearDepth{resource,spec,z}=>{
                    if resource.0!=spec.key{stop("depth resource identity mismatch");}
                    let _=call(Task::ClearDepth{image:*spec,z:*z});
                }
                Op::CopyImage{src,dst,from,to}=>{
                    if src.0!=from.key||dst.0!=to.key{stop("image copy identity mismatch");}
                    let _=call(Task::CopyImage{src:*from,dst:*to});
                }
            }
        }
    }
}
pub(crate) fn submit_lock()->std::sync::MutexGuard<'static,()>{
    SUBMIT.lock().unwrap_or_else(|e|e.into_inner())
}
fn range(resource:This,offset:u64,len:u64)->usize{
    if !registered(resource){stop("invalid guest buffer");}
    let(data,size)=crate::resource::buffer_bytes(resource).unwrap_or_else(||stop("copy requires guest buffer"));
    if offset.checked_add(len).is_none_or(|end|end>size){stop("guest buffer copy out of range");}
    let offset=usize::try_from(offset).unwrap_or_else(|_|stop("guest offset exceeds address space"));
    (data as usize).checked_add(offset).unwrap_or_else(||stop("guest address overflow"))
}
fn copy(dst:This,src:This,dst_offset:u64,src_offset:u64,len:u64){
    use windows_sys::Win32::System::{Diagnostics::Debug::{ReadProcessMemory,WriteProcessMemory},Threading::GetCurrentProcess};
    let from=range(src,src_offset,len);let to=range(dst,dst_offset,len);
    let n=usize::try_from(len).unwrap_or_else(|_|stop("copy size exceeds address space"));
    let end=from.checked_add(n).unwrap_or_else(||stop("source overflow"));
    let _=to.checked_add(n).unwrap_or_else(||stop("destination overflow"));
    // Copy in the correct direction even for overlapping guest addresses.
    let backward=to>from&&to<end;
    let mut done=0usize;
    while done<n{
        let size=(n-done).min(4*1024*1024);
        let offset=if backward{n-done-size}else{done};
        let mut bytes=vec![0u8;size];let mut actual=0usize;
        // SAFETY: own process; kernel verifies the guest mapping; exact byte
        // counts are required. The worker sees owned bytes, not guest pointers.
        if unsafe{ReadProcessMemory(GetCurrentProcess(),(from+offset) as *const _,bytes.as_mut_ptr().cast(),size,&mut actual)}==0||actual!=size{
            stop("guest buffer source is not readable");
        }
        let Reply::Bytes(bytes)=call(Task::CopyBytes(bytes))else{stop("invalid Vulkan copy response");};
        if bytes.len()!=size{stop("invalid Vulkan readback length");}
        actual=0;
        if unsafe{WriteProcessMemory(GetCurrentProcess(),(to+offset) as *mut _,bytes.as_ptr().cast(),size,&mut actual)}==0||actual!=size{
            stop("guest buffer destination is not writable");
        }
        done+=size;
    }
    if n>0{
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||println!("VULKAN_GUEST_COPY_OK: {n} actual guest bytes transferred through GPU"));
    }
}
pub(crate) fn present(resource:This){
    let _lock=submit_lock();
    let hold=Retained::new(resource);
    let spec=crate::resource::live_image_spec(hold.get()).unwrap_or_else(||stop("unsupported guest present resource"));
    if spec.depth{stop("depth image is not a presentation source");}
    static WINDOW:OnceLock<usize>=OnceLock::new();
    let hwnd=*WINDOW.get_or_init(||{
        let window=crate::host::title_window().or_else(||crate::host::window(spec.width,spec.height).ok())
            .unwrap_or_else(||stop("cannot create live presentation window"));
        window.0 as usize
    });
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    let hinstance=unsafe{GetModuleHandleW(None)}.unwrap_or_else(|e|stop(e)).0 as usize;
    let _=call(Task::Present{image:spec,hwnd,hinstance});
    static FIRST:std::sync::Once=std::sync::Once::new();
    FIRST.call_once(||println!("FIRST_VULKAN_GUEST_PRESENT_OK: {}x{} actual guest target (not proof of scene rendering)",spec.width,spec.height));
}
