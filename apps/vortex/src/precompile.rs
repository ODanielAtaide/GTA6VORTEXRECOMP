// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only offline coverage survey of local, authorized shader inputs.
//! No game files are changed, no encryption or access controls are handled,
//! no game code is executed. Fixture-ABI SPIR-V is never a guest pipeline.
use anyhow::{Context,Result};
use serde::{Deserialize,Serialize};
use std::{collections::BTreeMap,fs::{self,File},io::{Read,Seek,SeekFrom,Write,BufWriter},path::{Path,PathBuf},
    sync::{Mutex,atomic::{AtomicUsize,Ordering}},time::{SystemTime,UNIX_EPOCH}};
use vortex_shader::{Container,FourCC,HwStage,ShaderError,ir,gfx10,resource,structure,io as stage_io};

const BLOCK:usize=8*1024*1024;
const MAX_CONTAINER:usize=32*1024*1024;
const MAX_PACKED_FILE:u64=128*1024*1024;
const SCHEMA:&str="offline-v2";
type Counts=BTreeMap<String,u64>;

#[derive(Default,Serialize,Deserialize)]
struct Analysis {
    programs:u64,decoded:u64,ir_ok:u64,resources_ok:u64,io_ok:u64,structured:u64,
    candidate:u64,validated:u64,
    stages:Counts,opcodes:Counts,gaps:Counts,
    #[serde(default)]
    instruction_gaps:Counts,
}
impl Analysis {
    fn gap(&mut self,key:impl Into<String>){*self.gaps.entry(key.into()).or_default()+=1;}
    fn merge(&mut self,b:&Self){
        self.programs+=b.programs;self.decoded+=b.decoded;self.ir_ok+=b.ir_ok;
        self.resources_ok+=b.resources_ok;self.io_ok+=b.io_ok;self.structured+=b.structured;
        self.candidate+=b.candidate;self.validated+=b.validated;
        for (a,b) in [(&mut self.stages,&b.stages),(&mut self.opcodes,&b.opcodes),(&mut self.gaps,&b.gaps),(&mut self.instruction_gaps,&b.instruction_gaps)]{
            for (k,n) in b{*a.entry(k.clone()).or_default()+=n;}
        }
    }
}
fn reason(e:&ShaderError)->String{
    match e{
        ShaderError::Unsupported{what,..}=>what.to_string(),
        ShaderError::Isa{reason,..}=>reason.to_string(),
        _=>e.to_string(),
    }
}
fn length(header:&[u8])->Option<usize>{
    if header.get(..4)!=Some(b"DXBC"){return None;}
    let n=u32::from_le_bytes(header.get(24..28)?.try_into().ok()?) as usize;
    (32..=MAX_CONTAINER).contains(&n).then_some(n)
}
fn atomic_write(path:&Path,bytes:&[u8])->Result<()>{
    vortex_core::ensure_outside_repo(path)?;
    let parent=path.parent().context("output has no parent")?;
    let mut temp=tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e|e.error)?;
    Ok(())
}
fn digest_file(path:&Path)->Result<String>{
    let mut file=File::open(path)?;let mut h=blake3::Hasher::new();
    let mut buffer=vec![0u8;BLOCK];
    loop{let n=file.read(&mut buffer)?;if n==0{break;}h.update(&buffer[..n]);}
    Ok(h.finalize().to_hex().to_string())
}
struct Shared {
    cache:PathBuf, validate:bool,refresh:bool,
    seen:Mutex<BTreeMap<String,String>>,
    totals:Mutex<Analysis>,
    duplicates:AtomicUsize, hits:AtomicUsize, unique:AtomicUsize,
}
impl Shared {
    fn analyze(&self,bytes:&[u8],origin:String)->Result<()>{
        let Some(n)=length(bytes)else{return Ok(());};
        let Some(bytes)=bytes.get(..n)else{return Ok(());};
        let Ok(c)=Container::parse(bytes)else{return Ok(());};
        let hash=blake3::hash(bytes).to_hex().to_string();
        {
            let mut seen=self.seen.lock().unwrap_or_else(|e|e.into_inner());
            if seen.contains_key(&hash){self.duplicates.fetch_add(1,Ordering::Relaxed);return Ok(());}
            seen.insert(hash.clone(),origin);
        }
        self.unique.fetch_add(1,Ordering::Relaxed);
        let path=self.cache.join(format!("{hash}.toml"));
        if !self.refresh{
            if let Ok(raw)=fs::read_to_string(&path){
                if let Ok(a)=toml::from_str::<Analysis>(&raw){
                    self.hits.fetch_add(1,Ordering::Relaxed);
                    self.totals.lock().unwrap_or_else(|e|e.into_inner()).merge(&a);
                    return Ok(());
                }
            }
        }
        let mut a=Analysis::default();
        match c.part(FourCC::DXIL).map(|part|vortex_shader::native_programs(part.data)){
            None=>a.gap("container: no DXIL part"),
            Some(Err(e))=>a.gap(format!("native parse: {}",reason(&e))),
            Some(Ok(programs)) if programs.is_empty()=>a.gap("container: no native programs"),
            Some(Ok(programs))=>{
                for (index,p) in programs.iter().enumerate(){
                    a.programs+=1;
                    *a.stages.entry(format!("{:?}",p.stage)).or_default()+=1;
                    let code=&p.code;
                    match gfx10::decode(code){
                        Ok(decoded)=>{
                            a.decoded+=1;
                            for i in decoded{
                                let name=i.name().map(str::to_owned).unwrap_or_else(||format!("{:?}:{:#x}",i.encoding,i.opcode));
                                *a.opcodes.entry(name).or_default()+=1;
                            }
                        }
                        Err(e)=>a.gap(format!("decode: {}",reason(&e))),
                    }
                    let program=match ir::build(code,p.wave64,p.vgprs){
                        Ok(v)=>{a.ir_ok+=1;v}
                        Err(e)=>{a.gap(format!("IR: {}",reason(&e)));continue;}
                    };
                    if p.stage==HwStage::Cs{
                        for (key,n) in vortex_shader::spirv_cfg::diagnose_instructions(&program){
                            *a.instruction_gaps.entry(key).or_default()+=n;
                        }
                    }
                    let structured=structure::structure(&program);
                    match &structured{
                        Ok(_)=>a.structured+=1,
                        Err(e)=>a.gap(format!("structure: {}",reason(e))),
                    }
                    match resource::track(&program,&p.layout,p.stage,structured.as_ref().ok()){
                        Ok(_)=>a.resources_ok+=1,
                        Err(e)=>a.gap(format!("resources: {}",reason(&e))),
                    }
                    match stage_io::raise(&program,p){
                        Ok(_)=>a.io_ok+=1,
                        Err(e)=>a.gap(format!("stage I/O: {}",reason(&e))),
                    }
                    let words=match p.stage{
                        HwStage::Cs=>vortex_shader::spirv_cfg::emit_cfg(&program),
                        HwStage::Vs=>vortex_shader::spirv_graphics::emit_graphics(&program,vortex_shader::spirv_graphics::Stage::Vertex),
                        HwStage::Ps=>vortex_shader::spirv_graphics::emit_graphics(&program,vortex_shader::spirv_graphics::Stage::Fragment),
                        _=>{a.gap(format!("SPIR-V stage unsupported: {:?}",p.stage));continue;}
                    };
                    match words{
                        Ok(words)=>{
                            a.candidate+=1;
                            if self.validate{
                                // A validator checks module structure, not guest semantics.
                                // Use an ephemeral file outside the repository. No raw shader
                                // extraction, persistent SPIR-V or pipeline binaries.
                                let mut temp=tempfile::Builder::new().suffix(".spv").tempfile_in(&self.cache)?;
                                for w in words{temp.write_all(&w.to_le_bytes())?;}
                                temp.flush()?;
                                let output=std::process::Command::new("spirv-val").arg("--target-env").arg("vulkan1.2").arg(temp.path()).output()?;
                                if output.status.success(){a.validated+=1;}
                                else{a.gap(format!("spirv-val: rejected {:?} candidate",p.stage));}
                            }
                        }
                        Err(e)=>{
                            let opcode=match &e{
                                ShaderError::Unsupported{offset,..}|ShaderError::Isa{offset,..}=>program.blocks.iter().flat_map(|b|&b.insts)
                                    .find(|i|i.machine.offset==*offset).map(|i|i.name()).unwrap_or("unknown"),
                                _=>"unknown",
                            };
                            a.gap(format!("SPIR-V {:?}: {opcode}: {}",p.stage,reason(&e)));
                        }
                    }
                    let _=index;
                }
            }
        }
        // Cache only the aggregate metadata, not original or translated code.
        atomic_write(&path,toml::to_string(&a)?.as_bytes())?;
        self.totals.lock().unwrap_or_else(|e|e.into_inner()).merge(&a);
        Ok(())
    }
}
#[derive(Default)]
struct FileResult {size:u64,read:u64,markers:u64,accepted:u64,packed:u64,status:String}
fn scan(path:&Path,shared:&Shared,row:&mut FileResult)->Result<()>{
    let mut file=File::open(path)?;
    row.size=file.metadata()?.len();
    let mut lookup=File::open(path)?;
    let mut buffer=vec![0u8;BLOCK+3];let mut carry=0usize;let mut pos=0u64;
    let mut first=true;let mut packed=false;
    loop{
        let n=file.read(&mut buffer[carry..carry+BLOCK])?;
        if n==0{break;}
        if first{packed=n>=4&&&buffer[..4]==b"SGD5";first=false;}
        row.read+=n as u64;
        let len=carry+n;
        for at in memchr::memmem::find_iter(&buffer[..len],b"DXBC"){
            let off=pos-carry as u64+at as u64;
            row.markers+=1;
            let mut header=[0u8;32];
            lookup.seek(SeekFrom::Start(off))?;
            if lookup.read_exact(&mut header).is_err(){continue;}
            let Some(total)=length(&header)else{continue;};
            if off.checked_add(total as u64).is_none_or(|end|end>row.size){continue;}
            let mut bytes=vec![0u8;total];bytes[..32].copy_from_slice(&header);
            if lookup.read_exact(&mut bytes[32..]).is_err(){continue;}
            if Container::parse(&bytes).is_err(){continue;}
            row.accepted+=1;shared.analyze(&bytes,format!("{}@{off:#x}",path.display()))?;
        }
        carry=len.min(3);buffer.copy_within(len-carry..len,0);pos+=n as u64;
    }
    // Only files explicitly identified as the supported SGD5 format.
    // Opaque archives are scanned for visible DXBC, never decrypted or unpacked.
    if packed{
        if row.size<=MAX_PACKED_FILE{
            let data=fs::read(path)?;
            for record in vortex_game::sgd5::records(&data){
                if let Some(bytes)=record.unpack(){
                    if length(&bytes).is_some_and(|n|n<=bytes.len())&&Container::parse(&bytes).is_ok(){
                        row.packed+=1;shared.analyze(&bytes,format!("{}@{:#x} (SGD5)",path.display(),record.offset))?;
                    }
                }
            }
        }else{row.status="SGD5 too large for packed-record pass; raw scan only".into();}
    }
    if row.read!=row.size||fs::metadata(path)?.len()!=row.size{
        anyhow::bail!("file size changed during scan");
    }
    if row.status.is_empty(){row.status="raw scan complete; opaque content not decoded".into();}
    Ok(())
}
fn clean(s:&str)->String{s.replace(['\t','\r','\n']," ")}
fn section(w:&mut impl Write,name:&str,values:&Counts)->Result<()>{
    writeln!(w,"\n{name}")?;
    let mut rows:Vec<_>=values.iter().collect();
    rows.sort_by(|a,b|b.1.cmp(a.1).then_with(||a.0.cmp(b.0)));
    for (key,count) in rows{writeln!(w,"{count:>9}  {key}")?;}
    Ok(())
}
pub fn run(root:&Path,workers:usize,validate:bool,refresh:bool,code_only:bool)->Result<()>{
    anyhow::ensure!((1..=8).contains(&workers),"workers must be 1..=8");
    let root=fs::canonicalize(root)?;
    anyhow::ensure!(root.is_dir(),"game-dir must be a directory");
    vortex_core::ensure_outside_repo(&root)?;
    let cfg=vortex_core::Config::load()?;
    // Resolve cache before any output, and refuse writes below the scanned tree.
    let requested=cfg.cache_dir.join("vulkan-precompile");
    // Resolve the existing ancestor before creating directories. Refuse
    // symlink/junction aliases into the source tree or scanned tree.
    let absolute=if requested.is_absolute(){requested}else{std::env::current_dir()?.join(requested)};
    let mut ancestor=absolute.as_path();let mut tail=Vec::new();
    while !ancestor.exists(){
        let name=ancestor.file_name().context("cannot resolve cache location")?;
        tail.push(name.to_os_string());
        ancestor=ancestor.parent().context("cache has no existing parent")?;
    }
    let mut resolved=fs::canonicalize(ancestor)?;
    for name in tail.iter().rev(){resolved.push(name);}
    anyhow::ensure!(!resolved.starts_with(&root),"cache must be outside scanned directory");
    vortex_core::ensure_outside_repo(&resolved)?;
    fs::create_dir_all(&resolved)?;
    let cache_base=fs::canonicalize(resolved)?;
    anyhow::ensure!(!cache_base.starts_with(&root),"cache must be outside scanned directory");
    let exe=std::env::current_exe()?;
    let compiler=digest_file(&exe)?;
    let validator=if validate{
        let out=std::process::Command::new("spirv-val").arg("--version").output().context("spirv-val not found; omit --validate or install Vulkan SDK")?;
        anyhow::ensure!(out.status.success(),"spirv-val --version failed");
        blake3::hash(&out.stdout).to_hex().to_string()
    }else{"none".into()};
    let cache=cache_base.join(format!("{SCHEMA}-{compiler}-{validator}"));
    vortex_core::ensure_outside_repo(&cache)?;
    fs::create_dir_all(&cache)?;
    let stamp=SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let report=cache_base.join(format!("report-{stamp}-{}",std::process::id()));
    vortex_core::ensure_outside_repo(&report)?;
    fs::create_dir(&report)?;
    let mut files=Vec::new();let mut walk_errors=Vec::new();let mut excluded=0usize;
    for entry in walkdir::WalkDir::new(&root).follow_links(false){
        match entry{
            Ok(e) if e.file_type().is_file()=>{
                let selected=!code_only||e.path().extension().and_then(|v|v.to_str()).is_some_and(|v|
                    ["exe","dll","sgd5"].iter().any(|ext|v.eq_ignore_ascii_case(ext)));
                if selected{files.push(e.into_path());}else{excluded+=1;}
            }
            Ok(_)=>{},
            Err(e)=>walk_errors.push(e.to_string()),
        }
    }
    files.sort();
    println!("VULKAN_PRECOMPILE_START files={} excluded={excluded} code_only={code_only} workers={workers} report={}",files.len(),report.display());
    let shared=Shared{cache,validate,refresh,seen:Mutex::new(BTreeMap::new()),totals:Mutex::new(Analysis::default()),
        duplicates:AtomicUsize::new(0),hits:AtomicUsize::new(0),unique:AtomicUsize::new(0)};
    let next=AtomicUsize::new(0);
    let results=Mutex::new(Vec::new());
    std::thread::scope(|scope|{
        for _ in 0..workers{
            scope.spawn(||{
                loop{
                    let i=next.fetch_add(1,Ordering::Relaxed);
                    let Some(path)=files.get(i)else{break;};
                    let mut row=FileResult::default();
                    if let Err(e)=scan(path,&shared,&mut row){row.status=format!("ERROR: {e:#}");}
                    println!("VULKAN_PRECOMPILE_FILE {}/{} bytes={} containers={} packed={} {}",i+1,files.len(),row.read,row.accepted,row.packed,path.file_name().unwrap_or_default().to_string_lossy());
                    results.lock().unwrap_or_else(|e|e.into_inner()).push((i,row));
                }
            });
        }
    });
    let mut rows=results.into_inner().unwrap_or_else(|e|e.into_inner());rows.sort_by_key(|r|r.0);
    let mut inventory=BufWriter::new(File::create(report.join("files.tsv"))?);
    writeln!(inventory,"file\tsize\tbytes_read\traw_markers\traw_containers\tpacked_containers\tstatus")?;
    let(mut total_bytes,mut read_bytes,mut raw,mut packed,mut errors)=(0u64,0u64,0u64,0u64,0usize);
    for (i,row) in rows{
        total_bytes+=row.size;read_bytes+=row.read;raw+=row.accepted;packed+=row.packed;
        if row.status.starts_with("ERROR:"){errors+=1;}
        let relative=files[i].strip_prefix(&root).unwrap_or(&files[i]);
        writeln!(inventory,"{}\t{}\t{}\t{}\t{}\t{}\t{}",clean(&relative.to_string_lossy()),row.size,row.read,row.markers,row.accepted,row.packed,clean(&row.status))?;
    }
    inventory.flush()?;
    let mut index=BufWriter::new(File::create(report.join("containers.tsv"))?);
    writeln!(index,"blake3\tfirst_location\tmetadata_cached")?;
    let mut modules:BTreeMap<String,Analysis>=BTreeMap::new();
    for (hash,origin) in shared.seen.into_inner().unwrap_or_else(|e|e.into_inner()){
        let name=origin.rsplit_once('@').map_or(origin.as_str(),|(path,_)|path);
        if let Ok(raw)=fs::read_to_string(shared.cache.join(format!("{hash}.toml"))){
            if let Ok(a)=toml::from_str::<Analysis>(&raw){
                modules.entry(name.to_owned()).or_default().merge(&a);
            }
        }
        writeln!(index,"{hash}\t{}\t{}",clean(&origin),shared.cache.join(format!("{hash}.toml")).is_file())?;
    }
    index.flush()?;
    let totals=shared.totals.into_inner().unwrap_or_else(|e|e.into_inner());
    let unique=shared.unique.load(Ordering::Relaxed);let duplicates=shared.duplicates.load(Ordering::Relaxed);let hits=shared.hits.load(Ordering::Relaxed);
    let mut w=BufWriter::new(File::create(report.join("report.txt"))?);
    writeln!(w,"VULKAN OFFLINE COVERAGE (not playable compatibility)\ncode_only={code_only} excluded_files={excluded}\nfiles={} read_errors={errors} walk_errors={}\nfile_bytes={total_bytes} scanned_bytes={read_bytes}\nraw_containers={raw} packed_containers={packed} unique_containers={unique} duplicates={duplicates} metadata_cache_hits={hits}\nprograms={} decoded={} IR={} resource_tracking={} stage_io={} structured={}\ndiagnostic_SPIRV_candidates={} validator_passed={} guest_pipelines_ready=0\nOpaque/unsupported archives and runtime-created pipeline states are not resolved. Candidate modules use fixture ABI and must not execute guest work.",
        files.len(),walk_errors.len(),totals.programs,totals.decoded,totals.ir_ok,totals.resources_ok,totals.io_ok,totals.structured,totals.candidate,totals.validated)?;
    for e in &walk_errors{writeln!(w,"walk error: {}",clean(e))?;}
    section(&mut w,"Hardware stages",&totals.stages)?;
    section(&mut w,"Incompatibilities (counts by failing analysis step, not unique shaders)",&totals.gaps)?;
    section(&mut w,"Instruction frequency",&totals.opcodes)?;
    section(&mut w,"Compute instruction-local lowering gaps (not full-program compatibility)",&totals.instruction_gaps)?;
    let mut module_report=BufWriter::new(File::create(report.join("modules.txt"))?);
    for (path,a) in modules{
        writeln!(module_report,"\nMODULE {}\nprograms={} IR={} resource_tracking={} candidates={} validated={}",
            clean(&path),a.programs,a.ir_ok,a.resources_ok,a.candidate,a.validated)?;
        section(&mut module_report,"Stages",&a.stages)?;
        section(&mut module_report,"Full-program first failures",&a.gaps)?;
        section(&mut module_report,"Compute instruction-local gaps",&a.instruction_gaps)?;
    }
    module_report.flush()?;
    w.flush()?;
    atomic_write(&report.join("analysis.toml"),toml::to_string(&totals)?.as_bytes())?;
    println!("VULKAN_PRECOMPILE_DONE files={} bytes_read={read_bytes} unique={unique} IR={} candidates={} validated={} guest_pipelines_ready=0 errors={errors} report={}",files.len(),totals.ir_ok,totals.candidate,totals.validated,report.display());
    Ok(())
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn bounds_container_length(){
        let mut h=[0u8;32];h[..4].copy_from_slice(b"DXBC");
        h[24..28].copy_from_slice(&32u32.to_le_bytes());assert_eq!(length(&h),Some(32));
        h[24..28].copy_from_slice(&u32::MAX.to_le_bytes());assert_eq!(length(&h),None);
        assert_eq!(length(b"DXBC"),None);
    }
    #[test]fn scan_boundary_and_duplicate(){
        let temp=tempfile::tempdir().unwrap();
        let cache=temp.path().join("cache");fs::create_dir(&cache).unwrap();
        let path=temp.path().join("fixture.bin");
        let mut bytes=vec![0u8;BLOCK-2];let mut h=[0u8;32];
        h[..4].copy_from_slice(b"DXBC");h[24..28].copy_from_slice(&32u32.to_le_bytes());
        bytes.extend(h);bytes.extend(h);
        fs::write(&path,&bytes).unwrap();
        let shared=Shared{cache,validate:false,refresh:false,seen:Mutex::new(BTreeMap::new()),
            totals:Mutex::new(Analysis::default()),duplicates:AtomicUsize::new(0),
            hits:AtomicUsize::new(0),unique:AtomicUsize::new(0)};
        let mut row=FileResult::default();scan(&path,&shared,&mut row).unwrap();
        assert_eq!(row.accepted,2);assert_eq!(row.read,bytes.len() as u64);
        assert_eq!(shared.unique.load(Ordering::Relaxed),1);
        assert_eq!(shared.duplicates.load(Ordering::Relaxed),1);
    }
    #[test]fn metadata_roundtrip(){
        let mut a=Analysis{programs:2,ir_ok:1,..Default::default()};
        a.gap("SPIR-V Cs: unsupported");let text=toml::to_string(&a).unwrap();
        let b:Analysis=toml::from_str(&text).unwrap();
        a.merge(&b);assert_eq!(a.programs,4);assert_eq!(a.gaps["SPIR-V Cs: unsupported"],2);
    }
}
