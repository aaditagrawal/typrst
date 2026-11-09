//! System font discovery, caching, and sfnt metrics.
//!
//! `FontDb::discover` walks the platform font directories in parallel
//! (threads share a mutexed directory work-queue), parsing just enough of
//! each sfnt header to classify faces. The result is cached in
//! `~/.cache/typrst/fontdb-v1.bin` (`XDG_CACHE_HOME`-aware) and reused while
//! every scanned directory's mtime is unchanged, so a warm `discover()` is
//! a single file read. `Registry` adapts the db to the evaluator's
//! `FaceRegistry`; `load_metrics` reads cmap+hmtx, and a legacy `kern`
//! format 0 table when the face has one, for layout and PDF embedding.
use crate::ir::{FaceRegistry, FontId, FontUse};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::{env, fs, thread};

// ---------- big-endian readers (shared with pdf.rs) ----------

pub(crate) fn u16be(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
pub(crate) fn i16be(d: &[u8], o: usize) -> Option<i16> {
    Some(i16::from_be_bytes(d.get(o..o + 2)?.try_into().ok()?))
}
pub(crate) fn u32be(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}
fn i32be(d: &[u8], o: usize) -> Option<i32> {
    Some(i32::from_be_bytes(d.get(o..o + 4)?.try_into().ok()?))
}

/// Locate a table inside a face whose offset table starts at `base`.
pub(crate) fn table<'a>(d: &'a [u8], base: usize, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let n = u16be(d, base + 4)? as usize;
    for i in 0..n {
        let r = base + 12 + 16 * i;
        if d.get(r..r + 4) == Some(&tag[..]) {
            let (off, len) = (u32be(d, r + 8)? as usize, u32be(d, r + 12)? as usize);
            return d.get(off..off.saturating_add(len));
        }
    }
    None
}

/// Offset of a face's sfnt offset table inside `d` (handles ttc).
pub(crate) fn face_base(d: &[u8], ttc_index: u32) -> Option<usize> {
    if d.get(0..4) == Some(b"ttcf") {
        let n = u32be(d, 8)?;
        if ttc_index >= n {
            return None;
        }
        u32be(d, 12 + 4 * ttc_index as usize).map(|o| o as usize)
    } else if ttc_index == 0 {
        Some(0)
    } else {
        None
    }
}

fn is_sfnt(d: &[u8], base: usize) -> bool {
    matches!(
        u32be(d, base),
        Some(0x00010000 | 0x74727565 | 0x4F54544F) // 1.0 | 'true' | 'OTTO'
    )
}

// ---------- name table ----------

#[derive(Default)]
struct Names {
    fam: (u8, String),
    sub: (u8, String),
    ps: (u8, String),
}

fn bump(slot: &mut (u8, String), score: u8, s: String) {
    if !s.is_empty() && score > slot.0 {
        *slot = (score, s);
    }
}

/// Decode a name-table string. Windows (3) and Unicode (0) platforms are
/// UTF-16BE; Macintosh (1) is approximated as Latin-1 (exact for ASCII,
/// which is all real PostScript/family names use).
fn decode_name(platform: u16, s: &[u8]) -> String {
    if platform == 3 || platform == 0 {
        let u: Vec<u16> = s
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u).trim().to_string()
    } else {
        s.iter()
            .map(|&b| b as char)
            .collect::<String>()
            .trim()
            .to_string()
    }
}

fn names(tab: &[u8]) -> Names {
    let mut n = Names::default();
    let (Some(count), Some(str_base)) = (u16be(tab, 2), u16be(tab, 4)) else {
        return n;
    };
    for i in 0..count as usize {
        let r = 6 + 12 * i;
        let (Some(pf), Some(id), Some(len), Some(off)) = (
            u16be(tab, r),
            u16be(tab, r + 6),
            u16be(tab, r + 8),
            u16be(tab, r + 10),
        ) else {
            continue;
        };
        let start = str_base as usize + off as usize;
        let Some(raw) = tab.get(start..start + len as usize) else {
            continue;
        };
        let s = decode_name(pf, raw);
        // Platform 3 (Windows) preferred, then Unicode, then Mac.
        let prio = match pf {
            3 => 10,
            0 => 8,
            1 => 5,
            _ => 1,
        };
        match id {
            16 => bump(&mut n.fam, prio + 4, s), // typographic family
            1 => bump(&mut n.fam, prio + 2, s),  // legacy family
            17 => bump(&mut n.sub, prio + 4, s), // typographic subfamily
            2 => bump(&mut n.sub, prio + 2, s),  // legacy subfamily
            6 => bump(&mut n.ps, prio + 4, s),   // postscript name
            _ => {}
        }
    }
    n
}

// ---------- face parsing ----------

#[derive(Debug, Clone)]
pub struct FaceEntry {
    pub path: PathBuf,
    pub ttc_index: u32,
    pub family: String,
    pub subfamily: String,
    pub postscript_name: String,
    pub weight: u16, // OS/2 usWeightClass (400 regular, 700 bold)
    pub italic: bool,
    pub monospace: bool,
    pub upm: u16,
    pub ascent: i16,
    pub descent: i16,
}

fn parse_face(d: &[u8], base: usize, path: &Path, ttc_index: u32) -> Option<FaceEntry> {
    if !is_sfnt(d, base) {
        return None;
    }
    let head = table(d, base, b"head")?;
    let hhea = table(d, base, b"hhea")?;
    let upm = u16be(head, 18)?;
    let ascent = i16be(hhea, 4)?;
    let descent = i16be(hhea, 6)?;

    let n = table(d, base, b"name")
        .map(|t| names(t))
        .unwrap_or_default();
    let mut weight = table(d, base, b"OS/2")
        .and_then(|t| u16be(t, 4))
        .unwrap_or(400);
    let mut italic = table(d, base, b"OS/2")
        .and_then(|t| u16be(t, 62))
        .map(|fs| fs & 1 != 0)
        .unwrap_or(false);
    let monospace = table(d, base, b"post")
        .and_then(|t| u32be(t, 12))
        .map(|f| f != 0)
        .unwrap_or(false);

    // Infer weight from the subfamily when OS/2 is absent/implausible.
    // Order matters: "semibold"/"demi" contain "bold".
    let sub_l = n.sub.1.to_lowercase();
    if sub_l.contains("thin") {
        weight = 100;
    } else if sub_l.contains("light") {
        weight = 300;
    } else if sub_l.contains("semibold") || sub_l.contains("demi") {
        weight = 600;
    } else if weight == 400 && sub_l.contains("medium") {
        weight = 500;
    } else if sub_l.contains("black") || sub_l.contains("heavy") {
        weight = 900;
    } else if weight == 400 && sub_l.contains("bold") {
        weight = 700;
    }
    if !italic && (sub_l.contains("italic") || sub_l.contains("oblique")) {
        italic = true;
    }

    let mut family = n.fam.1;
    if family.is_empty() {
        family = path.file_stem()?.to_string_lossy().into_owned();
    }
    let mut postscript_name = n.ps.1;
    if postscript_name.is_empty() {
        postscript_name = family.split_whitespace().collect();
    }
    let mut subfamily = n.sub.1;
    if subfamily.is_empty() {
        subfamily = match (weight >= 600, italic) {
            (true, true) => "Bold Italic",
            (true, false) => "Bold",
            (false, true) => "Italic",
            (false, false) => "Regular",
        }
        .to_string();
    }
    Some(FaceEntry {
        path: path.to_path_buf(),
        ttc_index,
        family,
        subfamily,
        postscript_name,
        weight,
        italic,
        monospace,
        upm,
        ascent,
        descent,
    })
}

fn parse_font(path: &Path, d: &[u8], out: &mut Vec<FaceEntry>) {
    if d.get(0..4) == Some(b"ttcf") {
        if let Some(n) = u32be(d, 8) {
            for i in 0..n.min(64) {
                if let Some(base) = face_base(d, i) {
                    if let Some(e) = parse_face(d, base, path, i) {
                        out.push(e);
                    }
                }
            }
        }
    } else if let Some(e) = parse_face(d, 0, path, 0) {
        out.push(e);
    }
}

// ---------- discovery ----------

/// Normalize a family name for matching: lowercase, alphanumerics only.
pub(crate) fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

fn is_font_file(p: &Path) -> bool {
    // .dfont is a resource-fork wrapper we don't parse — just skip it.
    let e = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    e == "ttf" || e == "otf" || e == "ttc"
}

/// Directories to scan, in priority order.
pub fn scan_roots() -> Vec<PathBuf> {
    let mut v = vec![
        PathBuf::from("/System/Library/Fonts"),
        PathBuf::from("/Library/Fonts"),
        PathBuf::from("/System/Library/Fonts/Supplemental"),
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(home) = env::var_os("HOME") {
        let h = Path::new(&home);
        v.push(h.join("Library/Fonts"));
        v.push(h.join(".fonts"));
        v.push(h.join(".local/share/fonts"));
    }
    if let Ok(extra) = env::var("TYPRST_FONT_PATHS") {
        v.extend(
            extra
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from),
        );
    }
    v
}

type DirFp = (PathBuf, Option<(i64, u32)>); // dir -> mtime (secs, nanos)

fn dir_mtime(p: &Path) -> Option<(i64, u32)> {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| (d.as_secs() as i64, d.subsec_nanos()))
}

struct Work {
    dirs: Vec<PathBuf>,
    active: usize,
    seen: std::collections::HashSet<PathBuf>, // canonical dirs — symlink-cycle guard
}

/// Recursively walk `roots` in parallel, parsing each font's header.
/// Returns discovered faces plus every visited dir's mtime fingerprint.
fn scan(roots: &[PathBuf]) -> (Vec<FaceEntry>, Vec<DirFp>) {
    let seen = roots
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    let q = Arc::new(Mutex::new(Work {
        dirs: roots.to_vec(),
        active: 0,
        seen,
    }));
    let faces = Arc::new(Mutex::new(Vec::new()));
    let fps = Arc::new(Mutex::new(Vec::new()));
    let nthreads = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    thread::scope(|s| {
        for _ in 0..nthreads.max(1) {
            let (q, faces, fps) = (q.clone(), faces.clone(), fps.clone());
            s.spawn(move || loop {
                let dir = {
                    let mut w = q.lock().unwrap();
                    match w.dirs.pop() {
                        Some(d) => {
                            w.active += 1;
                            d
                        }
                        None if w.active == 0 => break,
                        None => {
                            drop(w);
                            thread::yield_now();
                            continue;
                        }
                    }
                };
                fps.lock().unwrap().push((dir.clone(), dir_mtime(&dir)));
                if let Ok(rd) = fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let p = e.path();
                        // Dirent type is free; only stat symlinks (to catch
                        // symlinked font dirs). `seen` breaks cycles.
                        let is_dir = match e.file_type() {
                            Ok(t) if t.is_dir() => true,
                            Ok(t) if t.is_symlink() => p.is_dir(),
                            _ => false,
                        };
                        if is_dir {
                            let canon = p.canonicalize().unwrap_or_else(|_| p.clone());
                            let mut w = q.lock().unwrap();
                            if w.seen.insert(canon) {
                                w.dirs.push(p);
                            }
                            drop(w);
                        } else if is_font_file(&p) {
                            if let Ok(d) = fs::read(&p) {
                                let mut v = Vec::new();
                                parse_font(&p, &d, &mut v);
                                if !v.is_empty() {
                                    faces.lock().unwrap().extend(v);
                                }
                            }
                        }
                    }
                }
                q.lock().unwrap().active -= 1;
            });
        }
    });
    let faces = faces.lock().unwrap().clone();
    let fps = fps.lock().unwrap().clone();
    (faces, fps)
}

// ---------- disk cache ----------

fn cache_file() -> Option<PathBuf> {
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))?;
    Some(base.join("typrst/fontdb-v1.bin"))
}

struct Wr(Vec<u8>);
impl Wr {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend(v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend(v.to_le_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.0.extend(v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend(v.to_le_bytes());
    }
    fn s(&mut self, v: &str) {
        self.u32(v.len() as u32);
        self.0.extend(v.as_bytes());
    }
    fn path(&mut self, p: &Path) {
        self.s(&p.to_string_lossy());
    }
}

struct Rd<'a> {
    d: &'a [u8],
    p: usize,
}
impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.p..self.p + n)?;
        self.p += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn i16(&mut self) -> Option<i16> {
        Some(i16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn s(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
    fn path(&mut self) -> Option<PathBuf> {
        self.s().map(PathBuf::from)
    }
}

const CACHE_MAGIC: u32 = 0x5446_4442; // "TFDB"
const CACHE_VER: u32 = 1;

fn write_cache(path: &Path, roots: &[PathBuf], fps: &[DirFp], faces: &[FaceEntry]) {
    let mut w = Wr(Vec::with_capacity(64 * 1024));
    w.u32(CACHE_MAGIC);
    w.u32(CACHE_VER);
    let mut rs = roots.to_vec();
    rs.sort();
    w.u32(rs.len() as u32);
    for r in &rs {
        w.path(r);
    }
    w.u32(fps.len() as u32);
    for (p, mt) in fps {
        w.path(p);
        match mt {
            Some((s, n)) => {
                w.u8(1);
                w.i64(*s);
                w.u32(*n);
            }
            None => w.u8(0),
        }
    }
    w.u32(faces.len() as u32);
    for f in faces {
        w.path(&f.path);
        w.u32(f.ttc_index);
        w.s(&f.family);
        w.s(&f.subfamily);
        w.s(&f.postscript_name);
        w.u16(f.weight);
        w.u8(f.italic as u8 | (f.monospace as u8) << 1);
        w.u16(f.upm);
        w.i16(f.ascent);
        w.i16(f.descent);
    }
    if let Some(dir) = path.parent() {
        if fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, &w.0).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Read the cache and validate it against the live filesystem. Every
/// directory we visited during the scan must still have the same mtime.
fn read_cache(path: &Path, roots: &[PathBuf]) -> Option<Vec<FaceEntry>> {
    let d = fs::read(path).ok()?;
    let mut r = Rd { d: &d, p: 0 };
    if r.u32()? != CACHE_MAGIC || r.u32()? != CACHE_VER {
        return None;
    }
    let nroots = r.u32()? as usize;
    let mut cached_roots = Vec::with_capacity(nroots);
    for _ in 0..nroots {
        cached_roots.push(r.path()?);
    }
    let mut want = roots.to_vec();
    want.sort();
    if cached_roots != want {
        return None; // dir set changed (e.g. TYPRST_FONT_PATHS)
    }
    let nfps = r.u32()? as usize;
    for _ in 0..nfps {
        let p = r.path()?;
        let mt = match r.u8()? {
            1 => Some((r.i64()?, r.u32()?)),
            _ => None,
        };
        if dir_mtime(&p) != mt {
            return None; // a directory changed — rescan
        }
    }
    let nfaces = r.u32()? as usize;
    let mut faces = Vec::with_capacity(nfaces);
    for _ in 0..nfaces {
        let path = r.path()?;
        let ttc_index = r.u32()?;
        let family = r.s()?;
        let subfamily = r.s()?;
        let postscript_name = r.s()?;
        let weight = r.u16()?;
        let flags = r.u8()?;
        faces.push(FaceEntry {
            path,
            ttc_index,
            family,
            subfamily,
            postscript_name,
            weight,
            italic: flags & 1 != 0,
            monospace: flags & 2 != 0,
            upm: r.u16()?,
            ascent: r.i16()?,
            descent: r.i16()?,
        });
    }
    Some(faces)
}

// ---------- FontDb ----------

pub struct FontDb {
    pub faces: Vec<FaceEntry>,
    index: HashMap<String, Vec<u32>>, // normalized family -> face indices
    ps_index: HashMap<String, u32>,   // normalized postscript name -> index
}

impl FontDb {
    /// Discover system fonts, using the disk cache when no scanned
    /// directory has changed. Cold scan is parallel; warm load is a
    /// single file read (<5ms).
    pub fn discover() -> FontDb {
        let roots = scan_roots();
        if let Some(cf) = cache_file() {
            if let Some(faces) = read_cache(&cf, &roots) {
                return FontDb::build(faces);
            }
        }
        let (faces, fps) = scan(&roots);
        if let Some(cf) = cache_file() {
            write_cache(&cf, &roots, &fps, &faces);
        }
        FontDb::build(faces)
    }

    fn build(mut faces: Vec<FaceEntry>) -> FontDb {
        faces.sort_by(|a, b| {
            (&a.path, a.ttc_index, &a.postscript_name).cmp(&(
                &b.path,
                b.ttc_index,
                &b.postscript_name,
            ))
        });
        faces.dedup_by(|a, b| a.path == b.path && a.ttc_index == b.ttc_index);
        let mut index: HashMap<String, Vec<u32>> = HashMap::new();
        let mut ps_index = HashMap::new();
        for (i, f) in faces.iter().enumerate() {
            index.entry(norm(&f.family)).or_default().push(i as u32);
            ps_index.entry(norm(&f.postscript_name)).or_insert(i as u32);
        }
        FontDb {
            faces,
            index,
            ps_index,
        }
    }

    pub fn len(&self) -> usize {
        self.faces.len()
    }
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Best face for `family` at the given style. Exact normalized family
    /// match preferred, then postscript name, then substring fuzz.
    #[allow(dead_code)] // convenience wrapper over find_idx
    pub fn find(&self, family: &str, bold: bool, italic: bool) -> Option<&FaceEntry> {
        self.find_idx(family, bold, italic, false)
            .map(|i| &self.faces[i])
    }

    fn find_idx(&self, family: &str, bold: bool, italic: bool, mono: bool) -> Option<usize> {
        let key = norm(family);
        if key.is_empty() {
            return None;
        }
        let cand: Vec<usize> = if let Some(v) = self.index.get(&key) {
            v.iter().map(|&i| i as usize).collect()
        } else if let Some(&i) = self.ps_index.get(&key) {
            vec![i as usize]
        } else {
            self.faces
                .iter()
                .enumerate()
                .filter(|(_, f)| {
                    let nf = norm(&f.family);
                    nf.contains(&key) || key.contains(&nf)
                })
                .map(|(i, _)| i)
                .collect()
        };
        let want_w = if bold { 700i32 } else { 400 };
        cand.into_iter().min_by_key(|&i| {
            let f = &self.faces[i];
            (f.weight as i32 - want_w).unsigned_abs()
                + if f.italic != italic { 500 } else { 0 }
                + if mono && !f.monospace { 200 } else { 0 }
        })
    }
}

// ---------- FaceRegistry adapter ----------

/// Bridges `FontDb` to eval's `FaceRegistry`: resolves a requested
/// family/style to a `FontId`, appending `FontUse` entries for new faces.
/// The db may be lazily discovered on first non-alias lookup via a
/// `OnceCell`, so documents that only use base-14 fonts never pay for
/// a system scan.
pub struct Registry<'a> {
    db: DbSource<'a>,
    fonts: &'a mut Vec<FontUse>,
    map: HashMap<u32, u16>, // db face index -> Ext index
}

enum DbSource<'a> {
    Ready(&'a FontDb),
    Lazy(&'a std::cell::OnceCell<FontDb>),
}

impl DbSource<'_> {
    fn get(&self) -> &FontDb {
        match self {
            DbSource::Ready(db) => db,
            DbSource::Lazy(cell) => cell.get_or_init(FontDb::discover),
        }
    }
}

impl<'a> Registry<'a> {
    pub fn new(db: &'a FontDb, fonts: &'a mut Vec<FontUse>) -> Self {
        Registry {
            db: DbSource::Ready(db),
            fonts,
            map: HashMap::new(),
        }
    }

    /// Discover the font db only on first non-alias resolve.
    pub fn lazy(cell: &'a std::cell::OnceCell<FontDb>, fonts: &'a mut Vec<FontUse>) -> Self {
        Registry {
            db: DbSource::Lazy(cell),
            fonts,
            map: HashMap::new(),
        }
    }
}

fn serif_id(bold: bool, italic: bool) -> FontId {
    match (bold, italic) {
        (true, true) => FontId::BoldItalic,
        (true, false) => FontId::Bold,
        (false, true) => FontId::Italic,
        (false, false) => FontId::Regular,
    }
}

impl FaceRegistry for Registry<'_> {
    fn resolve(&mut self, family: &str, bold: bool, italic: bool, mono: bool) -> Option<FontId> {
        let n = norm(family);
        if n.is_empty() {
            return None;
        }
        // Base-14 aliases skip the db entirely. There is no sans base
        // face, so sans aliases resolve to the Times stack.
        let mono_id = if bold { FontId::MonoBold } else { FontId::Mono };
        match n.as_str() {
            "courier" | "couriernew" | "monospace" | "mono" => return Some(mono_id),
            "times" | "timesnewroman" | "serif" | "roman" | "helvetica" | "helveticaneue"
            | "arial" | "sans" | "sansserif" => return Some(serif_id(bold, italic)),
            _ => {}
        }
        let db = self.db.get();
        let i = db.find_idx(family, bold, italic, mono)?;
        if let Some(&e) = self.map.get(&(i as u32)) {
            return Some(FontId::Ext(e));
        }
        let f = &db.faces[i];
        self.fonts.push(FontUse {
            postscript_name: f.postscript_name.clone(),
            family: family.to_string(),
            path: f.path.clone(),
            ttc_index: f.ttc_index,
        });
        let e = (self.fonts.len() - 1) as u16;
        self.map.insert(i as u32, e);
        Some(FontId::Ext(e))
    }
}

// ---------- per-face metrics (cmap + hmtx) ----------

/// Glyph mapping and advances for one discovered face, used for layout
/// (`DbMetrics`) and for the PDF `/Widths` array.
pub struct FaceMetrics {
    pub upm: u16,
    pub ascent: i16,  // hhea ascent, font units
    pub descent: i16, // hhea descent, font units (negative)
    pub bbox: [i16; 4],
    pub italic_angle: i32, // post.italicAngle, 16.16 fixed
    widths: Vec<u16>,      // hmtx advances, numGlyphs entries
    cmap4: Option<Vec<u8>>,
    cmap12: Option<Vec<u8>>,
    /// Legacy `kern` format 0: (left glyph, right glyph) -> font units.
    /// Empty when the table is absent or not a format we read.
    kern: HashMap<(u16, u16), i16>,
}

impl FaceMetrics {
    /// Glyph id for a Unicode scalar (0 = .notdef).
    pub fn glyph(&self, c: char) -> u32 {
        let cp = c as u32;
        if cp <= 0xFFFF {
            if let Some(t) = &self.cmap4 {
                let g = cmap4_lookup(t, cp as u16);
                if g != 0 {
                    return g as u32;
                }
            }
        }
        if let Some(t) = &self.cmap12 {
            return cmap12_lookup(t, cp);
        }
        0
    }

    /// Advance of `c` in 1/1000 em. Unmapped chars use '?' like the
    /// base-14 tables; a missing '?' falls back to .notdef, then 500.
    /// A single glyph has no kerning partner; pair adjustments are
    /// applied in [`str_width`](Self::str_width).
    pub fn char_width(&self, c: char) -> f64 {
        self.advance(self.glyph_id(c)) as f64 * 1000.0 / self.upm.max(1) as f64
    }

    /// Width of `s` at `size` pt. hmtx advances, plus a format-0 kern
    /// value between consecutive glyphs when the face has that pair.
    pub fn str_width(&self, size: f64, s: &str) -> f64 {
        let scale = 1000.0 / self.upm.max(1) as f64;
        let mut sum = 0.0;
        let mut prev: Option<u16> = None;
        for c in s.chars() {
            let g = self.glyph_id(c);
            sum += self.advance(g) as f64 * scale;
            if let (Some(left), Some(right)) = (prev, u16::try_from(g).ok()) {
                if let Some(k) = self.kern.get(&(left, right)) {
                    sum += *k as f64 * scale;
                }
            }
            prev = u16::try_from(g).ok();
        }
        sum * size / 1000.0
    }

    fn glyph_id(&self, c: char) -> u32 {
        let mut g = self.glyph(c);
        if g == 0 && c != '?' {
            g = self.glyph('?');
        }
        g
    }

    fn advance(&self, g: u32) -> u16 {
        self.widths
            .get(g as usize)
            .or(self.widths.first())
            .copied()
            .unwrap_or(500)
    }

    /// Typographic ascent in 1/1000 em.
    pub fn ascent(&self) -> f64 {
        self.ascent as f64 * 1000.0 / self.upm.max(1) as f64
    }
    /// Typographic descent in 1/1000 em (positive).
    pub fn descent(&self) -> f64 {
        (self.descent as f64).abs() * 1000.0 / self.upm.max(1) as f64
    }
    /// Font bbox scaled to 1/1000 em.
    pub fn bbox_1000(&self) -> [i64; 4] {
        let s = 1000.0 / self.upm.max(1) as f64;
        self.bbox.map(|v| (v as f64 * s).round() as i64)
    }
}

fn cmap4_lookup(t: &[u8], c: u16) -> u16 {
    let Some(seg) = u16be(t, 6).map(|x| x as usize / 2) else {
        return 0;
    };
    let (end, start, delta, roff) = (14, 14 + 2 * seg + 2, 14 + 4 * seg + 2, 14 + 6 * seg + 2);
    // First segment whose endCode >= c.
    let (mut lo, mut hi) = (0, seg);
    while lo < hi {
        let mid = (lo + hi) / 2;
        match u16be(t, end + 2 * mid) {
            Some(e) if e >= c => hi = mid,
            Some(_) => lo = mid + 1,
            None => return 0,
        }
    }
    let Some(s) = u16be(t, start + 2 * lo) else {
        return 0;
    };
    if c < s {
        return 0;
    }
    let Some(d) = i16be(t, delta + 2 * lo) else {
        return 0;
    };
    match u16be(t, roff + 2 * lo) {
        Some(0) => ((c as i32 + d as i32) & 0xFFFF) as u16,
        Some(r) => {
            let ga = roff + 2 * lo + r as usize + 2 * (c - s) as usize;
            match u16be(t, ga) {
                Some(0) | None => 0,
                Some(g) => ((g as i32 + d as i32) & 0xFFFF) as u16,
            }
        }
        None => 0,
    }
}

fn cmap12_lookup(t: &[u8], c: u32) -> u32 {
    let Some(n) = u32be(t, 12) else {
        return 0;
    };
    let (mut lo, mut hi) = (0u32, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let g = 16 + 12 * mid as usize;
        let (Some(_s), Some(e), Some(_g0)) = (u32be(t, g), u32be(t, g + 4), u32be(t, g + 8)) else {
            return 0;
        };
        if e >= c {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    if lo >= n {
        return 0;
    }
    let g = 16 + 12 * lo as usize;
    let (Some(s), Some(e), Some(g0)) = (u32be(t, g), u32be(t, g + 4), u32be(t, g + 8)) else {
        return 0;
    };
    if c >= s && c <= e {
        g0 + (c - s)
    } else {
        0
    }
}

/// Legacy OpenType `kern` version 0, format 0.
///
/// Coverage bits (OpenType): 8–15 format, 0 horizontal, 1 minimum,
/// 2 cross-stream, 3 override. Only horizontal format 0 is applied.
/// An Apple version-1.0 header or any other format yields an empty map,
/// so widths stay hmtx-only. A single subtable whose `length` is shorter
/// than its pairs (Open Sans, Calibri) is read to the end of the table.
fn kern_format0(tab: &[u8]) -> HashMap<(u16, u16), i16> {
    let mut map = HashMap::new();
    if u16be(tab, 0) != Some(0) {
        return map;
    }
    let Some(n_tables) = u16be(tab, 2) else {
        return map;
    };
    let only = n_tables == 1;
    let mut off = 4usize;
    for _ in 0..n_tables {
        if off.saturating_add(6) > tab.len() {
            break;
        }
        let (Some(length), Some(coverage)) = (u16be(tab, off + 2), u16be(tab, off + 4)) else {
            break;
        };
        let format = coverage >> 8;
        let flags = coverage & 0xff;
        if format == 0 && flags & 0x1 != 0 && flags & 0x6 == 0 {
            let end = kern_pair_end(tab, off, length as usize, only);
            read_kern_pairs(tab, off, end, flags & 0x8 != 0, &mut map);
        }
        if length < 6 {
            break;
        }
        off = off.saturating_add(length as usize);
    }
    map
}

fn kern_pair_end(tab: &[u8], off: usize, length: usize, only: bool) -> usize {
    let declared = off.saturating_add(length).min(tab.len());
    if !only {
        return declared;
    }
    match u16be(tab, off + 6) {
        Some(n) => off
            .saturating_add(14 + 6 * n as usize)
            .min(tab.len())
            .max(declared),
        None => declared,
    }
}

fn read_kern_pairs(
    tab: &[u8],
    off: usize,
    end: usize,
    replace: bool,
    map: &mut HashMap<(u16, u16), i16>,
) {
    if off.saturating_add(14) > end {
        return;
    }
    let Some(n) = u16be(tab, off + 6) else {
        return;
    };
    let mut p = off + 14;
    for _ in 0..n {
        if p.saturating_add(6) > end {
            break;
        }
        let (Some(left), Some(right), Some(value)) =
            (u16be(tab, p), u16be(tab, p + 2), i16be(tab, p + 4))
        else {
            break;
        };
        if replace {
            map.insert((left, right), value);
        } else if value != 0 {
            map.entry((left, right))
                .and_modify(|slot| *slot = slot.saturating_add(value))
                .or_insert(value);
        }
        p += 6;
    }
}

/// Parse cmap (fmt 4 + 12), hmtx, hhea, head, maxp, post, and kern for one face.
pub fn load_metrics(path: &Path, ttc_index: u32) -> Option<FaceMetrics> {
    let d = fs::read(path).ok()?;
    let base = face_base(&d, ttc_index)?;
    if !is_sfnt(&d, base) {
        return None;
    }
    let head = table(&d, base, b"head")?;
    let hhea = table(&d, base, b"hhea")?;
    let hmtx = table(&d, base, b"hmtx")?;
    let maxp = table(&d, base, b"maxp")?;
    let upm = u16be(head, 18)?;
    let bbox = [
        i16be(head, 36).unwrap_or(0),
        i16be(head, 38).unwrap_or(0),
        i16be(head, 40).unwrap_or(0),
        i16be(head, 42).unwrap_or(0),
    ];
    let num_h = u16be(hhea, 34)? as usize;
    let num_g = u16be(maxp, 4)? as usize;
    let mut widths = Vec::with_capacity(num_g);
    let mut last = 0u16;
    for g in 0..num_g.max(1) {
        let i = g.min(num_h.saturating_sub(1));
        if let Some(a) = u16be(hmtx, 4 * i) {
            last = a;
        }
        widths.push(last);
    }
    let italic_angle = table(&d, base, b"post")
        .and_then(|t| i32be(t, 4))
        .unwrap_or(0);

    // Pick the best cmap subtable of each format we support.
    let (mut c4, mut c12, mut s4, mut s12) = (None, None, 0, 0);
    if let Some(cmap) = table(&d, base, b"cmap") {
        if let Some(nsub) = u16be(cmap, 2) {
            for i in 0..nsub as usize {
                let r = 4 + 8 * i;
                let (Some(pf), Some(enc), Some(off)) =
                    (u16be(cmap, r), u16be(cmap, r + 2), u32be(cmap, r + 4))
                else {
                    continue;
                };
                let Some(sub) = cmap.get(off as usize..) else {
                    continue;
                };
                let score = match (pf, enc) {
                    (3, 10) | (0, 6) => 6,
                    (0, _) => 5,
                    (3, 1) | (3, 0) => 4,
                    (1, 0) => 2,
                    _ => 1,
                };
                match u16be(sub, 0) {
                    Some(4) if score > s4 => {
                        s4 = score;
                        c4 = Some(sub.to_vec());
                    }
                    Some(12) if score > s12 => {
                        s12 = score;
                        c12 = Some(sub.to_vec());
                    }
                    _ => {}
                }
            }
        }
    }
    Some(FaceMetrics {
        upm,
        ascent: i16be(hhea, 4)?,
        descent: i16be(hhea, 6)?,
        bbox,
        italic_angle,
        widths,
        cmap4: c4,
        cmap12: c12,
        kern: table(&d, base, b"kern")
            .map(kern_format0)
            .unwrap_or_default(),
    })
}

// ---------- tests ----------

/// A minimal but valid sfnt table set: upm 1000, ascent 800, descent -200,
/// 3 glyphs (advances 500/600/700; cmap maps 'A'->1, '?'->2).
#[cfg(test)]
pub(crate) fn synth_tables(
    family: &str,
    sub: &str,
    ps: &str,
    weight: u16,
    italic: bool,
    mono: bool,
) -> Vec<(&'static [u8; 4], Vec<u8>)> {
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&1000u16.to_be_bytes()); // unitsPerEm
    head[36..44].copy_from_slice(&[0i16, -200, 1000, 800].map(|v| v.to_be_bytes()).concat());
    let mut hhea = vec![0u8; 36];
    hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
    hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&3u16.to_be_bytes()); // numHMetrics
    let mut maxp = vec![0u8; 6];
    maxp[4..6].copy_from_slice(&3u16.to_be_bytes()); // numGlyphs
                                                     // hmtx: (advance, lsb) pairs — 500/600/700 advances, lsb 0.
    let hmtx = [500u16, 0, 600, 0, 700, 0]
        .map(|a| a.to_be_bytes())
        .concat();
    let mut os2 = vec![0u8; 78];
    os2[4..6].copy_from_slice(&weight.to_be_bytes()); // usWeightClass
    os2[62..64].copy_from_slice(&(italic as u16).to_be_bytes()); // fsSelection bit0
    let mut post = vec![0u8; 32];
    post[0..4].copy_from_slice(&0x00030000u32.to_be_bytes());
    post[12..16].copy_from_slice(&(mono as u32).to_be_bytes()); // isFixedPitch

    // cmap fmt4: '?'(0x3F)->gid 2, 'A'-'Z'->gid 1..26, terminator seg.
    // Only 'A' and '?' are exercised by tests; numGlyphs stays 3.
    let seg = 3usize;
    let ends = [0x003Fu16, 0x005A, 0xFFFF];
    let starts = [0x003Fu16, 0x0041, 0xFFFF];
    let deltas = [(2i32 - 0x3F) as i16, (1 - 0x41) as i16, 1];
    let mut c4 = Vec::new();
    c4.extend(4u16.to_be_bytes());
    c4.extend(((16 + 8 * seg) as u16).to_be_bytes());
    c4.extend(0u16.to_be_bytes()); // language
    c4.extend((2 * seg as u16).to_be_bytes());
    c4.extend(4u16.to_be_bytes()); // searchRange
    c4.extend(1u16.to_be_bytes()); // entrySelector
    c4.extend(2u16.to_be_bytes()); // rangeShift
    for v in ends {
        c4.extend(v.to_be_bytes());
    }
    c4.extend(0u16.to_be_bytes()); // reservedPad
    for v in starts {
        c4.extend(v.to_be_bytes());
    }
    for v in deltas {
        c4.extend(v.to_be_bytes());
    }
    for _ in 0..seg {
        c4.extend(0u16.to_be_bytes()); // idRangeOffset
    }
    let mut cmap = Vec::new();
    cmap.extend(0u16.to_be_bytes());
    cmap.extend(1u16.to_be_bytes());
    cmap.extend(3u16.to_be_bytes()); // platform 3
    cmap.extend(1u16.to_be_bytes()); // encoding 1
    cmap.extend(12u32.to_be_bytes());
    cmap.extend(&c4);

    // name: Windows platform, ids 16 family, 2 subfamily, 6 postscript
    let entries: [(u16, &str); 3] = [(16, family), (2, sub), (6, ps)];
    let mut store = Vec::new();
    let mut recs = Vec::new();
    for (id, s) in entries {
        let u: Vec<u8> = s.encode_utf16().flat_map(|c| c.to_be_bytes()).collect();
        recs.extend(3u16.to_be_bytes());
        recs.extend(1u16.to_be_bytes()); // enc
        recs.extend(0x0409u16.to_be_bytes()); // lang
        recs.extend(id.to_be_bytes());
        recs.extend((u.len() as u16).to_be_bytes());
        recs.extend((store.len() as u16).to_be_bytes());
        store.extend(u);
    }
    let mut name = Vec::new();
    name.extend(0u16.to_be_bytes());
    name.extend(3u16.to_be_bytes());
    name.extend(((6 + 12 * 3) as u16).to_be_bytes());
    name.extend(recs);
    name.extend(store);

    vec![
        (b"cmap", cmap),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"maxp", maxp),
        (b"name", name),
        (b"OS/2", os2),
        (b"post", post),
    ]
}

/// OpenType `kern` version 0 with one horizontal format-0 subtable.
#[cfg(test)]
pub(crate) fn synth_kern(pairs: &[(u16, u16, i16)]) -> Vec<u8> {
    let n = pairs.len() as u16;
    let sub_len = 14 + 6 * pairs.len();
    let mut t = Vec::new();
    t.extend(0u16.to_be_bytes());
    t.extend(1u16.to_be_bytes());
    t.extend(0u16.to_be_bytes());
    t.extend((sub_len as u16).to_be_bytes());
    t.extend(0x0001u16.to_be_bytes()); // format 0, horizontal
    t.extend(n.to_be_bytes());
    t.extend(0u16.to_be_bytes());
    t.extend(0u16.to_be_bytes());
    t.extend(0u16.to_be_bytes());
    for (left, right, value) in pairs {
        t.extend(left.to_be_bytes());
        t.extend(right.to_be_bytes());
        t.extend(value.to_be_bytes());
    }
    t
}

/// Assemble sfnt bytes from (tag, data) tables. Checksums are computed;
/// head.checkSumAdjustment is zeroed (valid for embedding tests).
#[cfg(test)]
pub(crate) fn synth_ttf(tables: &[(&'static [u8; 4], Vec<u8>)]) -> Vec<u8> {
    fn pad4(v: &mut Vec<u8>) {
        while v.len() % 4 != 0 {
            v.push(0);
        }
    }
    let n = tables.len();
    let mut out = Vec::new();
    out.extend(0x00010000u32.to_be_bytes());
    out.extend((n as u16).to_be_bytes());
    out.extend(0u16.to_be_bytes()); // searchRange etc: not validated
    out.extend(0u16.to_be_bytes());
    out.extend(0u16.to_be_bytes());
    let dir = out.len();
    out.resize(dir + 16 * n, 0);
    for (i, (tag, data)) in tables.iter().enumerate() {
        pad4(&mut out);
        let off = out.len();
        out.extend(data);
        let r = dir + 16 * i;
        out[r..r + 4].copy_from_slice(&tag[..]);
        out[r + 8..r + 12].copy_from_slice(&(off as u32).to_be_bytes());
        out[r + 12..r + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
    }
    out
}

/// Wrap faces into a 'ttcf' collection.
#[cfg(test)]
pub(crate) fn synth_ttc(faces: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(b"ttcf");
    out.extend(0x00010000u32.to_be_bytes());
    out.extend((faces.len() as u32).to_be_bytes());
    let mut off = 12 + 4 * faces.len();
    for f in faces {
        out.extend((off as u32).to_be_bytes());
        off += f.len();
    }
    for f in faces {
        out.extend(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ttf(family: &str, sub: &str, ps: &str, weight: u16, italic: bool, mono: bool) -> Vec<u8> {
        synth_ttf(&synth_tables(family, sub, ps, weight, italic, mono))
    }

    #[test]
    fn normalize_names() {
        assert_eq!(norm("Helvetica Neue"), "helveticaneue");
        assert_eq!(norm("Times-New Roman"), "timesnewroman");
        assert_eq!(norm("  Menlo  "), "menlo");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn parse_synth_face() {
        let d = ttf("Test Family", "Bold", "TestFam-Bold", 700, false, true);
        let e = parse_face(&d, 0, Path::new("/x/test.ttf"), 0).unwrap();
        assert_eq!(e.family, "Test Family");
        assert_eq!(e.subfamily, "Bold");
        assert_eq!(e.postscript_name, "TestFam-Bold");
        assert_eq!(e.weight, 700);
        assert!(!e.italic);
        assert!(e.monospace);
        assert_eq!(e.upm, 1000);
        assert_eq!((e.ascent, e.descent), (800, -200));
    }

    #[test]
    fn ttc_faces() {
        let f = ttf("Test Family", "Bold", "TestFam-Bold", 700, false, true);
        let d = synth_ttc(&[f.clone(), f]);
        let mut v = Vec::new();
        parse_font(Path::new("/x/a.ttc"), &d, &mut v);
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].ttc_index, 1);
    }

    #[test]
    fn metrics_lookup() {
        let dir = env::temp_dir().join(format!("typrst-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("synth.ttf");
        fs::write(
            &p,
            ttf("Test Family", "Bold", "TestFam-Bold", 700, false, true),
        )
        .unwrap();
        let m = load_metrics(&p, 0).unwrap();
        assert_eq!(m.glyph('A'), 1);
        assert_eq!(m.glyph('?'), 2);
        assert_eq!(m.glyph('\u{10FFFF}'), 0);
        assert_eq!(m.char_width('A'), 600.0);
        assert_eq!(m.char_width('?'), 700.0);
        assert_eq!(m.char_width('\u{1F600}'), 700.0); // unmappable -> '?'
        assert_eq!(m.ascent(), 800.0);
        assert_eq!(m.descent(), 200.0);
        // No kern table: the string width is the hmtx sum.
        assert_eq!(m.str_width(1000.0, "A?"), m.char_width('A') + m.char_width('?'));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kern_format0_adjusts_pair() {
        let dir = env::temp_dir().join(format!("typrst-kern-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("kern.ttf");
        let mut tables = synth_tables("Kern Face", "Regular", "KernFace", 400, false, false);
        // Glyph 1 is 'A' (advance 600). The pair pulls the two together by 100.
        tables.push((b"kern", synth_kern(&[(1, 1, -100)])));
        fs::write(&p, synth_ttf(&tables)).unwrap();
        let m = load_metrics(&p, 0).unwrap();
        assert_eq!(m.char_width('A'), 600.0);
        assert_eq!(m.str_width(10.0, "A"), 6.0);
        assert_eq!(m.str_width(10.0, "AA"), 11.0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn installed_serif_kern_narrows_av() {
        let p = Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf");
        if !p.exists() {
            return;
        }
        let m = load_metrics(p, 0).unwrap();
        let pair = m.str_width(1000.0, "AV");
        let sum = m.char_width('A') + m.char_width('V');
        assert!(pair < sum, "kern should narrow AV: {pair} vs {sum}");
    }

    #[test]
    fn kern_unknown_format_stays_hmtx() {
        assert!(kern_format0(&[]).is_empty());
        // Apple version 1.0 header is not the legacy table.
        assert!(kern_format0(&0x00010000u32.to_be_bytes()).is_empty());
        // Format 2 (high byte), horizontal bit set: not read.
        let mut t = Vec::new();
        t.extend(0u16.to_be_bytes());
        t.extend(1u16.to_be_bytes());
        t.extend(0u16.to_be_bytes());
        t.extend(14u16.to_be_bytes());
        t.extend(0x0201u16.to_be_bytes());
        t.extend(0u16.to_be_bytes());
        t.extend(0u16.to_be_bytes());
        t.extend(0u16.to_be_bytes());
        t.extend(0u16.to_be_bytes());
        assert!(kern_format0(&t).is_empty());
        // Format 0 but vertical (horizontal bit clear).
        t[8] = 0;
        t[9] = 0;
        assert!(kern_format0(&t).is_empty());
    }

    #[test]
    fn find_prefers_style() {
        let mut v = Vec::new();
        parse_font(
            Path::new("/a.ttf"),
            &ttf("Test Family", "Bold", "TestFam-Bold", 700, false, true),
            &mut v,
        );
        parse_font(
            Path::new("/b.ttf"),
            &ttf(
                "Test Family",
                "Regular",
                "TestFam-Regular",
                400,
                false,
                false,
            ),
            &mut v,
        );
        let db = FontDb::build(v);
        let b = db.find("test family", true, false).unwrap();
        assert_eq!(b.path, Path::new("/a.ttf"));
        let r = db.find("Test-Family", false, false).unwrap();
        assert_eq!(r.path, Path::new("/b.ttf"));
        // Fuzzy: substring match.
        assert!(db.find("test", false, false).is_some());
        assert!(db.find("nothing-here", false, false).is_none());
    }

    #[test]
    fn registry_aliases_and_dedup() {
        let mut v = Vec::new();
        parse_font(
            Path::new("/a.ttf"),
            &ttf("Test Family", "Bold", "TestFam-Bold", 700, false, true),
            &mut v,
        );
        let db = FontDb::build(v);
        let mut fonts = Vec::new();
        let mut r = Registry::new(&db, &mut fonts);
        assert_eq!(
            r.resolve("times", false, false, false),
            Some(FontId::Regular)
        );
        assert_eq!(
            r.resolve("serif", true, true, false),
            Some(FontId::BoldItalic)
        );
        assert_eq!(
            r.resolve("monospace", false, false, true),
            Some(FontId::Mono)
        );
        assert_eq!(
            r.resolve("mono", true, false, false),
            Some(FontId::MonoBold)
        );
        let a = r.resolve("Test Family", true, false, false).unwrap();
        let b = r.resolve("test family", true, false, false).unwrap();
        assert_eq!(a, b); // same face dedups to same Ext index
        assert!(matches!(a, FontId::Ext(0)));
        assert_eq!(r.resolve("nonexistent-xyz", false, false, false), None);
        drop(r);
        assert_eq!(fonts.len(), 1);
        assert_eq!(fonts[0].postscript_name, "TestFam-Bold");
    }

    #[test]
    fn cache_roundtrip() {
        let dir = env::temp_dir().join(format!("typrst-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let fontdir = dir.join("fonts");
        fs::create_dir_all(&fontdir).unwrap();
        let cp = dir.join("fontdb-v1.bin");
        let roots = vec![fontdir.clone()];
        let fps = vec![(fontdir.clone(), dir_mtime(&fontdir))];
        let mut faces = Vec::new();
        parse_font(
            Path::new("/a.ttf"),
            &ttf("Test Family", "Bold", "TestFam-Bold", 700, false, true),
            &mut faces,
        );
        write_cache(&cp, &roots, &fps, &faces);
        let got = read_cache(&cp, &roots).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].postscript_name, "TestFam-Bold");
        // Wrong roots -> invalid.
        assert!(read_cache(&cp, &[PathBuf::from("/elsewhere")]).is_none());
        // Corrupt -> invalid.
        fs::write(&cp, b"garbage").unwrap();
        assert!(read_cache(&cp, &roots).is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discover_returns_and_caches() {
        // On macOS this finds hundreds of faces; elsewhere it may find
        // none — either way it must not panic and must be consistent.
        let db = FontDb::discover();
        let _ = db.len();
        // Warm second call hits the cache.
        let db2 = FontDb::discover();
        assert_eq!(db.len(), db2.len());
    }
}
