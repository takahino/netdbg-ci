//! アセンブリの入口メソッドのうち、ソース行に `STOP` とある最初のシーケンスポイント。
//!
//! アセンブリも Portable PDB も ECMA-335 のメタデータ（BSJB）。表の幅は、行数とヒープの大きさで 2 か 4 になる。

use std::fs;
use std::ops::Range;
use std::path::Path;

const HIDDEN_LINE: u32 = 0x00FE_EFEE;
const TABLE_COUNT: usize = 64;
const NONE: usize = 63;

#[derive(Clone, Copy)]
enum Column {
    U16,
    U32,
    Str,
    Guid,
    Blob,
    Table(usize),
    Coded(&'static [usize]),
}
use Column::*;

const TYPE_DEF_OR_REF: Column = Coded(&[0x02, 0x01, 0x1B]);
const HAS_CONSTANT: Column = Coded(&[0x04, 0x08, 0x17]);
const HAS_CUSTOM_ATTRIBUTE: Column =
    Coded(&[0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x00, 0x0E, 0x17, 0x14, 0x11, 0x1A, 0x1B, 0x20, 0x23, 0x26, 0x27, 0x28, 0x2A, 0x2C, 0x2B]);
const HAS_FIELD_MARSHAL: Column = Coded(&[0x04, 0x08]);
const HAS_DECL_SECURITY: Column = Coded(&[0x02, 0x06, 0x20]);
const MEMBER_REF_PARENT: Column = Coded(&[0x02, 0x01, 0x1A, 0x06, 0x1B]);
const HAS_SEMANTICS: Column = Coded(&[0x14, 0x17]);
const METHOD_DEF_OR_REF: Column = Coded(&[0x06, 0x0A]);
const MEMBER_FORWARDED: Column = Coded(&[0x04, 0x06]);
const IMPLEMENTATION: Column = Coded(&[0x26, 0x23, 0x27]);
const CUSTOM_ATTRIBUTE_TYPE: Column = Coded(&[NONE, NONE, 0x06, 0x0A, NONE]);
const RESOLUTION_SCOPE: Column = Coded(&[0x00, 0x1A, 0x23, 0x01]);
const TYPE_OR_METHOD_DEF: Column = Coded(&[0x02, 0x06]);

fn schema(table: usize) -> &'static [Column] {
    match table {
        0x00 => &[U16, Str, Guid, Guid, Guid],
        0x01 => &[RESOLUTION_SCOPE, Str, Str],
        0x02 => &[U32, Str, Str, TYPE_DEF_OR_REF, Table(0x04), Table(0x06)],
        0x03 => &[Table(0x04)],
        0x04 => &[U16, Str, Blob],
        0x05 => &[Table(0x06)],
        0x06 => &[U32, U16, U16, Str, Blob, Table(0x08)],
        0x07 => &[Table(0x08)],
        0x08 => &[U16, U16, Str],
        0x09 => &[Table(0x02), TYPE_DEF_OR_REF],
        0x0A => &[MEMBER_REF_PARENT, Str, Blob],
        0x0B => &[U16, HAS_CONSTANT, Blob],
        0x0C => &[HAS_CUSTOM_ATTRIBUTE, CUSTOM_ATTRIBUTE_TYPE, Blob],
        0x0D => &[HAS_FIELD_MARSHAL, Blob],
        0x0E => &[U16, HAS_DECL_SECURITY, Blob],
        0x0F => &[U16, U32, Table(0x02)],
        0x10 => &[U32, Table(0x04)],
        0x11 => &[Blob],
        0x12 => &[Table(0x02), Table(0x14)],
        0x13 => &[Table(0x14)],
        0x14 => &[U16, Str, TYPE_DEF_OR_REF],
        0x15 => &[Table(0x02), Table(0x17)],
        0x16 => &[Table(0x17)],
        0x17 => &[U16, Str, Blob],
        0x18 => &[U16, Table(0x06), HAS_SEMANTICS],
        0x19 => &[Table(0x02), METHOD_DEF_OR_REF, METHOD_DEF_OR_REF],
        0x1A => &[Str],
        0x1B => &[Blob],
        0x1C => &[U16, MEMBER_FORWARDED, Str, Table(0x1A)],
        0x1D => &[U32, Table(0x04)],
        0x1E => &[U32, U32],
        0x1F => &[U32],
        0x20 => &[U32, U16, U16, U16, U16, U32, Blob, Str, Str],
        0x21 => &[U32],
        0x22 => &[U32, U32, U32],
        0x23 => &[U16, U16, U16, U16, U32, Blob, Str, Str, Blob],
        0x24 => &[U32, Table(0x23)],
        0x25 => &[U32, U32, U32, Table(0x23)],
        0x26 => &[U32, Str, Blob],
        0x27 => &[U32, U32, Str, Str, IMPLEMENTATION],
        0x28 => &[U32, U32, Str, IMPLEMENTATION],
        0x29 => &[Table(0x02), Table(0x02)],
        0x2A => &[U16, U16, TYPE_OR_METHOD_DEF, Str],
        0x2B => &[METHOD_DEF_OR_REF, Blob],
        0x2C => &[Table(0x2A), TYPE_DEF_OR_REF],
        0x30 => &[Blob, Guid, Blob, Guid],
        0x31 => &[Table(0x30), Blob],
        _ => &[],
    }
}

fn column_width(column: Column, rows: &[u32; TABLE_COUNT], heap_sizes: u8) -> usize {
    let wide = |is_wide: bool| if is_wide { 4 } else { 2 };
    match column {
        U16 => 2,
        U32 => 4,
        Str => wide(heap_sizes & 1 != 0),
        Guid => wide(heap_sizes & 2 != 0),
        Blob => wide(heap_sizes & 4 != 0),
        Table(table) => wide(rows[table] > 0xFFFF),
        Coded(tables) => {
            let tag_bits = usize::BITS - (tables.len() - 1).leading_zeros();
            let largest = tables.iter().map(|&t| rows[t]).max().unwrap_or(0);
            wide(largest >= 1 << (16 - tag_bits))
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.pos).ok_or("metadata ended early")?;
        self.pos += 1;
        Ok(b)
    }
    fn u16(&mut self) -> Result<u32, String> {
        Ok(self.u8()? as u32 | (self.u8()? as u32) << 8)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(self.u16()? | self.u16()? << 16)
    }
    fn skip(&mut self, count: usize) -> Result<(), String> {
        self.pos = self.pos.checked_add(count).filter(|&p| p <= self.data.len()).ok_or("metadata ended early")?;
        Ok(())
    }
    fn compressed(&mut self) -> Result<(u32, u32), String> {
        let b = self.u8()? as u32;
        if b & 0x80 == 0 {
            Ok((b, 1))
        } else if b & 0xC0 == 0x80 {
            Ok(((b & 0x3F) << 8 | self.u8()? as u32, 2))
        } else {
            let rest = (self.u8()? as u32) << 16 | (self.u8()? as u32) << 8 | self.u8()? as u32;
            Ok(((b & 0x1F) << 24 | rest, 4))
        }
    }
    fn unsigned(&mut self) -> Result<u32, String> {
        Ok(self.compressed()?.0)
    }
    fn signed(&mut self) -> Result<i32, String> {
        let (raw, width) = self.compressed()?;
        let value = (raw >> 1) as i32;
        if raw & 1 == 0 {
            return Ok(value);
        }
        Ok(value
            - match width {
                1 => 0x40,
                2 => 0x2000,
                _ => 0x1000_0000,
            })
    }
    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }
}

struct Meta<'a> {
    data: &'a [u8],
    blobs: Range<usize>,
    /// 表 0x31。先頭、行数、1 行の幅、Document 列の幅、SequencePoints 列の幅。
    method_debug: (usize, u32, usize, usize, usize),
    /// 表 0x30。先頭、1 行の幅、名前の列の幅。
    document: (usize, usize, usize),
}

fn parse_meta(data: &[u8]) -> Result<Meta<'_>, String> {
    let mut r = Reader { data, pos: 0 };
    if r.u32()? != 0x424A_5342 {
        return Err("not metadata (no BSJB)".into());
    }
    r.skip(8)?;
    let version_len = r.u32()? as usize;
    r.skip(version_len)?;
    r.skip(2)?;
    let stream_count = r.u16()?;
    let (mut tables, mut blobs, mut pdb) = (0..0, 0..0, 0..0);
    for _ in 0..stream_count {
        let (offset, size) = (r.u32()? as usize, r.u32()? as usize);
        let start = r.pos;
        while r.u8()? != 0 {}
        let name = &data[start..r.pos - 1];
        r.pos = (r.pos + 3) & !3;
        let range = offset..offset.saturating_add(size);
        if range.end > data.len() {
            return Err("a metadata stream is outside the file".into());
        }
        match name {
            b"#~" | b"#-" => tables = range,
            b"#Blob" => blobs = range,
            b"#Pdb" => pdb = range,
            _ => {}
        }
    }
    if tables.is_empty() || blobs.is_empty() {
        return Err("metadata has no #~ or #Blob".into());
    }

    let mut t = Reader { data: &data[..tables.end], pos: tables.start };
    t.skip(6)?;
    let heap_sizes = t.u8()?;
    t.skip(1)?;
    let valid = t.u32()? as u64 | (t.u32()? as u64) << 32;
    t.skip(8)?;
    let mut rows = [0u32; TABLE_COUNT];
    for (bit, row) in rows.iter_mut().enumerate() {
        if valid & (1 << bit) != 0 {
            *row = t.u32()?;
        }
    }
    if heap_sizes & 0x40 != 0 {
        t.skip(4)?;
    }
    // 単体の PDB は型の表を持たない。添字の幅は #Pdb に写してある行数で決まる。
    let mut sizing = rows;
    if !pdb.is_empty() {
        let mut p = Reader { data: &data[..pdb.end], pos: pdb.start };
        p.skip(24)?;
        let referenced = p.u32()? as u64 | (p.u32()? as u64) << 32;
        for (bit, row) in sizing.iter_mut().enumerate() {
            if referenced & (1 << bit) != 0 {
                *row = p.u32()?;
            }
        }
    }

    let mut start = t.pos;
    let mut method_debug = None;
    let mut document = None;
    for table in 0..0x32 {
        let columns = schema(table);
        if rows[table] > 0 && columns.is_empty() {
            return Err(format!("unknown metadata table 0x{table:02X}"));
        }
        let mut row_size = 0usize;
        let mut columns_at = Vec::new();
        for &column in columns {
            let width = column_width(column, &sizing, heap_sizes);
            columns_at.push((row_size, width));
            row_size += width;
        }
        if table == 0x30 && rows[table] > 0 {
            document = Some((start, row_size, columns_at[0].1));
        }
        if table == 0x31 && rows[table] > 0 {
            method_debug = Some((start, rows[table], row_size, columns_at[0].1, columns_at[1].1));
        }
        start = (rows[table] as usize).saturating_mul(row_size).saturating_add(start);
        if start > tables.end {
            return Err("metadata tables ended early".into());
        }
    }
    let method_debug = method_debug.ok_or("PDB has no MethodDebugInformation")?;
    let document = document.ok_or("PDB has no Document table")?;
    Ok(Meta { data, blobs, method_debug, document })
}

fn cell(data: &[u8], table_start: usize, row_size: usize, row: u32, column: usize, width: usize) -> Result<u32, String> {
    let at = table_start + (row as usize - 1) * row_size + column;
    let bytes = data.get(at..at + width).ok_or("metadata cell is outside the file")?;
    Ok(match width {
        2 => u16::from_le_bytes([bytes[0], bytes[1]]) as u32,
        4 => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        _ => return Err(format!("unexpected cell width {width}")),
    })
}

fn blob<'a>(data: &'a [u8], blobs: Range<usize>, index: u32) -> Result<&'a [u8], String> {
    let heap = data.get(blobs.clone()).ok_or("blob heap is missing")?;
    let mut r = Reader { data: heap, pos: index as usize };
    let len = r.unsigned()? as usize;
    heap.get(r.pos..r.pos + len).ok_or("blob ended early".to_string())
}

struct Point {
    il_offset: u32,
    document: u32,
    start_line: u32,
    #[allow(dead_code)]
    start_column: u32,
}

fn sequence_points(data: &[u8], mut document: u32) -> Result<Vec<Point>, String> {
    let mut s = Reader { data, pos: 0 };
    s.unsigned()?;
    if document == 0 {
        document = s.unsigned()?;
    }
    let mut points = Vec::new();
    let (mut il, mut line, mut column) = (0u32, 0i64, 0i64);
    let (mut first, mut first_visible) = (true, true);
    while !s.at_end() {
        let delta_il = s.unsigned()?;
        if !first && delta_il == 0 {
            document = s.unsigned()?;
            continue;
        }
        il += delta_il;
        first = false;
        let delta_lines = s.unsigned()?;
        let delta_columns = if delta_lines == 0 { s.unsigned()? as i64 } else { s.signed()? as i64 };
        if delta_lines == 0 && delta_columns == 0 {
            points.push(Point { il_offset: il, document, start_line: HIDDEN_LINE, start_column: 0 });
            continue;
        }
        if first_visible {
            line = s.unsigned()? as i64;
            column = s.unsigned()? as i64;
            first_visible = false;
        } else {
            line += s.signed()? as i64;
            column += s.signed()? as i64;
        }
        points.push(Point { il_offset: il, document, start_line: line as u32, start_column: column as u32 });
        let _end_column = column + delta_columns;
    }
    Ok(points)
}

fn document_name(meta: &Meta, row: u32) -> Result<String, String> {
    let (start, row_size, width) = meta.document;
    let index = cell(meta.data, start, row_size, row, 0, width)?;
    let mut r = Reader { data: blob(meta.data, meta.blobs.clone(), index)?, pos: 0 };
    let separator = r.u8()?;
    let mut name = String::new();
    let mut first = true;
    while !r.at_end() {
        if !first && separator != 0 {
            name.push(separator as char);
        }
        first = false;
        name.push_str(&String::from_utf8_lossy(blob(meta.data, meta.blobs.clone(), r.unsigned()?)?));
    }
    Ok(name)
}

/// CLR ヘッダーの入口トークン。マネージドの入口が無ければ `None`。
pub fn entry_point(data: &[u8]) -> Option<u32> {
    if data.get(0..2) != Some(b"MZ") {
        return None;
    }
    let pe = u32::from_le_bytes(data.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if data.get(pe..pe + 4) != Some(b"PE\0\0") {
        return None;
    }
    let coff = pe + 4;
    let sections = u16::from_le_bytes(data.get(coff + 2..coff + 4)?.try_into().ok()?) as usize;
    let optional_size = u16::from_le_bytes(data.get(coff + 16..coff + 18)?.try_into().ok()?) as usize;
    let optional = pe + 24;
    let magic = u16::from_le_bytes(data.get(optional..optional + 2)?.try_into().ok()?);
    let directories = optional + if magic == 0x20B { 112 } else if magic == 0x10B { 96 } else { return None };
    let clr_rva = u32::from_le_bytes(data.get(directories + 14 * 8..directories + 14 * 8 + 4)?.try_into().ok()?);
    if clr_rva == 0 {
        return None;
    }
    let section_table = optional + optional_size;
    let clr = rva_to_offset(data, section_table, sections, clr_rva)?;
    let flags = u32::from_le_bytes(data.get(clr + 16..clr + 20)?.try_into().ok()?);
    let token = u32::from_le_bytes(data.get(clr + 20..clr + 24)?.try_into().ok()?);
    (flags & 0x10 == 0 && token >> 24 == 0x06).then_some(token)
}

fn rva_to_offset(data: &[u8], section_table: usize, sections: usize, rva: u32) -> Option<usize> {
    for i in 0..sections {
        let s = section_table + i * 40;
        let virtual_size = u32::from_le_bytes(data.get(s + 8..s + 12)?.try_into().ok()?);
        let virtual_address = u32::from_le_bytes(data.get(s + 12..s + 16)?.try_into().ok()?);
        let raw_size = u32::from_le_bytes(data.get(s + 16..s + 20)?.try_into().ok()?);
        let raw_ptr = u32::from_le_bytes(data.get(s + 20..s + 24)?.try_into().ok()?);
        let size = raw_size.max(virtual_size);
        if rva >= virtual_address && rva < virtual_address.saturating_add(size) {
            return Some((raw_ptr + (rva - virtual_address)) as usize);
        }
    }
    None
}

pub struct Stop {
    pub token: u32,
    pub il_offset: u32,
    pub line: u32,
    pub file: String,
}

/// 入口メソッドの、行のテキストに `STOP` を含む最初のシーケンスポイント。
pub fn stop_line(dll: &Path) -> Result<Stop, String> {
    let image = fs::read(dll).map_err(|e| format!("cannot read {}: {e}", dll.display()))?;
    let token = entry_point(&image).ok_or("the assembly has no managed entry point")?;
    let pdb_path = dll.with_extension("pdb");
    let pdb = fs::read(&pdb_path).map_err(|e| format!("cannot read {}: {e}", pdb_path.display()))?;
    let meta = parse_meta(&pdb)?;
    let row = token & 0x00FF_FFFF;
    let (start, method_rows, row_size, doc_width, blob_width) = meta.method_debug;
    if row == 0 || row > method_rows {
        return Err(format!("entry method row {row} is outside MethodDebugInformation ({method_rows} rows)"));
    }
    let document = cell(meta.data, start, row_size, row, 0, doc_width)?;
    let points_index = cell(meta.data, start, row_size, row, doc_width, blob_width)?;
    if points_index == 0 {
        return Err("the entry method has no sequence points".into());
    }
    let points = sequence_points(blob(meta.data, meta.blobs.clone(), points_index)?, document)?;
    for point in points {
        if point.start_line == HIDDEN_LINE || point.document == 0 {
            continue;
        }
        let file = document_name(&meta, point.document)?;
        let text = fs::read_to_string(&file).map_err(|e| format!("cannot read {file}: {e}"))?;
        let source = text.lines().nth(point.start_line as usize - 1).unwrap_or("");
        if source.contains("STOP") {
            return Ok(Stop { token, il_offset: point.il_offset, line: point.start_line, file });
        }
    }
    Err("no sequence point lands on the line that contains STOP".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_visible_point_is_line_10() {
        // ローカルシグネチャ 0、IL 0 は 10 行目の 5 桁目から 9 桁目。
        let blob = [0, 0, 0, 4, 10, 5];
        let points = sequence_points(&blob, 1).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].il_offset, 0);
        assert_eq!(points[0].start_line, 10);
        assert_eq!(points[0].start_column, 5);
    }
}
