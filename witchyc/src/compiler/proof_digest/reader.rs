//! Bounded read-ahead; only the ordered consumer updates the installation hash.
use super::{check_cancelled, platform::Hash};
use std::{collections::VecDeque, fs::File, io::{self, Read}, path::PathBuf,
    sync::mpsc::{self, Receiver, SyncSender}, thread::{self, JoinHandle}};
const READERS:usize=4;
const CHUNK:usize=65_536;
const QUEUED_CHUNKS:usize=4;
const EVENTS:usize=64;
struct Job {path:PathBuf,length:u64,chunks:SyncSender<io::Result<Vec<u8>>>}
struct Worker {jobs:Option<SyncSender<Job>>,handle:Option<JoinHandle<()>>}
enum Event {Bytes(Vec<u8>),File(usize,Receiver<io::Result<Vec<u8>>>) }
pub(super) struct Ordered<'a>{hash:&'a mut Hash,workers:Vec<Worker>,free:Vec<usize>,events:VecDeque<Event>,metadata:usize}
fn read(job:&Job)->io::Result<()> {
    let mut file=File::open(&job.path)?;let mut remaining=job.length;
    while remaining>0 {
        let mut bytes=vec![0;CHUNK.min(remaining as usize)];let n=file.read(&mut bytes)?;
        if n==0{return Err(io::Error::other("Lean installation changed while reading"));}
        bytes.truncate(n);remaining-=n as u64;
        if job.chunks.send(Ok(bytes)).is_err(){return Ok(());}
    }
    if file.read(&mut [0u8;1])?!=0{return Err(io::Error::other("Lean installation grew while reading"));}Ok(())
}
impl<'a> Ordered<'a>{
    pub fn new(hash:&'a mut Hash)->io::Result<Self>{
        let mut pool=Self{hash,workers:vec![],free:vec![],events:VecDeque::new(),metadata:0};
        for id in 0..READERS {
            let(tx,rx)=mpsc::sync_channel::<Job>(0);
            let handle=thread::Builder::new().name(format!("proof-reader-{id}")).spawn(move||{
                while let Ok(job)=rx.recv(){let result=read(&job);if job.chunks.send(result.map(|()|vec![])).is_err(){continue;}}
            })?;
            pool.workers.push(Worker{jobs:Some(tx),handle:Some(handle)});pool.free.push(id);
        }Ok(pool)
    }
    pub fn update(&mut self,bytes:&[u8])->io::Result<()> {
        while self.events.len()>=EVENTS||self.metadata.saturating_add(bytes.len())>CHUNK {
            if self.events.is_empty(){return self.hash.update(bytes);}self.drain()?;
        }
        self.metadata+=bytes.len();self.events.push_back(Event::Bytes(bytes.to_vec()));Ok(())
    }
    pub fn file(&mut self,path:PathBuf,length:u64)->io::Result<()> {
        while self.free.is_empty()||self.events.len()>=EVENTS{self.drain()?;}
        let id=self.free.pop().expect("available reader");let(tx,rx)=mpsc::sync_channel(QUEUED_CHUNKS);
        self.workers[id].jobs.as_ref().expect("live reader").send(Job{path,length,chunks:tx}).map_err(|_|io::Error::other("Lean installation reader stopped"))?;
        self.events.push_back(Event::File(id,rx));Ok(())
    }
    fn drain(&mut self)->io::Result<()> {
        check_cancelled()?;
        match self.events.pop_front().expect("queued event"){
            Event::Bytes(bytes)=>{self.metadata-=bytes.len();self.hash.update(&bytes)?;},
            Event::File(id, chunks) => {
                loop {
                    check_cancelled()?;
                    let bytes = chunks.recv().map_err(|_| io::Error::other("Lean installation reader stopped"))??;
                    if bytes.is_empty() {
                        break;
                    }
                    self.hash.update(&bytes)?;
                }
                self.free.push(id);
            }
        }Ok(())
    }
    fn shutdown(&mut self)->io::Result<()> {
        // Drop every reply receiver first: blocked sends wake before we join.
        self.events.clear();for worker in &mut self.workers{worker.jobs.take();}
        let mut result=Ok(());for worker in &mut self.workers{if let Some(handle)=worker.handle.take(){if handle.join().is_err(){result=Err(io::Error::other("Lean installation reader panicked"));}}}result
    }
    pub fn finish(mut self)->io::Result<()> {
        let result=(||{while !self.events.is_empty(){self.drain()?;}Ok(())})();let joined=self.shutdown();result.and(joined)
    }
}
impl Drop for Ordered<'_>{fn drop(&mut self){let _=self.shutdown();}}
