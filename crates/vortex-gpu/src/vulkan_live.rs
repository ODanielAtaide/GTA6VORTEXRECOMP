// SPDX-License-Identifier: GPL-3.0-or-later
//! Initial live guest GPU backend. All unsupported calls stop with a concrete
//! error instead of falling back to host D3D12 or acknowledging fake draws.
use std::sync::{Mutex, OnceLock};
#[cfg(any(test, not(all(target_arch = "x86_64", target_env = "msvc"))))]
use std::sync::atomic::{AtomicBool, Ordering};
use crate::object::{self,This};
use vortex_vulkan::live::{ImageSpec,Live,Reply,Task};
#[cfg(any(test, not(all(target_arch = "x86_64", target_env = "msvc"))))]
static ENABLED: AtomicBool = AtomicBool::new(false);
static WORKER:OnceLock<Result<Live,String>>=OnceLock::new();
static SUBMIT:Mutex<()>=Mutex::new(());
// CPU-side provenance only. Registered resource spans are not Vulkan GPU
// virtual addresses. Never dereference a shader-provided address here.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct Span{start:u64,end:u64}
impl Span{
    fn new(start:u64,size:u64)->Option<Self>{
        if start==0||size==0{return None;}
        Some(Self{start,end:start.checked_add(size)?})
    }
    fn contains(self,address:u64,len:u64)->bool{
        len!=0&&address>=self.start&&address.checked_add(len).is_some_and(|end|end<=self.end)
    }
}
type ResourceMap=std::collections::BTreeMap<usize,Option<Span>>;
static RESOURCES:Mutex<ResourceMap>=Mutex::new(std::collections::BTreeMap::new());

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum AddressClass{Null,Unmapped,Ambiguous,Unique{key:usize,offset:u64,remaining:u64}}
fn classify_address(resources:&ResourceMap,address:u64,len:u64)->AddressClass{
    if address==0{return AddressClass::Null;}
    let mut found=AddressClass::Unmapped;
    for (&key,span) in resources{
        let Some(span)=span else{continue;};
        if span.contains(address,len){
            if found!=AddressClass::Unmapped{return AddressClass::Ambiguous;}
            found=AddressClass::Unique{key,offset:address-span.start,remaining:span.end-address};
        }
    }
    found
}
fn root_address_status(address:u64)->(&'static str,u64){
    let resources=RESOURCES.lock().unwrap_or_else(|e|e.into_inner());
    match classify_address(&resources,address,1){
        AddressClass::Null=>("null",0),
        AddressClass::Unmapped=>("unmapped",0),
        AddressClass::Ambiguous=>("aliased",0),
        AddressClass::Unique{remaining,..}=>("registered-buffer",remaining),
    }
}
#[cfg(test)]
mod address_tests{
    use super::*;
    #[test]fn ranges_check_overflow_boundaries_and_aliasing(){
        let mut map=ResourceMap::new();
        map.insert(1,Span::new(0x1000,0x100));
        assert_eq!(classify_address(&map,0,4),AddressClass::Null);
        assert_eq!(classify_address(&map,0x10fc,4),AddressClass::Unique{key:1,offset:0xfc,remaining:4});
        assert_eq!(classify_address(&map,0x10fc,8),AddressClass::Unmapped);
        assert_eq!(classify_address(&map,0x1100,1),AddressClass::Unmapped);
        assert_eq!(Span::new(u64::MAX,2),None);
        map.insert(2,Span::new(0x1080,0x80));
        assert_eq!(classify_address(&map,0x1080,4),AddressClass::Ambiguous);
        map.remove(&2);
        assert!(matches!(classify_address(&map,0x1080,4),AddressClass::Unique{key:1,..}));
        assert_eq!(classify_address(&map,0x1080,0),AddressClass::Unmapped);
    }
}

pub fn enable() {
    #[cfg(any(test, not(all(target_arch = "x86_64", target_env = "msvc"))))]
    ENABLED.store(true, Ordering::Release);
}

// The shipped native runtime selects Vulkan at build time. Tests can still
// switch paths to exercise the historical device independently of title runs.
#[cfg(all(not(test), target_arch = "x86_64", target_env = "msvc"))]
#[inline(always)]
pub const fn enabled() -> bool {
    true
}

#[cfg(any(test, not(all(target_arch = "x86_64", target_env = "msvc"))))]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}
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
pub(crate) fn register(key:usize,buffer:Option<(u64,u64)>){
    let span=buffer.and_then(|(start,size)|Span::new(start,size));
    RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).insert(key,span);
}
pub(crate) fn registered(resource:This)->bool{
    !resource.is_null()&&RESOURCES.lock().unwrap_or_else(|e|e.into_inner()).contains_key(&(resource as usize))
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

// Like XCL's cpu_writes design (xcl-dev/xcl commit 6089122), markers
// may live outside resource objects. Vortex keeps them in command order
// and checks the Windows mapping again after Vulkan has completed.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) struct HostMarker {
    address:usize,
    allocation:usize,
    kind:u32,
}
impl HostMarker {
    fn query(address:u64,bytes:usize)->Option<Self>{
        use windows_sys::Win32::System::Memory::{
            VirtualQuery,MEMORY_BASIC_INFORMATION,MEM_COMMIT,MEM_PRIVATE,MEM_MAPPED,
            PAGE_READWRITE,
        };
        let address=usize::try_from(address).ok()?;
        if address==0||!matches!(bytes,4|8)||address%bytes!=0{return None;}
        let end=address.checked_add(bytes)?;
        let mut m:MEMORY_BASIC_INFORMATION=unsafe{std::mem::zeroed()};
        if unsafe{VirtualQuery(address as *const _,&mut m,std::mem::size_of_val(&m))}==0
            || m.State!=MEM_COMMIT || !matches!(m.Type,MEM_PRIVATE|MEM_MAPPED)
            || m.Protect!=PAGE_READWRITE
            || (m.BaseAddress as usize)>address
            || (m.BaseAddress as usize).checked_add(m.RegionSize)?<end{return None;}
        Some(Self{address,allocation:m.AllocationBase as usize,kind:m.Type})
    }
    fn still_valid(self,bytes:usize)->bool{
        Self::query(self.address as u64,bytes)==Some(self)
    }
}
pub(crate) enum MarkerTarget{
    Buffer{resource:Retained,offset:u64},
    Host(HostMarker),
}
impl MarkerTarget {
    fn resolve(address:u64,bytes:usize)->Self{
        let location={
            let resources=RESOURCES.lock().unwrap_or_else(|e|e.into_inner());
            classify_address(&resources,address,bytes as u64)
        };
        match location{
            AddressClass::Unique{key,offset,..}=>Self::Buffer{resource:Retained::new(key as This),offset},
            AddressClass::Unmapped=>Self::Host(HostMarker::query(address,bytes).unwrap_or_else(||stop(format_args!(
                "marker address {address:#x} is neither a registered buffer nor committed writable data memory")))),
            _=>stop(format_args!("ambiguous or null marker address {address:#x}: {location:?}")),
        }
    }
    fn address(&self,bytes:usize)->usize{
        match self{
            Self::Buffer{resource,offset}=>range(resource.get(),*offset,bytes as u64),
            Self::Host(mapping)=>{
                if !mapping.still_valid(bytes){stop("guest marker mapping changed before completion");}
                mapping.address
            }
        }
    }
}
fn wait_comparison(op:u32,value:u64,reference:u64,mask:u64)->bool{
    let value=value&mask;
    match op{0|8=>true,1=>false,2=>value<reference,3=>value==reference,
        4=>value<=reference,5=>value>reference,6=>value!=reference,7=>value>=reference,
        _=>stop(format_args!("unknown guest wait comparison {op}"))}
}
fn wait_read(target:&MarkerTarget,bytes:usize,comparison:u32,reference:u64,mask:u64)->bool{
    if matches!(comparison,0|8){return true;}
    let address=match target{
        MarkerTarget::Buffer{resource,offset}=>range(resource.get(),*offset,bytes as u64),
        MarkerTarget::Host(mapping)=>mapping.address,
    };
    let mut raw=[0u8;8];
    if !crate::host::read_into(address as *const u8,&mut raw[..bytes]){
        stop(format_args!("guest wait address {address:#x} is unreadable"));
    }
    let ready=wait_comparison(comparison,u64::from_le_bytes(raw),reference,mask);
    // Avoid expensive VirtualQuery on every poll; revalidate before consuming.
    if ready {let _=target.address(bytes);}
    ready
}
#[derive(Clone,Copy)]
pub(crate) enum MarkerValue{Immediate(u64),GpuTimestamp,SystemTimestamp}
pub(crate) enum Op{
    Sync,
    TextureSync{resources:Vec<Retained>},
    WaitMarker{target:MarkerTarget,bytes:usize,comparison:u32,reference:u64,mask:u64},
    WriteMarker{target:MarkerTarget,value:MarkerValue,bytes:usize},
    Copy{dst:Retained,src:Retained,dst_offset:u64,src_offset:u64,len:u64},
    ClearColor{resource:Retained,spec:ImageSpec,rgba:[f32;4]},
    ClearDepth{resource:Retained,spec:ImageSpec,z:f32},
    CopyImage{src:Retained,dst:Retained,from:ImageSpec,to:ImageSpec},
}
#[derive(Default)]
struct RootState{
    signature:Option<std::sync::Arc<[u8]>>,
    tables:std::collections::BTreeMap<u32,u64>,
    views:std::collections::BTreeMap<u32,(u32,u64)>,
    constants:std::collections::BTreeMap<(u32,u32),u32>,
}
#[derive(Default)]
pub(crate) struct Recording{closed:bool,ops:Vec<std::sync::Arc<Op>>,
    pipeline:Option<std::sync::Arc<crate::pipeline::native_live::Description>>,
    roots:[RootState;2],
    heaps:Vec<crate::descriptor::LiveHeap>}

impl Recording{
    pub(crate) fn push(&mut self,op:Op){if self.closed{stop("recording into closed guest list");}self.ops.push(std::sync::Arc::new(op));}
    pub(crate) fn write_marker(&mut self,address:u64,value:u64,bytes:usize,flush:u32,flags:u32){
        if self.closed{stop("recording write into closed guest list");}
        if !(flags==0||((flags==1||flags==2)&&bytes==8))||!matches!(bytes,4|8)||address==0||address%(bytes as u64)!=0{
            stop(format_args!("unsupported marker flags or alignment: address={address:#x} flush={flush:#x} flags={flags:#x} bytes={bytes}"));
        }
        let location={
            let resources=RESOURCES.lock().unwrap_or_else(|e|e.into_inner());
            classify_address(&resources,address,bytes as u64)
        };
        let target=match location{
            AddressClass::Unique{key,offset,..}=>MarkerTarget::Buffer{resource:Retained::new(key as This),offset},
            AddressClass::Unmapped=>{
                let mapping=HostMarker::query(address,bytes).unwrap_or_else(||stop(format_args!(
                    "marker address {address:#x} is neither a registered buffer nor committed writable data memory")));
                static FIRST_HOST:std::sync::Once=std::sync::Once::new();
                FIRST_HOST.call_once(||tracing::info!(address,bytes,allocation=mapping.allocation,kind=mapping.kind,"VULKAN_GUEST_HOST_MARKER_RECORDED"));
                MarkerTarget::Host(mapping)
            }
            _=>stop(format_args!("ambiguous or null marker address {address:#x}: {location:?}")),
        };
        let marker=match flags{1=>MarkerValue::GpuTimestamp,2=>MarkerValue::SystemTimestamp,_=>MarkerValue::Immediate(value)};
        self.push(Op::WriteMarker{target,value:marker,bytes});
        if flags==1{static FIRST_TIMESTAMP:std::sync::Once=std::sync::Once::new();FIRST_TIMESTAMP.call_once(||tracing::info!(address,"VULKAN_GUEST_GPU_TIMESTAMP_RECORDED"));}
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||tracing::info!(address,value,bytes,flush,flags,"VULKAN_GUEST_WRITE_RECORDED"));
    }
    pub(crate) fn write32(&mut self,address:u64,value:u32,flush:u32,flags:u32){
        self.write_marker(address,u64::from(value),4,flush,flags);
    }
    pub(crate) fn copy_memory(&mut self,dst:u64,src:u64,len:u64,flags:u32){
        if len==0{return;}
        if flags!=0{stop(format_args!("unported CopyMemoryToMemoryX flags {flags:#x}"));}
        let (from,to)={
            let resources=RESOURCES.lock().unwrap_or_else(|e|e.into_inner());
            (classify_address(&resources,src,len),classify_address(&resources,dst,len))
        };
        let (AddressClass::Unique{key:src_key,offset:src_offset,..},AddressClass::Unique{key:dst_key,offset:dst_offset,..})=(from,to)
            else{stop(format_args!("CopyMemoryToMemoryX requires registered non-aliased spans: src={from:?}, dst={to:?}, bytes={len}"));};
        self.push(Op::Copy{dst:Retained::new(dst_key as This),src:Retained::new(src_key as This),dst_offset,src_offset,len});
    }
    pub(crate) fn wait_marker(&mut self,address:u64,comparison:u32,reference:u64,mask_or_flags:u64,bytes:usize){
        if comparison>8||!matches!(bytes,4|8)||address==0||address%(bytes as u64)!=0{
            stop(format_args!("unsupported guest wait address={address:#x} comparison={comparison} bytes={bytes}"));
        }
        // Retail 0x248/0x250 call shape documented by OpenScarlett's
        // d3d12_xs_shim.cpp (48a98fcd); 0/1 in the mask slot are flags.
        let width_mask=if bytes==4{u64::from(u32::MAX)}else{u64::MAX};
        // The newer retail method accepts a 32-bit flags field in this
        // argument position. Windows x64 does not define its high dword.
        // The older ABI uses explicit masks, preserved by its caller.
        let mask=if mask_or_flags<=1{width_mask}else{mask_or_flags&width_mask};
        let target=MarkerTarget::resolve(address,bytes);
        self.push(Op::WaitMarker{target,bytes,comparison,reference:reference&width_mask,mask});
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||tracing::info!(address,comparison,reference,mask,bytes,"VULKAN_GUEST_WAIT_RECORDED"));
    }
    pub(crate) fn close(&mut self){if self.closed{stop("guest list already closed");}self.closed=true;}
    pub(crate) fn reset(&mut self){self.ops.clear();self.closed=false;self.pipeline=None;self.roots=Default::default();self.heaps.clear();}
    pub(crate) fn set_pipeline(&mut self,pipeline:This){
        if self.closed{stop("binding into closed guest list");}
        self.pipeline=if pipeline.is_null(){None}else{Some(crate::pipeline::native_live::get(pipeline).unwrap_or_else(||stop("invalid native pipeline object")))};
    }
    pub(crate) fn set_heaps(&mut self,heaps:&[This]){
        if self.closed||heaps.len()>2{stop("invalid descriptor heap binding");}
        let mut retained: Vec<crate::descriptor::LiveHeap> = Vec::new();
        for &heap in heaps{
            let h=crate::descriptor::LiveHeap::retain(heap).unwrap_or_else(||stop("unknown or non-shader-visible descriptor heap"));
            if retained.iter().any(|old:&crate::descriptor::LiveHeap|old.kind==h.kind){stop("duplicate descriptor heap type");}
            retained.push(h);
        }
        retained.sort_by_key(|h|h.kind);
        if self.heaps!=retained{
            self.heaps=retained;
            for root in &mut self.roots{root.tables.clear();}
        }
        static FIRST:std::sync::Once=std::sync::Once::new();
        FIRST.call_once(||tracing::info!(heaps=?self.heaps,"VULKAN_GUEST_DESCRIPTOR_HEAPS_BOUND"));
    }
    fn root_state(&mut self,compute:bool)->&mut RootState{
        if self.closed{stop("setting root state in closed list");}
        &mut self.roots[usize::from(compute)]
    }
    pub(crate) fn set_root(&mut self,compute:bool,root:This){
        let signature=if root.is_null(){None}else{Some(crate::root::live_snapshot(root).unwrap_or_else(||stop("unknown bound root signature")))};
        let state=self.root_state(compute);
        // A different signature invalidates previous root bindings.
        let unchanged=match (&state.signature,&signature){
            (Some(a),Some(b))=>std::sync::Arc::ptr_eq(a,b),(None,None)=>true,_=>false
        };
        if !unchanged{*state=RootState{signature,..Default::default()};}
    }
    pub(crate) fn set_table(&mut self,compute:bool,slot:u32,handle:u64){
        if slot>=1024{stop("invalid root table slot");}
        let state=self.root_state(compute);
        state.views.remove(&slot);state.tables.insert(slot,handle);
        state.constants.retain(|&(s,_),_|s!=slot);
    }
    pub(crate) fn set_view(&mut self,compute:bool,slot:u32,kind:u32,address:u64){
        if slot>=1024{stop("invalid root view slot");}
        let state=self.root_state(compute);
        state.tables.remove(&slot);state.views.insert(slot,(kind,address));
        state.constants.retain(|&(s,_),_|s!=slot);
    }
    pub(crate) fn set_constants(&mut self,compute:bool,slot:u32,offset:u32,values:&[u32]){
        if slot>=1024||offset.checked_add(values.len() as u32).is_none_or(|n|n>64){stop("invalid root constants range");}
        let state=self.root_state(compute);
        state.tables.remove(&slot);state.views.remove(&slot);
        for (i,&value) in values.iter().enumerate(){state.constants.insert((slot,offset+i as u32),value);}
    }
    pub(crate) fn require_pipeline(&self,kind:&str)->!{
        let root=&self.roots[usize::from(kind=="dispatch")];
        tracing::info!(kind,heaps=?self.heaps,table_handles_valid=root.tables.values().all(|&h|self.heaps.iter().any(|heap|heap.contains(h))),root_bytes=root.signature.as_ref().map_or(0,|v|v.len()),
            table_slots=?root.tables.keys().collect::<Vec<_>>(),
            view_slots=?root.views.iter().map(|(k,v)|(*k,v.0,root_address_status(v.1))).collect::<Vec<_>>(),
            constant_dwords=root.constants.len(),"VULKAN_GUEST_ROOT_BINDINGS");
        self.pipeline.as_ref().unwrap_or_else(||stop(format_args!("{kind}: no bound native pipeline"))).require_execution(kind)
    }
    pub(crate) fn snapshot(&self)->Vec<std::sync::Arc<Op>>{
        if !self.closed{stop("submitting open guest list");}
        self.ops.clone()
    }
}
impl Op{
    fn ready(&self)->bool{
        match self{
            Self::WaitMarker{target,bytes,comparison,reference,mask}=>wait_read(target,*bytes,*comparison,*reference,*mask),
            _=>true,
        }
    }
    fn execute(&self){
            match self{
                Op::WaitMarker{..}=>{
                    static FIRST:std::sync::Once=std::sync::Once::new();
                    FIRST.call_once(||tracing::info!("VULKAN_GUEST_WAIT_SATISFIED"));
                },
                Op::TextureSync{resources}=>{
                    // Every materialized live image stays in GENERAL, so
                    // there is no Vulkan layout change to perform. A full
                    // device memory dependency covers prior and next uses.
                    // Unmaterialized resources remain unmaterialized.
                    let _=call(Task::Sync);
                    static FIRST:std::sync::Once=std::sync::Once::new();
                    FIRST.call_once(||tracing::info!(textures=resources.len(),"VULKAN_GUEST_TEXTURE_BARRIER_OK"));
                },

                Op::Sync=>{
                    let _=call(Task::Sync);
                    static FIRST:std::sync::Once=std::sync::Once::new();
                    FIRST.call_once(||println!("VULKAN_GUEST_SYNC_OK: native synchronization2 barrier submitted and completed"));
                }
                Op::WriteMarker{target,value,bytes}=>{
                    // Never publish a progress value if Vulkan submission failed.
                    let data=match value{
                        MarkerValue::Immediate(v)=>{let _=call(Task::Sync);*v}
                        MarkerValue::SystemTimestamp=>{
                            let _=call(Task::Sync);
                            let mut ticks=0i64;
                            if unsafe{windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut ticks)}==0{
                                stop("guest system timestamp query failed");
                            }
                            ticks as u64
                        }
                        MarkerValue::GpuTimestamp=>{
                            let Reply::Timestamp(ticks)=call(Task::Timestamp)else{stop("invalid Vulkan timestamp response");};
                            ticks
                        }
                    };
                    let address=match target{
                        MarkerTarget::Buffer{resource,offset}=>range(resource.get(),*offset,*bytes as u64),
                        MarkerTarget::Host(mapping)=>{
                            if !mapping.still_valid(*bytes){stop("guest marker mapping changed before completion");}
                            mapping.address
                        }
                    };
                    if !write_marker_bytes(address,data,*bytes){stop("guest marker destination is not writable");}
                    if matches!(value,MarkerValue::GpuTimestamp){static FIRST_GPU:std::sync::Once=std::sync::Once::new();FIRST_GPU.call_once(||println!("VULKAN_GUEST_GPU_TIMESTAMP_OK: {data} ticks at 100 MHz"));}
                    static FIRST:std::sync::Once=std::sync::Once::new();
                    FIRST.call_once(||println!("VULKAN_GUEST_WRITE_OK: ordered {bytes}-byte marker after Vulkan completion"));
                }
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
// Queue work is owned independently of reset/release of guest command lists.
// Similar to the old Vortex Held queue and shadPS4 cooperative command streams,
// a blocked queue yields without preventing producer queues from running.
struct FenceRef(usize);
impl FenceRef{
    fn new(fence:This)->Self{object::add_ref(fence);Self(fence as usize)}
    fn get(&self)->This{self.0 as This}
}
impl Drop for FenceRef{
    fn drop(&mut self){crate::fence::release(self.get());}
}
enum QueueWork{
    Command(std::sync::Arc<Op>),
    Wait(FenceRef,u64),
    Signal(FenceRef,u64),
    Present(Retained),
}
impl QueueWork{
    fn ready(&self)->bool{
        match self{
            Self::Command(op)=>op.ready(),
            Self::Wait(f,v)=>crate::fence::completed(f.get())>=*v,
            _=>true,
        }
    }
    fn execute(self){
        match self{
            Self::Command(op)=>op.execute(),
            Self::Wait(..)=>{},
            Self::Signal(f,v)=>crate::fence::set(f.get(),v),
            Self::Present(r)=>present(r.get()),
        }
    }
}
type QueueBatch=(u64,Vec<QueueWork>);
fn enqueue(queue:u64,work:Vec<QueueWork>){
    use std::{collections::{BTreeMap,VecDeque},sync::mpsc,time::{Duration,Instant}};
    static SCHEDULER:OnceLock<mpsc::Sender<QueueBatch>>=OnceLock::new();
    let tx=SCHEDULER.get_or_init(||{
        let(tx,rx)=mpsc::channel::<QueueBatch>();
        let _worker=std::thread::Builder::new().name("vortex-guest-queues".into()).spawn(move||{
            let mut pending:BTreeMap<u64,VecDeque<QueueWork>>=BTreeMap::new();
            let mut last_progress=Instant::now();
            loop{
                if pending.is_empty(){
                    let Ok((id,work))=rx.recv()else{return;};
                    pending.entry(id).or_default().extend(work);
                    last_progress=Instant::now();
                }
                for _ in 0..256{
                    let Ok((id,work))=rx.try_recv()else{break;};
                    pending.entry(id).or_default().extend(work);
                }
                let mut progressed=false;
                // At most one command per queue per round; no lock while
                // waiting on memory/fences, and Vulkan worker owns its device.
                for work in pending.values_mut(){
                    if work.front().is_some_and(QueueWork::ready){
                        if let Some(item)=work.pop_front(){item.execute();progressed=true;}
                    }
                }
                pending.retain(|_,work|!work.is_empty());
                if progressed{last_progress=Instant::now();}
                else if !pending.is_empty(){
                    if last_progress.elapsed()>=Duration::from_secs(30){
                        stop(format_args!("guest queue dependency stalled: {} queues; no false completion",pending.len()));
                    }
                    match rx.recv_timeout(Duration::from_millis(1)){
                        Ok((id,work))=>pending.entry(id).or_default().extend(work),
                        Err(mpsc::RecvTimeoutError::Timeout)=>{},
                        Err(mpsc::RecvTimeoutError::Disconnected)=>return,
                    }
                }
            }
        }).unwrap_or_else(|e|stop(e));
        tx
    });
    if tx.send((queue,work)).is_err(){stop("guest queue scheduler stopped");}
}
pub(crate) fn submit(queue:u64,ops:Vec<std::sync::Arc<Op>>){
    enqueue(queue,ops.into_iter().map(QueueWork::Command).collect());
}
pub(crate) fn queue_wait(queue:u64,fence:This,value:u64){enqueue(queue,vec![QueueWork::Wait(FenceRef::new(fence),value)]);}
pub(crate) fn queue_signal(queue:u64,fence:This,value:u64){enqueue(queue,vec![QueueWork::Signal(FenceRef::new(fence),value)]);}
pub(crate) fn queue_present(queue:u64,resource:This){enqueue(queue,vec![QueueWork::Present(Retained::new(resource))]);}
fn write_marker_bytes(address:usize,value:u64,bytes:usize)->bool{
    use windows_sys::Win32::System::{Diagnostics::Debug::WriteProcessMemory,Threading::GetCurrentProcess};
    if !matches!(bytes,4|8)||address==0||address%bytes!=0{return false;}
    let data=value.to_le_bytes();
    let mut actual=0usize;
    let ok=unsafe{WriteProcessMemory(GetCurrentProcess(),address as *mut _,data.as_ptr().cast(),bytes,&mut actual)};
    ok!=0&&actual==bytes
}
#[cfg(test)]
mod marker_tests{
    use super::*;
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc,VirtualFree,MEM_COMMIT,MEM_RESERVE,MEM_RELEASE,PAGE_READWRITE,PAGE_READONLY,
    };
    #[test]
    fn blocked_queue_allows_other_queue_to_signal(){
        let mut a=std::ptr::null();let mut b=std::ptr::null();
        assert_eq!(crate::fence::create(std::ptr::null(),0,0,&crate::fence::FENCE_IIDS[0],&mut a),crate::object::S_OK);
        assert_eq!(crate::fence::create(std::ptr::null(),0,0,&crate::fence::FENCE_IIDS[0],&mut b),crate::object::S_OK);
        queue_wait(u64::MAX-1,a,4);
        queue_signal(u64::MAX-1,b,9);
        queue_signal(u64::MAX,a,4);
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(5);
        while crate::fence::completed(b)!=9&&std::time::Instant::now()<deadline{
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(crate::fence::completed(a),4);
        assert_eq!(crate::fence::completed(b),9);
        crate::fence::release(a);crate::fence::release(b);
    }
    #[test]
    fn retail_wait_comparisons_and_mask(){
        assert!(wait_comparison(3,0x1f,0xf,0xf));
        assert!(!wait_comparison(3,0x1f,0xf,u64::MAX));
        assert!(!wait_comparison(1,7,7,u64::MAX));
        assert!(wait_comparison(7,8,7,u64::MAX));
        assert!(wait_comparison(0,0,99,u64::MAX));
    }
    #[test]
    fn host_marker_requires_writable_mapping_and_preserves_adjacent_word(){
        unsafe{
            let p=VirtualAlloc(std::ptr::null(),4096,MEM_COMMIT|MEM_RESERVE,PAGE_READWRITE);
            assert!(!p.is_null());
            let address=p as usize;
            let marker=HostMarker::query(address as u64,4).unwrap();
            assert!(marker.still_valid(4));
            assert!(HostMarker::query(address as u64+1,4).is_none());
            assert!(write_marker_bytes(address,0x12345678,4));
            assert_eq!(p.cast::<u32>().read(),0x12345678);
            assert_eq!(p.cast::<u32>().add(1).read(),0);
            assert!(write_marker_bytes(address,0x123456789abcdef0,8));
            assert_eq!(p.cast::<u64>().read(),0x123456789abcdef0);
            assert_ne!(VirtualFree(p,0,MEM_RELEASE),0);
            assert!(!marker.still_valid(4));
            let q=VirtualAlloc(std::ptr::null(),4096,MEM_COMMIT|MEM_RESERVE,PAGE_READONLY);
            assert!(!q.is_null());
            assert!(HostMarker::query(q as u64,4).is_none());
            assert_ne!(VirtualFree(q,0,MEM_RELEASE),0);
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
