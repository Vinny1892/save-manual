//! Read-only parser for PCSX2 .ps2 memory-card files.
//!
//! Format reference: PS2DEV memcard wiki + PCSX2 source. Handles both
//! 8 MB layouts: with ECC (528-byte physical pages, ~8.25 MB file) and
//! without ECC (512-byte pages, exactly 8 MB). Detected by file size — ECC
//! files are stripped to a logical buffer before parsing.

use std::path::Path;

use chrono::{DateTime, Local, NaiveDate, TimeZone};
use serde::Serialize;

const MAGIC: &[u8] = b"Sony PS2 Memory Card Format ";
const ENTRY_SIZE: usize = 512;
const FAT_EOC: u32 = 0xFFFFFFFF;
const LOW_31: u32 = 0x7FFFFFFF;

const MODE_EXISTS: u16 = 0x8000;
const MODE_DIR: u16 = 0x0020;

#[derive(Debug, Clone, Serialize)]
pub struct McSave {
    pub name: String,
    pub serial: Option<String>,
    pub title: Option<String>,
    pub modified: Option<String>,
    pub size_bytes: u64,
}

struct SuperBlock {
    page_len: u32,
    pages_per_cluster: u32,
    clusters_per_card: u32,
    alloc_offset: u32,
    rootdir_cluster: u32,
    ifc_list: [u32; 32],
}

impl SuperBlock {
    fn cluster_size(&self) -> u32 {
        self.page_len * self.pages_per_cluster
    }

    fn raw_cluster_offset(&self, cluster: u32) -> usize {
        cluster as usize * self.cluster_size() as usize
    }

    fn alloc_cluster_offset(&self, cluster: u32) -> usize {
        self.raw_cluster_offset(self.alloc_offset + cluster)
    }
}

fn parse_super(data: &[u8]) -> Result<SuperBlock, String> {
    if data.len() < 0x158 {
        return Err("file too small to contain a superblock".into());
    }
    if !data.starts_with(MAGIC) {
        return Err("invalid magic — not a PS2 memcard".into());
    }
    let r16 = |off: usize| u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
    let r32 = |off: usize| u32::from_le_bytes(data[off..off + 4].try_into().unwrap());

    let page_len = r16(0x28) as u32;
    let pages_per_cluster = r16(0x2A) as u32;
    let clusters_per_card = r32(0x30);
    let alloc_offset = r32(0x34);
    let rootdir_cluster = r32(0x3C);

    let mut ifc_list = [0u32; 32];
    for (i, ifc) in ifc_list.iter_mut().enumerate() {
        *ifc = r32(0x50 + i * 4);
    }

    if page_len == 0 || pages_per_cluster == 0 {
        return Err("invalid page geometry".into());
    }
    Ok(SuperBlock {
        page_len,
        pages_per_cluster,
        clusters_per_card,
        alloc_offset,
        rootdir_cluster,
        ifc_list,
    })
}

/// FAT chain navigation: given an `alloc-relative` cluster, return the next
/// one in its chain, or `None` for end-of-chain.
fn fat_next(data: &[u8], sb: &SuperBlock, cluster: u32) -> Option<u32> {
    let entries_per_cluster = sb.cluster_size() / 4;
    let i = (cluster / entries_per_cluster / entries_per_cluster) as usize;
    let j = ((cluster / entries_per_cluster) % entries_per_cluster) as usize;
    let k = (cluster % entries_per_cluster) as usize;
    if i >= sb.ifc_list.len() {
        return None;
    }

    let ifc_cluster = sb.ifc_list[i];
    if ifc_cluster == FAT_EOC {
        return None;
    }
    let ifc_off = sb.raw_cluster_offset(ifc_cluster);
    let ifc_entry_off = ifc_off + j * 4;
    if ifc_entry_off + 4 > data.len() {
        return None;
    }
    let fat_cluster = u32::from_le_bytes(data[ifc_entry_off..ifc_entry_off + 4].try_into().unwrap());
    if fat_cluster == FAT_EOC {
        return None;
    }

    let fat_off = sb.raw_cluster_offset(fat_cluster);
    let fat_entry_off = fat_off + k * 4;
    if fat_entry_off + 4 > data.len() {
        return None;
    }
    let raw = u32::from_le_bytes(data[fat_entry_off..fat_entry_off + 4].try_into().unwrap());
    if raw == FAT_EOC || raw & LOW_31 == LOW_31 {
        return None;
    }
    // top bit = "in use" flag; the next-cluster index is in the lower 31.
    Some(raw & LOW_31)
}

fn walk_chain(data: &[u8], sb: &SuperBlock, start: u32) -> Vec<u32> {
    let mut out = vec![start];
    let mut cur = start;
    let mut guard = 0u32;
    while let Some(next) = fat_next(data, sb, cur) {
        if next == cur || guard > sb.clusters_per_card {
            break; // cycle / runaway
        }
        out.push(next);
        cur = next;
        guard += 1;
    }
    out
}

/// Extract a Sony serial code (e.g. SLUS-20002) from a save folder name.
/// Save folder names typically look like `BISLUS-20002...` or `BESLES-50000...`
/// — i.e. an arbitrary prefix (often "BI"/"BE"/"BA") followed by the canonical
/// `S[CL][UEKP]S-\d{5}` serial. We grab the first match anywhere in the name.
fn extract_serial(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    for i in 0..bytes.len().saturating_sub(9) {
        let window = &bytes[i..i + 10];
        if window[0] == b'S'
            && window[1].is_ascii_uppercase()
            && window[2].is_ascii_uppercase()
            && window[3].is_ascii_uppercase()
            && window[4] == b'-'
            && window[5..10].iter().all(|c| c.is_ascii_digit())
        {
            return Some(std::str::from_utf8(window).ok()?.to_string());
        }
    }
    None
}

/// Read a directory entry's `modified` timestamp (8 bytes, mc-native layout)
/// and format it as dd/mm/yyyy HH:MM. Treats the year as already absolute.
fn parse_mc_time(buf: &[u8]) -> Option<DateTime<Local>> {
    // layout: resv u8, sec u8, min u8, hour u8, day u8, month u8, year u16
    if buf.len() < 8 {
        return None;
    }
    let sec = buf[1] as u32;
    let min = buf[2] as u32;
    let hour = buf[3] as u32;
    let day = buf[4] as u32;
    let month = buf[5] as u32;
    let year = u16::from_le_bytes([buf[6], buf[7]]) as i32;
    if year < 1990 || year > 2100 || month == 0 || month > 12 || day == 0 || day > 31 {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year, month, day)?
        .and_hms_opt(hour, min, sec)?;
    Local.from_local_datetime(&date).single()
}

/// Strip the per-page ECC overhead (16 trailing bytes per 528-byte physical
/// page) when the file is in ECC layout. Returns the logical buffer used by
/// the rest of the parser.
fn strip_ecc(raw: Vec<u8>) -> Vec<u8> {
    const PHYS: usize = 528;
    const LOG: usize = 512;
    if raw.len() % PHYS != 0 {
        return raw;
    }
    let pages = raw.len() / PHYS;
    let logical_size = pages * LOG;
    // Common card capacities — guards against false positives on weird files.
    let known_sizes = [
        8 * 1024 * 1024,
        16 * 1024 * 1024,
        32 * 1024 * 1024,
        64 * 1024 * 1024,
    ];
    if !known_sizes.contains(&logical_size) {
        return raw;
    }
    let mut out = Vec::with_capacity(logical_size);
    for i in 0..pages {
        let start = i * PHYS;
        out.extend_from_slice(&raw[start..start + LOG]);
    }
    out
}

pub fn list_saves(memcard_path: &Path) -> Result<Vec<McSave>, String> {
    let raw = std::fs::read(memcard_path).map_err(|e| e.to_string())?;
    let data = strip_ecc(raw);

    // Friendlier message for unformatted/blank cards (PCSX2 creates these as
    // placeholders before a game writes to them).
    if data.iter().take(28).all(|&b| b == 0) {
        return Err("memcard vazio / não formatado".into());
    }

    let sb = parse_super(&data)?;

    // Walk the root dir's cluster chain and collect every directory entry.
    let chain = walk_chain(&data, &sb, sb.rootdir_cluster);
    let cluster_size = sb.cluster_size() as usize;
    let entries_per_cluster = cluster_size / ENTRY_SIZE;

    let mut saves = Vec::new();
    let mut seen_root_meta = 0u32;

    for cluster_rel in chain {
        let off = sb.alloc_cluster_offset(cluster_rel);
        if off + cluster_size > data.len() {
            break;
        }
        for slot in 0..entries_per_cluster {
            let entry_off = off + slot * ENTRY_SIZE;
            let entry = &data[entry_off..entry_off + ENTRY_SIZE];

            let mode = u16::from_le_bytes(entry[0..2].try_into().unwrap());
            if mode == 0 {
                continue;
            }
            // Name field, 32 bytes nul-terminated, at offset 0x40.
            let name_raw = &entry[0x40..0x40 + 32];
            let name_end = name_raw.iter().position(|&b| b == 0).unwrap_or(name_raw.len());
            let name = std::str::from_utf8(&name_raw[..name_end])
                .unwrap_or("")
                .to_string();

            // Skip "." / ".." first two entries of the root dir.
            if name == "." || name == ".." {
                seen_root_meta += 1;
                continue;
            }
            if mode & MODE_EXISTS == 0 || mode & MODE_DIR == 0 {
                continue;
            }
            if name.is_empty() {
                continue;
            }

            let length = u32::from_le_bytes(entry[0x04..0x08].try_into().unwrap());
            let cluster_start = u32::from_le_bytes(entry[0x10..0x14].try_into().unwrap());
            let modified = parse_mc_time(&entry[0x18..0x20])
                .map(|t| t.format("%d/%m/%Y %H:%M").to_string());

            // Approximate size: (#entries inside the save * ENTRY_SIZE).
            // Real per-file byte sizes would require walking each child entry,
            // which we skip for now — it's not worth the IO for a list view.
            let approx_size = length as u64 * ENTRY_SIZE as u64;

            let serial = match extract_serial(&name) {
                Some(s) => s,
                // Drop system-level directories like BADATA-SYSTEM that don't
                // carry a Sony serial — these are browser metadata, not saves.
                None => continue,
            };
            saves.push(McSave {
                name,
                serial: Some(serial),
                title: None,
                modified,
                size_bytes: approx_size,
            });
            // also: cluster_start used later if we ever read inner files
            let _ = cluster_start;
        }
    }
    let _ = seen_root_meta;

    saves.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(saves)
}
