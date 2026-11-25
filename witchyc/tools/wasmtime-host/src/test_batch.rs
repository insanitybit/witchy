//! Private framed transport from the trusted package supervisor. Never reads ambient guest input.
use super::*;
use std::{collections::BTreeMap, path::Path, time::Instant};

pub(super) struct Invocation<'a> {
    pub module: &'a [u8],
    pub edges: usize,
    pub feedback: &'a mut std::collections::BTreeSet<u16>,
    pub input: Vec<u8>,
    pub dirs: Vec<(Dir, u8)>,
    /// Epoch ticks (25 ms each) a case may run before it is interrupted.
    pub ticks: u64,
}

fn byte(input: &mut impl Read) -> std::io::Result<u8> {
    let mut b = [0];
    input.read_exact(&mut b)?;
    Ok(b[0])
}
fn word(input: &mut impl Read) -> std::io::Result<u64> {
    let mut b = [0; 8];
    input.read_exact(&mut b)?;
    Ok(u64::from_be_bytes(b))
}
fn frame(input: &mut impl Read, limit: usize) -> wasmtime::Result<Vec<u8>> {
    let mut size = [0; 4];
    input.read_exact(&mut size)?;
    let len = u32::from_be_bytes(size) as usize;
    if len > limit {
        return Err(reject("test request exceeds bound"));
    }
    let mut v = vec![0; len];
    input.read_exact(&mut v)?;
    Ok(v)
}

/// A scheduling store can ask its supervisor to bind the collected descriptor. Case
/// stores see only EOF here. The reply is one status byte, never a path or guest handle.
pub(super) fn binding_reply(commands: &mut Vec<u8>) -> wasmtime::Result<i64> {
    let mut out = std::io::stdout().lock();
    out.write_all(&[128])?;
    out.write_all(&(commands.len() as u32).to_be_bytes())?;
    out.write_all(commands)?;
    commands.clear();
    out.flush()?;
    Ok(i64::from(byte(&mut std::io::stdin().lock())?))
}

pub fn serve() -> wasmtime::Result<()> {
    let mut pins = std::env::args().skip(2).collect::<Vec<_>>();
    // `--case-deadline-ms=N` first, if at all: how long one case may run (100 ms unless said),
    // in whole ticks of the 25 ms epoch timer.
    let mut deadline_ms = 100u64;
    if let Some(text) = pins.first().and_then(|first| first.strip_prefix("--case-deadline-ms=")) {
        deadline_ms = text.parse::<u64>().ok().filter(|ms| (25..=4000).contains(ms)).ok_or_else(|| reject("--case-deadline-ms takes 25 to 4000"))?;
        pins.remove(0);
    }
    let ticks = deadline_ms.div_ceil(25);
    if pins.len() % 4 != 0 || pins.len() > 1024 {
        return Err(reject("expected module, digest, index and coverage count"));
    }
    let mut images = Vec::new();
    for triple in pins.chunks_exact(4) {
        let mut bytes = Vec::new();
        std::fs::File::open(&triple[0])?
            .take(16_777_217)
            .read_to_end(&mut bytes)?;
        let digest: String = sha::sha256(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let marker = [b"\0\x4d\x0cwitchy.tests".as_slice(), triple[2].as_bytes()].concat();
        if digest != triple[1] || triple[2].len() != 64 || !bytes.ends_with(&marker) {
            return Err(reject("test image or index mismatch"));
        }
        bytes.truncate(bytes.len() - marker.len());
        let edges: usize = triple[3].parse()?;
        if edges > 4096 {
            return Err(reject("guest edge map too large"));
        }
        images.push((bytes, edges));
    }
    let mut input = std::io::stdin();
    let mut output = std::io::stdout().lock();
    let mut engines = BTreeMap::new();
    let mut modules = BTreeMap::new();
    let fixed = [
        "test",
        "--output",
        // A case that takes `&Allocator` gets a heap: the default memory grant, in a store of
        // its own, so every case starts with an empty heap and none can exhaust another's.
        "--allocator",
        "--output-bytes=8192",
        "--module-bytes=16777216",
    ]
    .map(str::to_owned);
    for _ in 0..65_536 {
        let mut tag = [0];
        if input.read(&mut tag)? == 0 {
            return Ok(());
        }
        let group = tag[0] as usize;
        let (module, edges) = images.get(group).ok_or_else(|| reject("unknown module"))?;
        let count = byte(&mut input)?;
        if count > 10 {
            return Err(reject("too many test directory slots"));
        }
        let mut dirs = Vec::new();
        let mut denied = None;
        for index in 0..count {
            let right = byte(&mut input)?;
            let expected = (word(&mut input)?, word(&mut input)?);
            let path = frame(&mut input, 4096)?;
            let dir = open_without_links(Path::new(std::str::from_utf8(&path)?));
            match dir {
                Ok(dir) if right <= 1 && identity(&dir.dir_metadata()?) == expected => {
                    dirs.push((dir, right))
                }
                _ => {
                    denied.get_or_insert(index);
                }
            }
        }
        let data = frame(&mut input, 65_536 + 81)?;
        let start = Instant::now();
        let mut compiled = 0u8;
        // Compiling the module is the suite's start-up, paid once by whichever case comes
        // first: timed apart, and not charged to that case.
        let mut compiling = std::time::Duration::ZERO;
        let mut compile = |unmetered, reservation, bytes: &[u8]| {
            if let Some(pair) = modules.get(&group) {
                return Ok(Clone::clone(pair));
            }
            let began = Instant::now();
            let engine = match engines.entry(reservation) {
                std::collections::btree_map::Entry::Occupied(v) => v.into_mut(),
                std::collections::btree_map::Entry::Vacant(v) => {
                    let mut config = engine_config(unmetered, reservation, true);
                    config.epoch_interruption(true);
                    let engine = Engine::new(&config)?;
                    let timer = engine.weak();
                    std::thread::spawn(move || {
                        loop {
                            std::thread::sleep(std::time::Duration::from_millis(25));
                            let Some(engine) = timer.upgrade() else { break };
                            engine.increment_epoch();
                        }
                    });
                    v.insert(engine)
                }
            };
            let pair = (engine.clone(), Module::new(&*engine, bytes)?);
            modules.insert(group, pair.clone());
            compiled = 1;
            compiling = began.elapsed();
            Ok(pair)
        };
        let mut feedback = Default::default();
        let (status, result) = if let Some(index) = denied {
            (3, vec![index])
        } else {
            match execute(
                &fixed,
                Default::default(),
                &mut compile,
                Some(Invocation {
                    module,
                    edges: *edges,
                    feedback: &mut feedback,
                    input: data,
                    dirs,
                    ticks,
                }),
            ) {
                Ok(v) => (0, v),
                Err(e)
                    if matches!(
                        e.downcast_ref::<wasmtime::Trap>(),
                        Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::Interrupt)
                    ) =>
                {
                    (2, vec![])
                }
                Err(e) if e.downcast_ref::<wasmtime::Trap>().is_some() => (1, vec![]),
                Err(e) => return Err(e),
            }
        };
        output.write_all(&[status, compiled])?;
        output.write_all(&(start.elapsed().saturating_sub(compiling).as_nanos() as u64).to_be_bytes())?;
        output.write_all(&(result.len() as u32).to_be_bytes())?;
        output.write_all(&(feedback.len() as u16).to_be_bytes())?;
        for id in feedback {
            output.write_all(&id.to_be_bytes())?;
        }
        output.write_all(&result)?;
        output.flush()?;
    }
    Err(reject("test session request limit exceeded"))
}
