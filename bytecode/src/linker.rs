use std::collections::HashMap;
use crate::*;
use std::io::{Seek, Write, Read, SeekFrom};
use std::ops::RangeInclusive;
use range_set::RangeSet;
use smallvec::{smallvec, SmallVec};

#[derive(Debug)]
pub enum LinkError {
    Io(std::io::Error),
    ByteCode(ByteCodeError),
    VersionMismatch,
    SymbolDefinedTwice(String),
    SymbolNotFound(String),
}

impl From<std::io::Error> for LinkError {
    fn from(e: std::io::Error) -> Self {
        LinkError::Io(e)
    }
}

impl From<ByteCodeError> for LinkError {
    fn from(e: ByteCodeError) -> Self {
        LinkError::ByteCode(e)
    }
}

/// after concatenating all files into one, run self_link
pub fn cat_together<W: Write + Seek + Read>(output: &mut W, input: &[u8]) -> Result<(), LinkError> {
    let input = Bytecode::try_from(input)?;

    let mut out_header = [0_u8; 16];
    output.seek(SeekFrom::Start(0))?;
    output.read_exact(&mut out_header)?;
    let out_header = Header::try_from(out_header.as_slice())?;
    if out_header.writer_version != input.header.writer_version {
        return Err(LinkError::VersionMismatch)
    }

    output.seek(SeekFrom::Start(16 + out_header.globals_tab_off as u64))?;
    let mut out_rem = vec!();
    output.read_to_end(&mut out_rem)?;

    output.seek(SeekFrom::Start(16 + out_header.globals_tab_off as u64))?;
    output.write_all(input.data_table())?;

    let new_globals_begin = output.stream_position()?;
    let new_globals_len = input.header.globals_tab_num + out_header.globals_tab_num;
    output.write_all(&out_rem[..out_header.globals_tab_num as usize * 8])?;
    output.write_all(input.globals_table())?;

    output.write_all(&out_rem[out_header.globals_tab_num as usize * 8..])?;
    output.write_all(input.main_ops_area())?;

    output.seek(SeekFrom::Start(0))?;
    Header {
        globals_tab_num: new_globals_len,
        globals_tab_off: new_globals_begin as u32,
        ..out_header
    }.write(output)?;
    Ok(())
}

pub trait Target {
    fn allow_undeclared_symbol(&self, sym: &str) -> bool;
}

pub fn self_link<T: Target>(bin: &mut [u8], target: &T) -> Result<(), LinkError> {
    let header = Header::try_from(bin.as_ref())?;

    let mut decls = HashMap::new();
    for decl in Bytecode::from_header(bin, header.clone()).named_globals() {
        let (name,val) = decl?;
        if decls.contains_key(name) {
            Err(LinkError::SymbolDefinedTwice(name.to_string()))?;
        }
        decls.insert(unsafe{ &*(name as *const str) }, val);
    }

    let mut done = RangeSet::<[RangeInclusive<usize>;16]>::new();

    let mut todo = vec!(
        Bytecode::from_header(bin, header.clone()).main_ops_area().as_ptr().addr() - bin.as_ptr().addr());
    todo.extend(decls.iter().map(|x| (*x.1 + 1) as usize));

    while let Some(off) = todo.pop() {
        let mut to_write: SmallVec<(usize,u32), 16> = smallvec!();
        for op in OpsIter::new(off, &bin[off..]) {
            let (pos, op) = op?;
            done.insert(pos);
            match op {
                Op::Unresolved { id } => {
                    let str = Bytecode::from_header(bin, header.clone()).string(id)?;
                    match decls.get(str) {
                        Some(decl_pos) => {
                            to_write.push((pos, *decl_pos));
                        }

                        None => {
                            if !target.allow_undeclared_symbol(str) {
                                Err(LinkError::SymbolNotFound(str.to_string()))?;
                            }
                        }
                    }
                }

                Op::Const { idx } => {
                    if !done.contains(idx as usize) {
                        todo.push(idx as usize);
                    }
                }

                _ => ()
            }
        }

        for (pos,val) in to_write {
            let mut v = vec!();
            Op::Const { idx: val }.write(&mut v)?;
            bin[pos..v.len()].copy_from_slice(v.as_slice());
        }
    }

    Ok(())
}

// TODO: self_gc()