//! A `.gdbtable`'s layout (its header and field descriptors), its `.gdbtablx` row index and its
//! rows' values.
//!
//! Layout as documented in the reverse-engineered FileGDB specification
//! (<https://github.com/rouault/dump_gdbtable/wiki/FGDB-Spec>), for version 3 tables (ArcGIS 10
//! and later) and version 4 ones (64-bit object IDs, ArcGIS Pro 3.2 and later).

use super::{ReadAt, bad};
use crate::{FieldType, Result, Value};

/// A field's type, by its code in the field descriptors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Int16,
    Int32,
    Float32,
    Float64,
    Text,
    DateTime,
    ObjectId,
    Geometry,
    Binary,
    Guid,
    GlobalId,
    Xml,
    Int64,
    DateOnly,
    TimeOnly,
    DateTimeOffset,
}

impl Kind {
    /// The type a reader gives the field's values; `None` for fields left out (the object ID,
    /// which is the feature's id, the geometry and binary data).
    pub(super) fn out(self) -> Option<FieldType> {
        Some(match self {
            Kind::Int16 | Kind::Int32 | Kind::Int64 => FieldType::Integer,
            Kind::Float32 | Kind::Float64 => FieldType::Double,
            Kind::Text
            | Kind::Xml
            | Kind::Guid
            | Kind::GlobalId
            | Kind::DateTime
            | Kind::DateOnly
            | Kind::TimeOnly
            | Kind::DateTimeOffset => FieldType::Text,
            Kind::ObjectId | Kind::Geometry | Kind::Binary => return None,
        })
    }
}

/// A field descriptor.
#[derive(Clone, Debug)]
pub(super) struct Column {
    pub name: String,
    pub kind: Kind,
    pub nullable: bool,
}

/// The geometry field's coordinate system and the precision its coordinates are stored at.
#[derive(Clone, Debug)]
pub(super) struct GeomField {
    /// The coordinate system as Esri WKT (empty or a placeholder when unknown).
    pub wkt: String,
    pub x_origin: f64,
    pub y_origin: f64,
    pub xy_scale: f64,
    /// The extent the table says its features have, in its own coordinates (NaN when empty).
    pub extent: [f64; 4],
}

/// A table's layout.
#[derive(Clone, Debug)]
pub(super) struct Layout {
    /// The geometry type the table declares: 0 none, 1 point, 2 multipoint, 3 polyline,
    /// 4 polygon, 9 multipatch.
    pub geometry_type: u8,
    pub columns: Vec<Column>,
    pub geom: Option<GeomField>,
}

impl Layout {
    /// How many bytes of null flags start each row: a bit per nullable field.
    pub(super) fn null_bytes(&self) -> usize {
        self.columns.iter().filter(|c| c.nullable).count().div_ceil(8)
    }
}

/// Most fields a table may have (the format counts them in 16 bits).
const MAX_FIELDS: usize = 65_535;

/// Most bytes a table's field descriptors may take.
const MAX_HEADER: u64 = 64 << 20;

/// Reads a `.gdbtable`'s header and field descriptors.
pub(super) fn read_layout(f: &mut dyn ReadAt) -> Result<Layout> {
    let len = f.len();
    let mut head = [0u8; 40];
    f.read_at(0, &mut head).map_err(|_| bad("the table header is cut short"))?;
    let version = le_u32(&head, 0).unwrap_or(0);
    if !(3..=4).contains(&version) {
        return Err(bad(format!("table version {version} isn't supported")));
    }
    let fields_at = le_u64(&head, 32).unwrap_or(0);
    let mut size = [0u8; 4];
    f.read_at(fields_at, &mut size).map_err(|_| bad("the field descriptors are cut short"))?;
    let size = u64::from(u32::from_le_bytes(size));
    if size > MAX_HEADER || fields_at.saturating_add(4).saturating_add(size) > len {
        return Err(bad("the field descriptors run past the end of the file"));
    }
    let mut b = vec![0u8; size as usize];
    f.read_at(fields_at + 4, &mut b).map_err(|_| bad("the field descriptors are cut short"))?;
    parse_fields(&b)
}

/// Parses the field descriptors (after their 4-byte size).
fn parse_fields(b: &[u8]) -> Result<Layout> {
    let mut r = Bytes::new(b);
    let short = || bad("the field descriptors are cut short");
    let _version = r.u32().ok_or_else(short)?;
    let flags = r.u32().ok_or_else(short)?;
    let geometry_type = (flags & 0xff) as u8;
    let has_z = flags & (1 << 31) != 0;
    let has_m = flags & (1 << 30) != 0;
    let n = usize::from(r.u16().ok_or_else(short)?);
    if n > MAX_FIELDS {
        return Err(bad("too many fields"));
    }
    let mut columns = Vec::with_capacity(n.min(1024));
    let mut geom = None;
    for _ in 0..n {
        let name = r.utf16_u8len().ok_or_else(short)?;
        let _alias = r.utf16_u8len().ok_or_else(short)?;
        let code = r.u8().ok_or_else(short)?;
        let (kind, nullable) = match code {
            6 => {
                // Width (4, or 8 for 64-bit IDs), then a flag.
                r.skip(2).ok_or_else(short)?;
                (Kind::ObjectId, false)
            }
            4 => {
                let _width = r.u32().ok_or_else(short)?;
                let flag = r.u8().ok_or_else(short)?;
                let default = r.varuint().ok_or_else(short)?;
                r.skip(usize::try_from(default).map_err(|_| short())?).ok_or_else(short)?;
                (Kind::Text, flag & 1 != 0)
            }
            0 | 1 | 2 | 3 | 5 | 13 | 14 | 15 | 16 => {
                let _width = r.u8().ok_or_else(short)?;
                let flag = r.u8().ok_or_else(short)?;
                let default = r.u8().ok_or_else(short)?;
                r.skip(usize::from(default)).ok_or_else(short)?;
                let kind = match code {
                    0 => Kind::Int16,
                    1 => Kind::Int32,
                    2 => Kind::Float32,
                    3 => Kind::Float64,
                    5 => Kind::DateTime,
                    13 => Kind::Int64,
                    14 => Kind::DateOnly,
                    15 => Kind::TimeOnly,
                    _ => Kind::DateTimeOffset,
                };
                (kind, flag & 1 != 0)
            }
            7 => {
                let g = read_geom_field(&mut r, has_z, has_m).ok_or_else(|| bad("the geometry field's descriptor is damaged"))?;
                geom = Some(g.0);
                (Kind::Geometry, g.1)
            }
            8 | 12 => {
                let _ = r.u8().ok_or_else(short)?;
                let flag = r.u8().ok_or_else(short)?;
                (if code == 8 { Kind::Binary } else { Kind::Xml }, flag & 1 != 0)
            }
            10 | 11 => {
                let _width = r.u8().ok_or_else(short)?;
                let flag = r.u8().ok_or_else(short)?;
                (if code == 10 { Kind::Guid } else { Kind::GlobalId }, flag & 1 != 0)
            }
            9 => return Err(bad("tables with a raster field aren't read")),
            other => return Err(bad(format!("unknown field type {other}"))),
        };
        columns.push(Column { name, kind, nullable });
    }
    Ok(Layout { geometry_type, columns, geom })
}

/// A geometry field's descriptor (after its type code), and whether it's nullable.
fn read_geom_field(r: &mut Bytes<'_>, table_z: bool, table_m: bool) -> Option<(GeomField, bool)> {
    let _ = r.u8()?;
    let flag = r.u8()?;
    let wkt_len = usize::from(r.u16()?);
    let wkt = r.take(wkt_len)?;
    let wkt = utf16le(wkt);
    // Which origins and scales follow: bit 1 for M, bit 2 for Z.
    let present = r.u8()?;
    let (x_origin, y_origin, xy_scale) = (r.f64()?, r.f64()?, r.f64()?);
    if present & 2 != 0 {
        r.skip(16)?;
    }
    if present & 4 != 0 {
        r.skip(16)?;
    }
    // XY tolerance, then the M and Z ones.
    r.skip(8)?;
    if present & 2 != 0 {
        r.skip(8)?;
    }
    if present & 4 != 0 {
        r.skip(8)?;
    }
    let extent = [r.f64()?, r.f64()?, r.f64()?, r.f64()?];
    // The Z and M ranges for a table with them, then a zero byte and the spatial index's grid
    // sizes (1 to 3). Some writers differ on the ranges, so when the byte and count aren't where
    // expected, look a few doubles further, as GDAL does.
    let expected = 16 * (usize::from(table_z) + usize::from(table_m));
    let grid_at =
        |r: &Bytes<'_>, skip: usize| -> bool { r.peek(skip) == Some(0) && r.peek_u32(skip + 1).is_some_and(|n| (1..=3).contains(&n)) };
    let skip = std::iter::once(expected).chain((0..=8).map(|i| i * 8)).find(|s| grid_at(r, *s))?;
    r.skip(skip + 1)?;
    let grids = r.u32()? as usize;
    r.skip(grids * 8)?;
    if !(xy_scale.is_finite() && xy_scale > 0.0 && x_origin.is_finite() && y_origin.is_finite()) {
        return None;
    }
    Some((GeomField { wkt, x_origin, y_origin, xy_scale, extent }, flag & 1 != 0))
}

/// Where each row is in the `.gdbtable`, by object ID, from the `.gdbtablx`. Deleted rows are
/// left out.
pub(super) fn read_index(f: &mut dyn ReadAt) -> Result<Vec<(u64, u64)>> {
    let len = f.len();
    let mut head = [0u8; 16];
    f.read_at(0, &mut head).map_err(|_| bad("the row index header is cut short"))?;
    let version = le_u32(&head, 0).unwrap_or(0);
    let blocks = u64::from(le_u32(&head, 4).unwrap_or(0));
    let header_rows = u64::from(le_u32(&head, 8).unwrap_or(0));
    let width = u64::from(le_u32(&head, 12).unwrap_or(0));
    if !(4..=6).contains(&width) || !(3..=4).contains(&version) {
        return Err(bad("the row index is damaged"));
    }
    let offsets_len = blocks.saturating_mul(1024).saturating_mul(width);
    if offsets_len.saturating_add(16) > len {
        return Err(bad("the row index is cut short"));
    }
    // After the offsets: (version 4) the number of rows, then the bitmap of which blocks of
    // 1024 rows are stored, when some aren't.
    let mut trailer = vec![0u8; (len - 16 - offsets_len).min(1 << 20) as usize];
    f.read_at(16 + offsets_len, &mut trailer).map_err(|_| bad("the row index is cut short"))?;
    let (rows, bitmap_at) = if version == 4 { (le_u64(&trailer, 0).unwrap_or(0), 8) } else { (header_rows, 0) };
    let words = u64::from(le_u32(&trailer, bitmap_at).unwrap_or(0));
    // Which block of rows each stored block is.
    let present: Vec<u64> = if words == 0 {
        (0..blocks).collect()
    } else {
        let bits = trailer
            .get(bitmap_at + 16..)
            .and_then(|b| b.get(..usize::try_from(words * 4).ok()?))
            .ok_or_else(|| bad("the row index's bitmap is cut short"))?;
        let v: Vec<u64> = (0..words * 32).filter(|i| bits.get((i / 8) as usize).is_some_and(|b| b >> (i % 8) & 1 == 1)).collect();
        if v.len() as u64 != blocks {
            return Err(bad("the row index's bitmap doesn't match its blocks"));
        }
        v
    };
    let mut out = Vec::new();
    let mut buf = vec![0u8; (1024 * width) as usize];
    for (stored, block) in present.iter().enumerate() {
        f.read_at(16 + stored as u64 * 1024 * width, &mut buf).map_err(|_| bad("the row index is cut short"))?;
        for (i, c) in buf.chunks_exact(width as usize).enumerate() {
            let oid = block * 1024 + i as u64 + 1;
            if oid > rows {
                break;
            }
            let offset = c.iter().rev().fold(0u64, |a, b| a << 8 | u64::from(*b));
            if offset != 0 {
                out.push((oid, offset));
            }
        }
    }
    Ok(out)
}

/// A row's values, by field: `None` for a null.
pub(super) enum Cell<'a> {
    Null,
    Value(Value),
    /// The geometry field's blob.
    Geometry(&'a [u8]),
    /// A field left out (the object ID, binary data).
    Skipped,
}

/// Parses a row (after its 4-byte size) into its cells, one per column. `stop_after_geometry`
/// stops at the geometry (for reading just its box); the cells after it are then missing.
pub(super) fn parse_row<'a>(layout: &Layout, row: &'a [u8], stop_after_geometry: bool) -> Option<Vec<Cell<'a>>> {
    let nulls = row.get(..layout.null_bytes())?;
    let mut r = Bytes::new(row);
    r.skip(nulls.len())?;
    let mut nullable = 0usize;
    let mut out = Vec::with_capacity(layout.columns.len());
    for c in &layout.columns {
        if c.kind == Kind::ObjectId {
            out.push(Cell::Skipped);
            continue;
        }
        if c.nullable {
            let i = nullable;
            nullable += 1;
            if nulls.get(i / 8)? >> (i % 8) & 1 == 1 {
                out.push(Cell::Null);
                if c.kind == Kind::Geometry && stop_after_geometry {
                    return Some(out);
                }
                continue;
            }
        }
        let cell = match c.kind {
            Kind::Int16 => Cell::Value(Value::Int(i64::from(i16::from_le_bytes(r.array()?)))),
            Kind::Int32 => Cell::Value(Value::Int(i64::from(i32::from_le_bytes(r.array()?)))),
            Kind::Int64 => Cell::Value(Value::Int(i64::from_le_bytes(r.array()?))),
            Kind::Float32 => Cell::Value(float(f64::from(f32::from_le_bytes(r.array()?)))),
            Kind::Float64 => Cell::Value(float(f64::from_le_bytes(r.array()?))),
            Kind::DateTime => Cell::Value(date_value(f64::from_le_bytes(r.array()?), Dated::DateTime)),
            Kind::DateOnly => Cell::Value(date_value(f64::from_le_bytes(r.array()?), Dated::Date)),
            Kind::TimeOnly => Cell::Value(date_value(f64::from_le_bytes(r.array()?), Dated::Time)),
            Kind::DateTimeOffset => {
                let days = f64::from_le_bytes(r.array()?);
                let minutes = i16::from_le_bytes(r.array()?);
                Cell::Value(date_value(days, Dated::Offset(minutes)))
            }
            Kind::Text | Kind::Xml => {
                let n = usize::try_from(r.varuint()?).ok()?;
                Cell::Value(Value::Text(String::from_utf8_lossy(r.take(n)?).into_owned()))
            }
            Kind::Binary => {
                let n = usize::try_from(r.varuint()?).ok()?;
                r.skip(n)?;
                Cell::Skipped
            }
            Kind::Geometry => {
                let n = usize::try_from(r.varuint()?).ok()?;
                let g = Cell::Geometry(r.take(n)?);
                if stop_after_geometry {
                    out.push(g);
                    return Some(out);
                }
                g
            }
            Kind::Guid | Kind::GlobalId => Cell::Value(Value::Text(guid(r.take(16)?))),
            Kind::ObjectId => Cell::Skipped,
        };
        out.push(cell);
    }
    Some(out)
}

fn float(v: f64) -> Value {
    if v.is_finite() { Value::Float(v) } else { Value::Null }
}

/// A GUID as Esri writes it: `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`, its first three groups
/// stored little-endian.
fn guid(b: &[u8]) -> String {
    let h = |r: &[u8]| r.iter().map(|x| format!("{x:02X}")).collect::<String>();
    let rev = |r: &[u8]| r.iter().rev().map(|x| format!("{x:02X}")).collect::<String>();
    format!(
        "{{{}-{}-{}-{}-{}}}",
        rev(b.get(0..4).unwrap_or(&[])),
        rev(b.get(4..6).unwrap_or(&[])),
        rev(b.get(6..8).unwrap_or(&[])),
        h(b.get(8..10).unwrap_or(&[])),
        h(b.get(10..16).unwrap_or(&[]))
    )
}

#[derive(Clone, Copy)]
enum Dated {
    DateTime,
    Date,
    Time,
    /// A date and time with its offset from UTC, in minutes.
    Offset(i16),
}

/// A date stored as days since 1899-12-30 as text: `YYYY-MM-DDTHH:MM:SS`, `YYYY-MM-DD` for a
/// date alone, `HH:MM:SS` for a time of day (a fraction of a day), with `+HH:MM` for an offset.
fn date_value(days: f64, kind: Dated) -> Value {
    // Years 1 to 9999.
    if !days.is_finite() || !(-693_593.0..2_958_466.0).contains(&days) {
        return Value::Null;
    }
    let mut whole = days.floor() as i64;
    let mut secs = ((days - days.floor()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        whole += 1;
        secs -= 86_400;
    }
    let time = format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60);
    if let Dated::Time = kind {
        return Value::Text(time);
    }
    let (y, m, d) = civil(whole - 25_569);
    let date = format!("{y:04}-{m:02}-{d:02}");
    Value::Text(match kind {
        Dated::Date => date,
        Dated::Offset(o) => {
            let sign = if o < 0 { '-' } else { '+' };
            let o = o.unsigned_abs();
            format!("{date}T{time}{sign}{:02}:{:02}", o / 60, o % 60)
        }
        _ => format!("{date}T{time}"),
    })
}

/// The civil date of a day counted from 1970-01-01 (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// UTF-16 little-endian text (an odd last byte ignored).
fn utf16le(b: &[u8]) -> String {
    String::from_utf16_lossy(&b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect::<Vec<_>>())
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A reader over bytes that never reads past their end.
pub(super) struct Bytes<'a> {
    b: &'a [u8],
    pub pos: usize,
}

impl<'a> Bytes<'a> {
    pub(super) fn new(b: &'a [u8]) -> Self {
        Bytes { b, pos: 0 }
    }

    pub(super) fn remaining(&self) -> usize {
        self.b.len().saturating_sub(self.pos)
    }

    pub(super) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.pos..self.pos.checked_add(n)?)?;
        self.pos += n;
        Some(s)
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }

    pub(super) fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }

    fn peek(&self, at: usize) -> Option<u8> {
        self.b.get(self.pos.checked_add(at)?).copied()
    }

    fn peek_u32(&self, at: usize) -> Option<u32> {
        le_u32(self.b, self.pos.checked_add(at)?)
    }

    pub(super) fn u8(&mut self) -> Option<u8> {
        self.array::<1>().map(|b| b[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.array().map(u16::from_le_bytes)
    }

    pub(super) fn u32(&mut self) -> Option<u32> {
        self.array().map(u32::from_le_bytes)
    }

    pub(super) fn f64(&mut self) -> Option<f64> {
        self.array().map(f64::from_le_bytes)
    }

    /// A name: its length in UTF-16 units (one byte), then the units.
    fn utf16_u8len(&mut self) -> Option<String> {
        let n = usize::from(self.u8()?);
        let b = self.take(n * 2)?;
        Some(utf16le(b))
    }

    /// An unsigned number in 7-bit groups, least significant first, the top bit set on all but
    /// the last.
    pub(super) fn varuint(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let c = self.u8()?;
            v |= u64::from(c & 0x7f).checked_shl(shift)?;
            if c & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// A signed number: like [`Bytes::varuint`], but the first byte holds the sign (0x40) and
    /// six bits.
    pub(super) fn varint(&mut self) -> Option<i64> {
        let c = self.u8()?;
        let neg = c & 0x40 != 0;
        let mut v = u64::from(c & 0x3f);
        let mut more = c & 0x80 != 0;
        let mut shift = 6u32;
        while more {
            let c = self.u8()?;
            if shift >= 64 {
                return None;
            }
            v |= u64::from(c & 0x7f).checked_shl(shift)?;
            more = c & 0x80 != 0;
            shift += 7;
        }
        let v = i64::try_from(v).ok()?;
        Some(if neg { -v } else { v })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date_value(45_293.127_835_648_15, Dated::DateTime), Value::Text("2024-01-02T03:04:05".into()));
        assert_eq!(date_value(36_925.0, Dated::Date), Value::Text("2001-02-03".into()));
        assert_eq!(date_value(0.551_562_5, Dated::Time), Value::Text("13:14:15".into()));
        assert_eq!(date_value(45_418.297_326_388_885, Dated::Offset(-300)), Value::Text("2024-05-06T07:08:09-05:00".into()));
        assert_eq!(date_value(36_585.0, Dated::DateTime), Value::Text("2000-02-29T00:00:00".into()));
        assert_eq!(date_value(f64::NAN, Dated::Date), Value::Null);
        assert_eq!(date_value(1e300, Dated::Date), Value::Null);
    }

    #[test]
    fn guids_as_esri_writes_them() {
        let b = [0x78, 0x56, 0x34, 0x12, 0x34, 0x12, 0x34, 0x12, 0x12, 0x34, 0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc];
        assert_eq!(guid(&b), "{12345678-1234-1234-1234-123456789ABC}");
    }

    #[test]
    fn varints() {
        let mut r = Bytes::new(&[0x81, 0x01, 0x05, 0xc1, 0x01, 0xff]);
        assert_eq!(r.varuint(), Some(129));
        assert_eq!(r.varint(), Some(5));
        assert_eq!(r.varint(), Some(-65));
        // Cut short, and an endless run of continuation bytes, are refused.
        assert_eq!(r.varuint(), None);
        assert_eq!(Bytes::new(&[0xff; 20]).varuint(), None);
        assert_eq!(Bytes::new(&[0xff; 20]).varint(), None);
    }
}
