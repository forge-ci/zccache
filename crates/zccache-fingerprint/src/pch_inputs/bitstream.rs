//! Minimal LLVM bitstream reader for clang PCH input-file records (#1952).
//!
//! A clang PCH is an LLVM bitstream that starts with the `CPCH` magic. Its
//! `CONTROL_BLOCK` holds an `INPUT_FILES_BLOCK` with one `INPUT_FILE` record
//! (id, size, mtime, flags, spelling) and one `INPUT_FILE_HASH` record (xxh3
//! of the content) per input. Those are the values clang compares when a
//! consumer loads the PCH, so they are read here directly instead of being
//! re-derived from the workspace.
//!
//! Only what that walk needs is implemented: abbreviation definitions (local
//! and from `BLOCKINFO`), unabbreviated and abbreviated records, blobs and
//! arrays, and block skipping by length. Every other block is skipped
//! without being decoded, and the walk stops when the input-file block ends,
//! so only the head of the file is ever read.

use std::collections::HashMap;
use std::path::Path;

use super::{PchInput, PchInputsError};

/// The four-byte magic every clang PCH/PCM starts with.
pub(super) const CLANG_PCH_MAGIC: &[u8; 4] = b"CPCH";

/// `llvm::bitc::FIRST_APPLICATION_BLOCKID` + clang's `BlockIDs` ordinal.
const CONTROL_BLOCK_ID: u64 = 15;
const INPUT_FILES_BLOCK_ID: u64 = 16;
const BLOCKINFO_BLOCK_ID: u64 = 0;

/// `INPUT_FILES_BLOCK` record codes (`clang/Serialization/ASTBitCodes.h`).
const INPUT_FILE: u64 = 1;
const INPUT_FILE_HASH: u64 = 2;

/// `CONTROL_BLOCK` record code for one imported AST file (a chained PCH's
/// base PCH), and clang's `ModuleKind::MK_PCH`.
const IMPORT: u64 = 2;
const METADATA: u64 = 1;
const MODULE_KIND_PCH: u64 = 2;

/// Fixed abbreviation ids.
const END_BLOCK: u64 = 0;
const ENTER_SUBBLOCK: u64 = 1;
const DEFINE_ABBREV: u64 = 2;
const UNABBREV_RECORD: u64 = 3;

/// `BLOCKINFO` record code selecting the block later abbreviations apply to.
const BLOCKINFO_SETBID: u64 = 1;

#[derive(Clone, Debug)]
enum AbbrevOp {
    Literal(u64),
    Fixed(u32),
    Vbr(u32),
    Array,
    Char6,
    Blob,
}

type Abbrev = Vec<AbbrevOp>;

struct Record<'a> {
    code: u64,
    ops: Vec<u64>,
    blob: Option<&'a [u8]>,
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn read(&mut self, width: u32) -> Result<u64, PchInputsError> {
        if width > 64 {
            return Err(malformed("field wider than 64 bits"));
        }
        let mut value = 0u64;
        let mut got = 0u32;
        while got < width {
            let byte = *self
                .data
                .get(self.bit / 8)
                .ok_or(PchInputsError::Truncated)?;
            let offset = (self.bit % 8) as u32;
            let take = (8 - offset).min(width - got);
            let bits = (u64::from(byte) >> offset) & ((1u64 << take) - 1);
            value |= bits << got;
            got += take;
            self.bit += take as usize;
        }
        Ok(value)
    }

    fn read_vbr(&mut self, width: u32) -> Result<u64, PchInputsError> {
        if width < 2 {
            return Err(malformed("VBR width below 2"));
        }
        let high = 1u64 << (width - 1);
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let chunk = self.read(width)?;
            if shift >= 64 {
                return Err(malformed("VBR value overflows 64 bits"));
            }
            value |= (chunk & (high - 1)) << shift;
            if chunk & high == 0 {
                return Ok(value);
            }
            shift += width - 1;
        }
    }

    fn align32(&mut self) {
        self.bit = self.bit.div_ceil(32) * 32;
    }

    fn skip_words(&mut self, words: u64) -> Result<(), PchInputsError> {
        let bits = usize::try_from(words)
            .ok()
            .and_then(|words| words.checked_mul(32))
            .ok_or_else(|| malformed("block length overflows"))?;
        let end = self
            .bit
            .checked_add(bits)
            .ok_or_else(|| malformed("block length overflows"))?;
        if end > self.data.len() * 8 {
            return Err(PchInputsError::Truncated);
        }
        self.bit = end;
        Ok(())
    }

    fn read_bytes(&mut self, len: u64) -> Result<&'a [u8], PchInputsError> {
        let start = self.bit / 8;
        let len = usize::try_from(len).map_err(|_| malformed("blob length overflows"))?;
        let end = start
            .checked_add(len)
            .ok_or_else(|| malformed("blob length overflows"))?;
        let bytes = self.data.get(start..end).ok_or(PchInputsError::Truncated)?;
        self.bit = end * 8;
        Ok(bytes)
    }
}

fn malformed(message: &'static str) -> PchInputsError {
    PchInputsError::Malformed(message)
}

fn read_abbrev_definition(reader: &mut BitReader<'_>) -> Result<Abbrev, PchInputsError> {
    let count = reader.read_vbr(5)?;
    let mut ops = Vec::new();
    let mut index = 0;
    while index < count {
        index += 1;
        if reader.read(1)? == 1 {
            ops.push(AbbrevOp::Literal(reader.read_vbr(8)?));
            continue;
        }
        let op = match reader.read(3)? {
            1 => AbbrevOp::Fixed(width_of(reader.read_vbr(5)?)?),
            2 => AbbrevOp::Vbr(width_of(reader.read_vbr(5)?)?),
            3 => AbbrevOp::Array,
            4 => AbbrevOp::Char6,
            5 => AbbrevOp::Blob,
            _ => return Err(malformed("unknown abbreviation encoding")),
        };
        ops.push(op);
    }
    Ok(ops)
}

fn width_of(value: u64) -> Result<u32, PchInputsError> {
    u32::try_from(value)
        .ok()
        .filter(|width| *width <= 64)
        .ok_or_else(|| malformed("abbreviation width above 64"))
}

fn read_scalar(reader: &mut BitReader<'_>, op: &AbbrevOp) -> Result<u64, PchInputsError> {
    match op {
        AbbrevOp::Literal(value) => Ok(*value),
        AbbrevOp::Fixed(width) => reader.read(*width),
        AbbrevOp::Vbr(0) => Ok(0),
        AbbrevOp::Vbr(width) => reader.read_vbr(*width),
        AbbrevOp::Char6 => reader.read(6),
        AbbrevOp::Array | AbbrevOp::Blob => Err(malformed("aggregate used as a scalar")),
    }
}

fn read_abbreviated<'a>(
    reader: &mut BitReader<'a>,
    abbrev: &Abbrev,
) -> Result<Record<'a>, PchInputsError> {
    let first = abbrev
        .first()
        .ok_or_else(|| malformed("empty abbreviation"))?;
    let code = read_scalar(reader, first)?;
    let mut ops = Vec::new();
    let mut blob = None;
    let mut index = 1;
    while index < abbrev.len() {
        match &abbrev[index] {
            AbbrevOp::Array => {
                let element = abbrev
                    .get(index + 1)
                    .ok_or_else(|| malformed("array without an element type"))?;
                let len = reader.read_vbr(6)?;
                for _ in 0..len {
                    ops.push(read_scalar(reader, element)?);
                }
                index += 2;
            }
            AbbrevOp::Blob => {
                let len = reader.read_vbr(6)?;
                reader.align32();
                blob = Some(reader.read_bytes(len)?);
                reader.align32();
                index += 1;
            }
            scalar => {
                ops.push(read_scalar(reader, scalar)?);
                index += 1;
            }
        }
    }
    Ok(Record { code, ops, blob })
}

fn read_unabbreviated<'a>(reader: &mut BitReader<'a>) -> Result<Record<'a>, PchInputsError> {
    let code = reader.read_vbr(6)?;
    let count = reader.read_vbr(6)?;
    let mut ops = Vec::new();
    for _ in 0..count {
        ops.push(reader.read_vbr(6)?);
    }
    Ok(Record {
        code,
        ops,
        blob: None,
    })
}

/// Header of a sub-block: its id, abbreviation width and length in words.
struct BlockHeader {
    id: u64,
    abbrev_width: u32,
    words: u64,
}

fn read_block_header(reader: &mut BitReader<'_>) -> Result<BlockHeader, PchInputsError> {
    let id = reader.read_vbr(8)?;
    let abbrev_width = u32::try_from(reader.read_vbr(4)?)
        .ok()
        .filter(|width| (1..=32).contains(width))
        .ok_or_else(|| malformed("abbreviation id width out of range"))?;
    reader.align32();
    let words = reader.read(32)?;
    Ok(BlockHeader {
        id,
        abbrev_width,
        words,
    })
}

/// What a block walk does with a sub-block it meets.
enum SubBlock {
    Skip,
    Enter,
}

/// Walk one block's body. `on_block` decides per sub-block; entered
/// sub-blocks are walked recursively with the same callbacks. Returns when
/// the block's `END_BLOCK` is read, or early when `on_record` says stop.
fn walk_block<'a>(
    reader: &mut BitReader<'a>,
    header: &BlockHeader,
    blockinfo: &mut HashMap<u64, Vec<Abbrev>>,
    on_block: &mut dyn FnMut(u64) -> SubBlock,
    on_record: &mut dyn FnMut(u64, Record<'a>) -> bool,
) -> Result<bool, PchInputsError> {
    let mut abbrevs = blockinfo.get(&header.id).cloned().unwrap_or_default();
    let mut blockinfo_target: Option<u64> = None;
    loop {
        let id = reader.read(header.abbrev_width)?;
        match id {
            END_BLOCK => {
                reader.align32();
                return Ok(true);
            }
            ENTER_SUBBLOCK => {
                let sub = read_block_header(reader)?;
                if sub.id == BLOCKINFO_BLOCK_ID {
                    walk_blockinfo(reader, &sub, blockinfo)?;
                    continue;
                }
                match on_block(sub.id) {
                    SubBlock::Skip => reader.skip_words(sub.words)?,
                    SubBlock::Enter => {
                        if !walk_block(reader, &sub, blockinfo, on_block, on_record)? {
                            return Ok(false);
                        }
                    }
                }
            }
            DEFINE_ABBREV => {
                let abbrev = read_abbrev_definition(reader)?;
                match (header.id, blockinfo_target) {
                    (BLOCKINFO_BLOCK_ID, Some(target)) => {
                        blockinfo.entry(target).or_default().push(abbrev);
                    }
                    (BLOCKINFO_BLOCK_ID, None) => {
                        return Err(malformed("BLOCKINFO abbreviation before SETBID"));
                    }
                    _ => abbrevs.push(abbrev),
                }
            }
            UNABBREV_RECORD => {
                let record = read_unabbreviated(reader)?;
                if header.id == BLOCKINFO_BLOCK_ID {
                    if record.code == BLOCKINFO_SETBID {
                        blockinfo_target = record.ops.first().copied();
                    }
                    continue;
                }
                if !on_record(header.id, record) {
                    return Ok(false);
                }
            }
            n => {
                let index = usize::try_from(n - 4).map_err(|_| malformed("abbreviation id"))?;
                let abbrev = abbrevs
                    .get(index)
                    .ok_or_else(|| malformed("undefined abbreviation id"))?
                    .clone();
                let record = read_abbreviated(reader, &abbrev)?;
                if header.id == BLOCKINFO_BLOCK_ID {
                    continue;
                }
                if !on_record(header.id, record) {
                    return Ok(false);
                }
            }
        }
    }
}

fn walk_blockinfo(
    reader: &mut BitReader<'_>,
    header: &BlockHeader,
    blockinfo: &mut HashMap<u64, Vec<Abbrev>>,
) -> Result<(), PchInputsError> {
    walk_block(
        reader,
        header,
        blockinfo,
        &mut |_| SubBlock::Skip,
        &mut |_, _| true,
    )
    .map(|_| ())
}

/// Parse the input-file records of a clang PCH held in `bytes`.
///
/// Returns `Ok(None)` when `bytes` is not a clang PCH (no `CPCH` magic), and
/// [`PchInputsError::Truncated`] when `bytes` ends before the input-file
/// block does, so a caller holding only a prefix can retry with more.
pub(super) fn parse(bytes: &[u8]) -> Result<Option<Vec<PchInput>>, PchInputsError> {
    if bytes.len() < CLANG_PCH_MAGIC.len() {
        return if CLANG_PCH_MAGIC.starts_with(bytes) {
            Err(PchInputsError::Truncated)
        } else {
            Ok(None)
        };
    }
    if &bytes[..4] != CLANG_PCH_MAGIC {
        return Ok(None);
    }
    let mut reader = BitReader {
        data: bytes,
        bit: 32,
    };
    let mut blockinfo: HashMap<u64, Vec<Abbrev>> = HashMap::new();
    let mut inputs: Vec<PchInput> = Vec::new();
    let mut compiler_major = None;
    let mut decode_error: Option<PchInputsError> = None;
    let mut finished = false;
    while !finished {
        if reader.bit >= bytes.len() * 8 {
            return Err(malformed("no INPUT_FILES_BLOCK in CONTROL_BLOCK"));
        }
        if reader.read(2)? != ENTER_SUBBLOCK {
            return Err(malformed("top-level item is not a block"));
        }
        let header = read_block_header(&mut reader)?;
        match header.id {
            BLOCKINFO_BLOCK_ID => walk_blockinfo(&mut reader, &header, &mut blockinfo)?,
            CONTROL_BLOCK_ID => {
                let mut saw_input_files = false;
                let mut on_block = |id: u64| {
                    if id == INPUT_FILES_BLOCK_ID {
                        saw_input_files = true;
                        SubBlock::Enter
                    } else {
                        SubBlock::Skip
                    }
                };
                let mut on_record = |block: u64, record: Record<'_>| {
                    let decoded = match block {
                        INPUT_FILES_BLOCK_ID => decode_input_record(&mut inputs, record),
                        CONTROL_BLOCK_ID if record.code == METADATA => {
                            // METADATA: AST major/minor, clang major/minor,
                            // relocatable/error flags. Only clang >= 19 uses
                            // reproducible xxh3 INPUT_FILE_HASH records.
                            compiler_major = record.ops.get(2).copied();
                            Ok(())
                        }
                        CONTROL_BLOCK_ID if record.code == IMPORT => {
                            decode_import_record(&mut inputs, &record)
                        }
                        _ => Ok(()),
                    };
                    if let Err(error) = decoded {
                        decode_error = Some(error);
                        return false;
                    }
                    true
                };
                walk_block(
                    &mut reader,
                    &header,
                    &mut blockinfo,
                    &mut on_block,
                    &mut on_record,
                )?;
                if let Some(error) = decode_error.take() {
                    return Err(error);
                }
                if !saw_input_files {
                    return Err(malformed("CONTROL_BLOCK has no INPUT_FILES_BLOCK"));
                }
                finished = true;
            }
            _ => reader.skip_words(header.words)?,
        }
    }
    if let Some(error) = decode_error {
        return Err(error);
    }
    if compiler_major.is_none_or(|major| major < 19) {
        for input in &mut inputs {
            input.content_xxh3 = None;
        }
    }
    Ok(Some(inputs))
}

/// An imported PCH (`-include-pch` while building a chained PCH). clang
/// checks the imported file's size and mtime against this record, so it is
/// validated like an input.
///
/// clang >= 20 writes one abbreviated `IMPORT` per file: kind, standard C++
/// module, import location, module-name length, size, mtime, path length,
/// with the path at the end of the blob. clang 17-19 write one unabbreviated
/// `IMPORTS` record listing every import as kind, standard C++ module, import
/// location, size, mtime, 20 signature words, then the module name and the
/// path as length-prefixed strings.
fn decode_import_record(
    inputs: &mut Vec<PchInput>,
    record: &Record<'_>,
) -> Result<(), PchInputsError> {
    let mut push = |kind: u64, size: u64, mtime: u64, path: String| {
        if kind != MODULE_KIND_PCH {
            return Err(malformed("imported AST file is not a PCH"));
        }
        inputs.push(PchInput {
            as_requested: path.clone(),
            path,
            size,
            mtime_secs: mtime,
            content_xxh3: None,
            overridden: false,
            transient: false,
            top_level: false,
            imported_pch: true,
        });
        Ok(())
    };
    if let Some(blob) = record.blob {
        let [kind, _, _, _, size, mtime, path_len] = record.ops[..] else {
            return Err(malformed("unsupported IMPORT record layout"));
        };
        let path_len = usize::try_from(path_len)
            .ok()
            .filter(|len| *len <= blob.len())
            .ok_or_else(|| malformed("IMPORT path length exceeds its blob"))?;
        let path = String::from_utf8_lossy(&blob[blob.len() - path_len..]).into_owned();
        return push(kind, size, mtime, path);
    }
    // Layout variants seen across clang releases (and vendor forks): with or
    // without the standard-C++-module flag, and a 20-byte or 5-word
    // signature. Take the first one that consumes the record exactly; if none
    // does, the record is not understood and the PCH is refused.
    for (has_cxx_flag, signature_words) in [(true, 20), (false, 20), (true, 5), (false, 5)] {
        if let Ok(imports) = decode_legacy_imports(&record.ops, has_cxx_flag, signature_words) {
            for (kind, size, mtime, path) in imports {
                push(kind, size, mtime, path)?;
            }
            return Ok(());
        }
    }
    Err(malformed("unsupported IMPORTS record layout"))
}

fn decode_legacy_imports(
    ops: &[u64],
    has_cxx_flag: bool,
    signature_words: usize,
) -> Result<Vec<(u64, u64, u64, String)>, PchInputsError> {
    let mut cursor = OpCursor { ops, at: 0 };
    let mut imports = Vec::new();
    while !cursor.done() {
        let kind = cursor.one()?;
        if has_cxx_flag && cursor.one()? != 0 {
            return Err(malformed("imported AST file is a C++20 module"));
        }
        let _import_loc = cursor.one()?;
        let size = cursor.one()?;
        let mtime = cursor.one()?;
        cursor.take(signature_words)?;
        let _module_name = cursor.string()?;
        let path = cursor.string()?;
        if !Path::new(&path).is_absolute() {
            return Err(malformed("IMPORTS path is not absolute"));
        }
        imports.push((kind, size, mtime, path));
    }
    Ok(imports)
}

/// Sequential reader over an unabbreviated record's operands.
struct OpCursor<'a> {
    ops: &'a [u64],
    at: usize,
}

impl<'a> OpCursor<'a> {
    fn done(&self) -> bool {
        self.at >= self.ops.len()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u64], PchInputsError> {
        let end = self
            .at
            .checked_add(count)
            .ok_or_else(|| malformed("IMPORTS record is truncated"))?;
        let slice = self
            .ops
            .get(self.at..end)
            .ok_or_else(|| malformed("IMPORTS record is truncated"))?;
        self.at = end;
        Ok(slice)
    }

    fn one(&mut self) -> Result<u64, PchInputsError> {
        Ok(self.take(1)?[0])
    }

    /// clang's `AddString`: a length, then one operand per byte.
    fn string(&mut self) -> Result<String, PchInputsError> {
        let len = usize::try_from(self.one()?).map_err(|_| malformed("IMPORTS string length"))?;
        let bytes = self
            .take(len)?
            .iter()
            .map(|op| u8::try_from(*op).map_err(|_| malformed("IMPORTS string byte")))
            .collect::<Result<Vec<u8>, _>>()?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

fn decode_input_record(
    inputs: &mut Vec<PchInput>,
    record: Record<'_>,
) -> Result<(), PchInputsError> {
    match record.code {
        INPUT_FILE => {
            let op = |index: usize| record.ops.get(index).copied();
            let (Some(size), Some(mtime)) = (op(1), op(2)) else {
                return Err(malformed("INPUT_FILE record is too short"));
            };
            let blob = record
                .blob
                .ok_or_else(|| malformed("INPUT_FILE record has no name blob"))?;
            // clang >= 17 stores the name as requested, then the resolved
            // name when it differs; `op7` is the as-requested length. Older
            // layouts carry just the name.
            let split = op(7)
                .and_then(|len| usize::try_from(len).ok())
                .filter(|len| *len <= blob.len())
                .unwrap_or(blob.len());
            let (as_requested, resolved) = blob.split_at(split);
            let name = if resolved.is_empty() {
                as_requested
            } else {
                resolved
            };
            inputs.push(PchInput {
                path: String::from_utf8_lossy(name).into_owned(),
                as_requested: String::from_utf8_lossy(as_requested).into_owned(),
                size,
                mtime_secs: mtime,
                content_xxh3: None,
                overridden: op(3).unwrap_or(0) != 0,
                transient: op(4).unwrap_or(0) != 0,
                top_level: op(5).unwrap_or(0) != 0,
                imported_pch: false,
            });
        }
        INPUT_FILE_HASH => {
            let (Some(low), Some(high)) = (record.ops.first(), record.ops.get(1)) else {
                return Err(malformed("INPUT_FILE_HASH record is too short"));
            };
            let hash = (high << 32) | (low & 0xffff_ffff);
            let last = inputs
                .last_mut()
                .ok_or_else(|| malformed("INPUT_FILE_HASH before any INPUT_FILE"))?;
            // A zero hash means clang did not hash this input.
            last.content_xxh3 = (hash != 0).then_some(hash);
        }
        _ => {}
    }
    Ok(())
}
