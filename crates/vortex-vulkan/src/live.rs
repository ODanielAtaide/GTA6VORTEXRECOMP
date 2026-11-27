// SPDX-License-Identifier: GPL-3.0-or-later
//! Live guest GPU bridge. One worker owns the AMD device, command session
//! and Win32 swapchain. No guest pointers cross the worker boundary.
use std::{collections::BTreeMap,sync::mpsc};
use crate::{Context,commands::{self,BufferId,Command,ImageId,ImageRegion,MemoryKind},present::Presenter};

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ImageSpec { pub key:usize,pub width:u32,pub height:u32,pub depth:bool }
#[derive(Debug)]
pub enum Task {
    CopyBytes(Vec<u8>),
    ClearColor{image:ImageSpec,rgba:[f32;4]},
    ClearDepth{image:ImageSpec,z:f32},
    CopyImage{src:ImageSpec,dst:ImageSpec},
    Forget(usize),
    Present{image:ImageSpec,hwnd:usize,hinstance:usize},
}
pub enum Reply { Done, Bytes(Vec<u8>) }
type Answer=std::result::Result<Reply,String>;
type Request=(Task,mpsc::SyncSender<Answer>);
pub struct Live { tx:mpsc::Sender<Request> }
impl Live {
    pub fn start()->std::result::Result<Self,String>{
        let(tx,rx)=mpsc::channel::<Request>();
        let(init,ready)=mpsc::sync_channel::<std::result::Result<(),String>>(1);
        std::thread::Builder::new().name("vortex-live-vulkan".into()).spawn(move||{
            let context=match Context::new_live(){Ok(c)=>c,Err(e)=>{let _=init.send(Err(e.to_string()));return;}};
            let mut session=match commands::Session::new(&context){Ok(s)=>s,Err(e)=>{let _=init.send(Err(e.to_string()));return;}};
            let mut presenter:Option<Presenter<'_>>=None;
            let mut images:BTreeMap<usize,(ImageSpec,ImageId)>=BTreeMap::new();
            let mut scratch=None;
            if init.send(Ok(())).is_err(){return;}
            while let Ok((task,reply))=rx.recv(){
                let answer=run(&context,&mut session,&mut presenter,&mut images,&mut scratch,task).map_err(|e|e.to_string());
                let failed=answer.is_err();
                let _=reply.send(answer);
                if failed{break;}
            }
        }).map_err(|e|e.to_string())?;
        ready.recv().map_err(|e|e.to_string())??;
        Ok(Self{tx})
    }
    pub fn call(&self,task:Task)->Answer{
        let(tx,rx)=mpsc::sync_channel(1);
        self.tx.send((task,tx)).map_err(|_|"Vulkan worker unavailable".to_owned())?;
        rx.recv().map_err(|_|"Vulkan worker stopped".to_owned())?
    }
}
const CHUNK:usize=4*1024*1024;
fn image(s:&mut commands::Session<'_>,map:&mut BTreeMap<usize,(ImageSpec,ImageId)>,spec:ImageSpec,create:bool)->crate::Result<ImageId>{
    if let Some(&(old,id))=map.get(&spec.key){
        if old!=spec{return Err("guest image identity changed".into());}
        return Ok(id);
    }
    if !create{return Err("guest image has not been produced by Vulkan commands".into());}
    let id=if spec.depth{s.create_depth_image(spec.width,spec.height)?}else{s.create_image(spec.width,spec.height)?};
    map.insert(spec.key,(spec,id));Ok(id)
}
fn run<'a>(context:&'a Context,s:&mut commands::Session<'_>,presenter:&mut Option<Presenter<'a>>,
    map:&mut BTreeMap<usize,(ImageSpec,ImageId)>,scratch:&mut Option<[BufferId;3]>,task:Task)->crate::Result<Reply>{
    match task{
        Task::CopyBytes(bytes)=>{
            if bytes.is_empty()||bytes.len()>CHUNK{return Err("invalid live copy size".into());}
            if scratch.is_none(){
                *scratch=Some([s.create_buffer(CHUNK as u64,MemoryKind::HostVisible)?,
                    s.create_buffer(CHUNK as u64,MemoryKind::DeviceLocal)?,
                    s.create_buffer(CHUNK as u64,MemoryKind::HostVisible)?]);
            }
            let [upload,local,read]=(*scratch).ok_or("copy scratch missing")?;
            let len=bytes.len();let mut padded=bytes;padded.resize(len.next_multiple_of(4),0);
            let words:Vec<u32>=padded.chunks_exact(4).map(|p|u32::from_le_bytes([p[0],p[1],p[2],p[3]])).collect();
            s.write_words(upload,0,&words)?;
            let n=padded.len() as u64;
            s.submit(&[Command::Copy{src:upload,src_offset:0,dst:local,dst_offset:0,bytes:n},
                Command::Copy{src:local,src_offset:0,dst:read,dst_offset:0,bytes:n}])?;
            let mut output:Vec<u8>=s.read_words(read,0,words.len())?.into_iter().flat_map(u32::to_le_bytes).collect();
            output.truncate(len);return Ok(Reply::Bytes(output));
        }
        Task::ClearColor{image:spec,rgba}=>{
            if spec.depth{return Err("color clear on depth image".into());}
            let dst=image(s,map,spec,true)?;
            s.submit(&[Command::ClearImage{dst,rgba}])?;
        }
        Task::ClearDepth{image:spec,z}=>{
            if !spec.depth{return Err("depth clear on color image".into());}
            let dst=image(s,map,spec,true)?;
            s.submit(&[Command::ClearDepth{dst,depth:z}])?;
        }
        Task::CopyImage{src,dst}=>{
            if (src.width,src.height,src.depth)!=(dst.width,dst.height,dst.depth)||src.key==dst.key{
                return Err("incompatible live image copy".into());
            }
            let from=image(s,map,src,false)?;let to=image(s,map,dst,true)?;
            s.submit(&[Command::CopyImage{src:from,dst:to,region:ImageRegion{x:0,y:0,width:src.width,height:src.height},x:0,y:0}])?;
        }
        Task::Forget(key)=>{if let Some((_,id))=map.remove(&key){s.destroy_image(id)?;}}
        Task::Present{image:spec,hwnd,hinstance}=>{
            if spec.depth{return Err("cannot present depth".into());}
            let id=image(s,map,spec,false)?;
            let(handle,w,h)=s.present_source(id)?;
            if presenter.as_ref().is_none_or(|p|p.hwnd()!=hwnd){
                *presenter=None;
                *presenter=Some(Presenter::new(context,hwnd,hinstance,w,h)?);
            }
            presenter.as_mut().ok_or("presenter missing")?.present(handle,w,h)?;
        }
    }
    Ok(Reply::Done)
}
