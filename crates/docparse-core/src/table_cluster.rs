//! Cluster-based borderless-table recognition — the highest-coverage detector.
//!
//! A streaming state machine grows a [`RecognitionArea`]: it first locks a
//! header band (a row of aligned tokens), then attracts body tokens below it
//! into single-row clusters until the table ends (big vertical gap / page
//! change / a token that overflows the area). The recognizer then numbers rows
//! and columns, assigns each body cluster to the header column that contains
//! it, builds a grid, and validates row separation.
//!
//! Algorithm referenced from veraPDF-wcag-algs `ClusterTableConsumer` /
//! `TableRecognitionArea` / `TableRecognizer` / `TableUtils` / `Table` and
//! `ChunksMergeUtils`; **independently reimplemented** (Apache-2.0, no GPL code,
//! no StaticContainers global state). See
//! `docs/plans/cluster-table-recognizer-rust.md` for the verbatim spec.
//!
//! Scope = **P1a**: the clean-table path. Each body cell must be x-contained by
//! exactly one header column, else the area is rejected. The two "attraction"
//! stages that rescue ragged tables (`merge_weak_clusters`,
//! `merge_clusters_by_min_gaps`) are stubbed (`// TODO P1b`) — until they land
//! this detector only fires on tables whose cells sit under their headers,
//! which keeps false positives near zero.

use crate::ir::{BBox, Cell, Table, TextChunk};

// ---- constants (TableUtils) ----------------------------------------------
const WIDTH_TOLERANCE: f32 = 0.33;
const ONE_LINE_TOLERANCE: f32 = 0.9;
const NEXT_LINE_TOLERANCE: f32 = 1.05;
const NEXT_LINE_MAX_TOLERANCE: f32 = 1.5;
const TABLE_GAP: f32 = 3.0;
const NEXT_TOKEN_LENGTH: f32 = 1.2;
const MERGE_PROB: f32 = 0.75;
const HEADERS_PROB: f32 = 0.75;
const TABLE_PROB: f32 = 0.75;
const ROW_WIDTH: f32 = 1.2;

/// Our baseline approximation: the bottom of the glyph box (`bbox.y0`). Good
/// enough for relative comparisons since we use it uniformly. TODO: subtract a
/// descender fraction if a font has deep descenders (would only shift all
/// baselines equally, so unlikely to matter).
fn base_of(c: &TextChunk) -> f32 {
    c.bbox.y0
}

// ---- geometry helpers (a chunk/cluster reduced to its x-span + font) ------
#[derive(Clone, Copy)]
struct XSpan {
    x0: f32,
    x1: f32,
    font: f32,
}
fn tol(a: XSpan, b: XSpan) -> f32 {
    WIDTH_TOLERANCE * a.font.min(b.font)
}

/// `b` fits inside `a` (within tolerance). veraPDF `TableUtils.isContaining`.
fn is_containing(a: XSpan, b: XSpan) -> bool {
    let t = tol(a, b);
    b.x0 + t > a.x0 && b.x1 < a.x1 + t
}

// ---- probability primitives (ChunksMergeUtils) ---------------------------
/// Flat-top trapezoid: 1 inside `[lo,hi]`, linear ramp down over `width`, 0
/// beyond. veraPDF `ChunksMergeUtils.getUniformProbability`.
fn uniform_prob(lo: f32, hi: f32, x: f32, width: f32) -> f32 {
    const EPS: f32 = 1e-6;
    if x >= lo - EPS && x <= hi + EPS {
        return 1.0;
    }
    if x < lo - width - EPS || x > hi + width + EPS {
        return 0.0;
    }
    let dev = if x < lo + EPS { lo - x } else { x - hi };
    (width - dev) / width
}

/// Probability two chunks belong to the same text line (the `isTable=true`
/// path of veraPDF `ChunksMergeUtils.toLineMergeProbability`): a char-spacing
/// term × a baseline/font-similarity term.
fn line_merge_prob(a: &TextChunk, b: &TextChunk) -> f32 {
    let maxf = a.font_size.max(b.font_size).max(1.0);
    // char spacing: whitespace-trimmed inner edges, normalized by font.
    let end = a.bbox.x1 - trailing_ws(&a.text) as f32 * 0.25 * a.font_size;
    let start = b.bbox.x0 + leading_ws(&b.text) as f32 * 0.25 * b.font_size;
    let dist = (start - end).abs() / maxf;
    let spacing = uniform_prob(0.0, 0.67, dist, 0.33);
    // baseline + font deviation (normal_line_prob with veraPDF's table params).
    let d_base = (base_of(a) - base_of(b)).abs() / maxf;
    let d_font = (a.font_size - b.font_size).abs() / maxf;
    let normal = (1.0 - 2.0 * d_base - 0.033 * d_font).max(0.0);
    spacing * normal
}
fn leading_ws(s: &str) -> usize {
    s.chars().take_while(|c| c.is_whitespace()).count()
}
fn trailing_ws(s: &str) -> usize {
    s.chars().rev().take_while(|c| c.is_whitespace()).count()
}

// ---- token-line and cluster ----------------------------------------------
/// One text line (one or more chunks sharing a baseline) within a cluster.
struct Line<'a> {
    chunks: Vec<&'a TextChunk>,
}
impl<'a> Line<'a> {
    fn new(c: &'a TextChunk) -> Self {
        Line { chunks: vec![c] }
    }
    fn base(&self) -> f32 {
        self.chunks.iter().map(|c| base_of(c)).fold(f32::MAX, f32::min)
    }
}

/// A growing column/header/body fragment: lines stacked top→bottom.
struct Cluster<'a> {
    lines: Vec<Line<'a>>,
    /// Recognizer phase: index of the header column this cluster belongs to.
    header: Option<usize>,
    col: Option<i32>,
    row: i32,
}
impl<'a> Cluster<'a> {
    fn single(c: &'a TextChunk) -> Self {
        Cluster { lines: vec![Line::new(c)], header: None, col: None, row: 0 }
    }
    fn all(&self) -> Vec<&'a TextChunk> {
        self.lines.iter().flat_map(|l| l.chunks.iter().copied()).collect()
    }
    fn x0(&self) -> f32 {
        self.all().iter().map(|c| c.bbox.x0).fold(f32::MAX, f32::min)
    }
    fn x1(&self) -> f32 {
        self.all().iter().map(|c| c.bbox.x1).fold(f32::MIN, f32::max)
    }
    fn y0(&self) -> f32 {
        self.all().iter().map(|c| c.bbox.y0).fold(f32::MAX, f32::min)
    }
    fn y1(&self) -> f32 {
        self.all().iter().map(|c| c.bbox.y1).fold(f32::MIN, f32::max)
    }
    fn font(&self) -> f32 {
        self.all().iter().map(|c| c.font_size).fold(0.0, f32::max)
    }
    fn span(&self) -> XSpan {
        XSpan { x0: self.x0(), x1: self.x1(), font: self.font() }
    }
    /// Lowest baseline (bottom line). veraPDF `getBaseLine`.
    fn base_line(&self) -> f32 {
        self.lines.iter().map(|l| l.base()).fold(f32::MAX, f32::min)
    }
    /// First (top) line's baseline. veraPDF `getFirstBaseLine`.
    fn first_base_line(&self) -> f32 {
        self.lines.first().map(|l| l.base()).unwrap_or(0.0)
    }
    fn last_chunk(&self) -> &'a TextChunk {
        self.lines.last().unwrap().chunks.last().unwrap()
    }
    fn push_same_line(&mut self, c: &'a TextChunk) {
        self.lines.last_mut().unwrap().chunks.push(c);
    }
    fn push_new_line(&mut self, c: &'a TextChunk) {
        self.lines.push(Line::new(c));
    }
}

// ---- recognition area (streaming state machine) --------------------------
struct RecognitionArea<'a> {
    page: usize,
    headers: Vec<Cluster<'a>>,
    clusters: Vec<Cluster<'a>>,
    bbox: Option<BBox>,
    base_line: f32,
    headers_base_line: f32,
    has_complete_headers: bool,
    is_complete: bool,
    is_valid: bool,
    adaptive_next_line_tol: f32,
}

impl<'a> RecognitionArea<'a> {
    fn new(page: usize) -> Self {
        RecognitionArea {
            page,
            headers: Vec::new(),
            clusters: Vec::new(),
            bbox: None,
            base_line: f32::MAX,
            headers_base_line: f32::MAX,
            has_complete_headers: false,
            is_complete: false,
            is_valid: false,
            adaptive_next_line_tol: NEXT_LINE_TOLERANCE,
        }
    }

    fn union(&mut self, c: &TextChunk) {
        self.bbox = Some(match self.bbox {
            None => c.bbox,
            Some(b) => BBox {
                x0: b.x0.min(c.bbox.x0),
                y0: b.y0.min(c.bbox.y0),
                x1: b.x1.max(c.bbox.x1),
                y1: b.y1.max(c.bbox.y1),
            },
        });
        self.base_line = self.base_line.min(base_of(c));
    }

    fn add_token(&mut self, c: &'a TextChunk) {
        if c.page != self.page {
            self.is_complete = true;
            return;
        }
        if !self.has_complete_headers {
            if self.belongs_to_headers_area(c) {
                self.expand_headers(c);
            } else {
                self.headers_base_line = self.base_line;
                if self.check_headers() {
                    self.has_complete_headers = true;
                    self.add_cluster(c);
                } else {
                    self.is_complete = true;
                }
            }
        } else {
            self.add_cluster(c);
        }
    }

    fn belongs_to_headers_area(&self, c: &TextChunk) -> bool {
        if self.headers.is_empty() {
            return true;
        }
        if self.base_line - base_of(c) > self.adaptive_next_line_tol * c.font_size {
            return false;
        }
        let top_y = self.bbox.map(|b| b.y1).unwrap_or(c.bbox.y1);
        if c.bbox.y0 > top_y + TABLE_GAP * c.font_size {
            return false;
        }
        true
    }

    /// Grow the header band: the first existing header the token extends becomes
    /// "current"; any further headers the token bridges merge into it.
    fn expand_headers(&mut self, c: &'a TextChunk) {
        if self.headers.is_empty() {
            self.headers.push(Cluster::single(c));
            self.union(c);
            return;
        }
        let mut current: Option<usize> = None;
        let mut absorbed: Vec<usize> = Vec::new();
        for i in 0..self.headers.len() {
            match current {
                None => {
                    if self.expand_header(i, c) {
                        current = Some(i);
                    }
                }
                Some(cur) => {
                    // join: token bridges header `i` into the current column.
                    let h = self.headers[i].span();
                    if h.x0 < c.bbox.x1 && c.bbox.x0 < h.x1 {
                        let moved: Vec<Line> = self.headers[i].lines.drain(..).collect();
                        self.headers[cur].lines.extend(moved);
                        absorbed.push(i);
                    }
                }
            }
        }
        match current {
            None => {
                self.headers.push(Cluster::single(c));
                self.union(c);
            }
            Some(_) => {
                self.union(c);
                for &i in absorbed.iter().rev() {
                    self.headers.remove(i);
                }
            }
        }
    }

    /// Try to extend header `i` with the token, same-line or next-line.
    /// veraPDF `TableRecognitionArea.expandHeader` (incl. adaptive row pitch).
    fn expand_header(&mut self, i: usize, c: &'a TextChunk) -> bool {
        let (h_base, h_first, hx0, hx1) = {
            let h = &self.headers[i];
            (h.base_line(), h.first_base_line(), h.x0(), h.x1())
        };
        let delta = (h_base - base_of(c)).abs().min((h_first - base_of(c)).abs());
        if delta < ONE_LINE_TOLERANCE * c.font_size {
            let last = self.headers[i].last_chunk();
            if line_merge_prob(last, c) > MERGE_PROB {
                self.headers[i].push_same_line(c);
                self.base_line = self.base_line.min(base_of(c));
                return true;
            }
        }
        if hx0 < c.bbox.x1 && c.bbox.x0 < hx1 {
            let lsf = delta / c.font_size;
            if lsf < NEXT_LINE_MAX_TOLERANCE {
                if self.adaptive_next_line_tol < lsf {
                    self.adaptive_next_line_tol = lsf * NEXT_LINE_TOLERANCE;
                }
                self.headers[i].push_new_line(c);
                self.base_line = self.base_line.min(base_of(c));
                return true;
            }
        }
        false
    }

    /// Accept the header band only if ≥2 columns are vertically consistent.
    /// veraPDF `TableRecognitionArea.checkHeaders`.
    fn check_headers(&self) -> bool {
        let n = self.headers.len();
        if n < 2 {
            return false;
        }
        let firsts: Vec<f32> = self.headers.iter().map(|h| h.first_base_line()).collect();
        let lasts: Vec<f32> = self.headers.iter().map(|h| h.base_line()).collect();
        let centers: Vec<f32> = firsts.iter().zip(&lasts).map(|(a, b)| (a + b) / 2.0).collect();
        let avg = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        let (af, al, ac) = (avg(&firsts), avg(&lasts), avg(&centers));
        let mut max_top = 0.0f32;
        let mut max_bot = 0.0f32;
        let mut max_cen = 0.0f32;
        for (k, h) in self.headers.iter().enumerate() {
            let f = h.font().max(1.0);
            max_top = max_top.max((af - firsts[k]).abs() / f);
            max_bot = max_bot.max((al - lasts[k]).abs() / f);
            max_cen = max_cen.max((ac - centers[k]).abs() / f);
        }
        1.0 - max_top.min(max_bot).min(max_cen) > HEADERS_PROB
    }

    /// Attract a body token as a single-row cluster, or close the area.
    /// veraPDF `TableRecognitionArea.addCluster`.
    fn add_cluster(&mut self, c: &'a TextChunk) {
        if c.page != self.page {
            self.is_complete = true;
            return;
        }
        if self.base_line - base_of(c) > TABLE_GAP * c.font_size
            || self.headers_base_line < base_of(c)
        {
            self.is_complete = true;
            return;
        }
        if let Some(b) = self.bbox {
            let overflow = (b.x0 - c.bbox.x0).min(c.bbox.x1 - b.x1);
            if overflow > NEXT_TOKEN_LENGTH * c.font_size {
                self.is_complete = true;
                return;
            }
        }
        self.clusters.push(Cluster::single(c));
        self.union(c);
        self.is_valid = true;
    }
}

// ---- recognizer (numbering → columns → grid → validate) ------------------
fn cmp(a: f32, b: f32) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

/// Turn a finished, valid area into a [`Table`], or `None` if it isn't a clean
/// grid. P1a: every body cluster must be contained by exactly one header.
fn recognize(mut headers: Vec<Cluster>, mut clusters: Vec<Cluster>, page: usize) -> Option<Table> {
    if headers.len() < 2 || clusters.is_empty() {
        return None;
    }
    // setup_col_numbers: headers left→right.
    headers.sort_by(|a, b| cmp(a.x0(), b.x0()));
    for (i, h) in headers.iter_mut().enumerate() {
        h.col = Some(i as i32);
    }
    // setup_row_numbers: clusters top→bottom (first_base_line descending).
    clusters.sort_by(|a, b| cmp(b.first_base_line(), a.first_base_line()));
    let num_rows = setup_row_numbers(&mut clusters);
    if num_rows < 2 {
        return None;
    }

    // calculate_initial_columns: each body cluster → the header that contains it.
    // P1a bails on any cluster without a unique container (that's a ragged table
    // needing the P1b attraction stages). TODO P1b: merge_weak_clusters +
    // merge_clusters_by_min_gaps to rescue these instead of bailing.
    let ncols = headers.len();
    for cl in &mut clusters {
        let cs = cl.span();
        let mut hit = None;
        for (hi, h) in headers.iter().enumerate() {
            if is_containing(h.span(), cs) {
                if hit.is_some() {
                    return None; // contained by >1 header → ambiguous
                }
                hit = Some(hi);
            }
        }
        cl.header = Some(hit?); // none → bail (clean-table only)
        cl.col = headers[hit.unwrap()].col;
    }

    // build grid: row 0 = header band, rows 1.. = body by row number.
    let mut grid: Vec<Vec<Vec<&TextChunk>>> = vec![vec![Vec::new(); ncols]; num_rows as usize];
    let mut row_base = vec![f32::NAN; num_rows as usize];
    for (col, h) in headers.iter().enumerate() {
        grid[0][col] = h.all();
        row_base[0] = nan_min(row_base[0], h.base_line());
    }
    for cl in &clusters {
        let r = cl.row as usize;
        let col = cl.col.unwrap() as usize;
        grid[r][col].extend(cl.all());
        row_base[r] = nan_min(row_base[r], cl.base_line());
    }

    let table_font = clusters
        .iter()
        .map(|c| c.font())
        .chain(headers.iter().map(|h| h.font()))
        .fold(0.0, f32::max)
        .max(1.0);

    // validate: rows must be vertically separated (no overlapping baselines).
    if validation_score(&grid, &row_base, table_font) < TABLE_PROB {
        return None;
    }
    // every body row needs ≥2 filled cells (a real grid, not a stray pair).
    for grow in grid.iter().skip(1) {
        let filled = grow.iter().filter(|c| !c.is_empty()).count();
        if filled < 2 {
            return None;
        }
    }

    let rows: Vec<Vec<Cell>> = grid
        .iter()
        .map(|row| row.iter().map(|cs| make_cell(cs)).collect())
        .collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for h in &headers {
        x0 = x0.min(h.x0());
        y0 = y0.min(h.y0());
        x1 = x1.max(h.x1());
        y1 = y1.max(h.y1());
    }
    for c in &clusters {
        x0 = x0.min(c.x0());
        y0 = y0.min(c.y0());
        x1 = x1.max(c.x1());
        y1 = y1.max(c.y1());
    }
    Some(Table { bbox: BBox { x0, y0, x1, y1 }, page, rows })
}

fn nan_min(a: f32, b: f32) -> f32 {
    if a.is_nan() {
        b
    } else {
        a.min(b)
    }
}

/// Assign `row` numbers to baseline-sorted clusters; returns `num_rows`
/// (header is row 0, body rows are 1..num_rows). veraPDF `setupRowNumbers`.
fn setup_row_numbers(clusters: &mut [Cluster]) -> i32 {
    if clusters.is_empty() {
        return 0;
    }
    let mut row = 1;
    let mut anchor_base = clusters[0].base_line();
    clusters[0].row = 1;
    for cl in clusters.iter_mut().skip(1) {
        let tol = cl.lines[0].chunks[0].font_size * ONE_LINE_TOLERANCE;
        let (cb, cf) = (cl.base_line(), cl.first_base_line());
        if anchor_base > cf + tol {
            row += 1;
            anchor_base = cb;
        } else if anchor_base > cb + tol {
            anchor_base = cb;
        }
        cl.row = row;
    }
    row + 1
}

/// Row-separation score. 0 if the grid is degenerate or any body cell's
/// baseline overlaps the previous row's. veraPDF `Table.validate`.
fn validation_score(grid: &[Vec<Vec<&TextChunk>>], row_base: &[f32], font: f32) -> f32 {
    let nrows = grid.len();
    let ncols = grid.first().map(|r| r.len()).unwrap_or(0);
    let filled: usize = grid.iter().flatten().filter(|c| !c.is_empty()).count();
    if nrows < 2 || ncols < 2 || (nrows == 2 && ncols == 2 && filled < 4) {
        return 0.0;
    }
    let mut max_int = 0.0f32;
    for r in 1..nrows {
        let prev = row_base[r - 1];
        if prev.is_nan() {
            continue;
        }
        for cell in &grid[r] {
            if cell.is_empty() {
                continue;
            }
            let cell_base = cell.iter().map(|c| base_of(c)).fold(f32::MAX, f32::min);
            let inter = 1.0 - (prev - cell_base) / (font * ROW_WIDTH);
            max_int = max_int.max(inter);
        }
    }
    (1.0 - max_int).max(0.0)
}

fn make_cell(chunks: &[&TextChunk]) -> Cell {
    let text = crate::layout::reconstruct_lines(chunks)
        .iter()
        .map(|l| l.text.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in chunks {
        x0 = x0.min(c.bbox.x0);
        y0 = y0.min(c.bbox.y0);
        x1 = x1.max(c.bbox.x1);
        y1 = y1.max(c.bbox.y1);
    }
    if chunks.is_empty() {
        x0 = 0.0;
        y0 = 0.0;
        x1 = 0.0;
        y1 = 0.0;
    }
    Cell { text, bbox: BBox { x0, y0, x1, y1 } }
}

// ---- driver --------------------------------------------------------------
/// Detect cluster tables on one page. Feeds chunks in reading order through the
/// state machine; on each area close, recognizes a table and re-feeds the token
/// that broke it. Skips chunks already inside an excluded (bordered/ruled) table.
pub fn detect_cluster_tables(chunks: &[&TextChunk], exclude: &[BBox], page: usize) -> Vec<Table> {
    let kept: Vec<&TextChunk> = chunks
        .iter()
        .copied()
        .filter(|c| !exclude.iter().any(|b| center_in(c, b)))
        .collect();
    if kept.is_empty() {
        return Vec::new();
    }
    let ordered = scan_order(&kept);

    let mut tables = Vec::new();
    let mut area = RecognitionArea::new(ordered[0].page);
    let mut idx = 0;
    while idx < ordered.len() {
        let c = ordered[idx];
        area.add_token(c);
        if area.is_complete {
            flush(&mut area, page, &mut tables);
            area = RecognitionArea::new(c.page);
            continue; // re-feed the breaking token into the fresh area
        }
        idx += 1;
    }
    flush(&mut area, page, &mut tables);
    tables
}

fn flush(area: &mut RecognitionArea, page: usize, out: &mut Vec<Table>) {
    if area.is_valid {
        let headers = std::mem::take(&mut area.headers);
        let clusters = std::mem::take(&mut area.clusters);
        if let Some(t) = recognize(headers, clusters, page) {
            out.push(t);
        }
    }
}

/// Feed order for the state machine: row-by-row top→bottom, left→right within a
/// row. This is what veraPDF gets from content/tag order — and crucially NOT
/// XY-cut reading order, which slices a table into columns (feeding a whole
/// column before the next), destroying the header-band assumption.
fn scan_order<'a>(chunks: &[&'a TextChunk]) -> Vec<&'a TextChunk> {
    let mut v: Vec<&TextChunk> = chunks.to_vec();
    v.sort_by(|a, b| cmp(b.bbox.y0, a.bbox.y0)); // top first (y descending)
    let mut out: Vec<&TextChunk> = Vec::with_capacity(v.len());
    let mut band: Vec<&TextChunk> = Vec::new();
    let mut band_y = f32::NAN;
    for c in v {
        let tol = c.font_size.max(1.0) * 0.5;
        if band_y.is_nan() || (band_y - c.bbox.y0).abs() <= tol {
            if band_y.is_nan() {
                band_y = c.bbox.y0;
            }
            band.push(c);
        } else {
            band.sort_by(|a, b| cmp(a.bbox.x0, b.bbox.x0));
            out.append(&mut band);
            band_y = c.bbox.y0;
            band.push(c);
        }
    }
    band.sort_by(|a, b| cmp(a.bbox.x0, b.bbox.x0));
    out.append(&mut band);
    out
}

fn center_in(c: &TextChunk, b: &BBox) -> bool {
    let cx = (c.bbox.x0 + c.bbox.x1) / 2.0;
    let cy = c.bbox.cy();
    cx >= b.x0 && cx <= b.x1 && cy >= b.y0 && cy <= b.y1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str, x0: f32, x1: f32, cy: f32) -> TextChunk {
        TextChunk {
            text: text.into(),
            bbox: BBox { x0, y0: cy - 5.0, x1, y1: cy + 5.0 },
            font_size: 10.0,
            font: None,
            page: 1,
            confidence: 1.0,
            bold: false,
        }
    }

    #[test]
    fn is_containing_basic() {
        let wide = XSpan { x0: 0.0, x1: 100.0, font: 10.0 };
        let narrow = XSpan { x0: 20.0, x1: 40.0, font: 10.0 };
        assert!(is_containing(wide, narrow));
        assert!(!is_containing(narrow, wide));
    }

    #[test]
    fn uniform_prob_ramp() {
        assert_eq!(uniform_prob(0.0, 0.67, 0.3, 0.33), 1.0); // inside plateau
        assert_eq!(uniform_prob(0.0, 0.67, 2.0, 0.33), 0.0); // far outside
        let mid = uniform_prob(0.0, 0.67, 0.67 + 0.165, 0.33); // halfway down ramp
        assert!((mid - 0.5).abs() < 0.05);
    }

    #[test]
    fn clean_numeric_table_detected() {
        // Header band: two wide headers spanning their columns; body numbers
        // sit under them. 3 body rows × 2 cols.
        let cs: Vec<TextChunk> = vec![
            chunk("Method", 10.0, 70.0, 200.0),
            chunk("Score", 110.0, 160.0, 200.0),
            chunk("alpha", 20.0, 55.0, 180.0),
            chunk("0.91", 120.0, 150.0, 180.0),
            chunk("beta", 22.0, 52.0, 165.0),
            chunk("0.85", 121.0, 149.0, 165.0),
            chunk("gamma", 18.0, 58.0, 150.0),
            chunk("0.78", 119.0, 151.0, 150.0),
        ];
        let refs: Vec<&TextChunk> = cs.iter().collect();
        let tables = detect_cluster_tables(&refs, &[], 1);
        assert_eq!(tables.len(), 1, "clean 2-col table detected");
        let t = &tables[0];
        assert_eq!(t.rows[0].len(), 2);
        assert_eq!(t.rows[0][0].text, "Method");
        assert_eq!(t.rows[0][1].text, "Score");
        // 1 header row + 3 body rows.
        assert_eq!(t.rows.len(), 4);
        assert_eq!(t.rows[1][0].text, "alpha");
        assert_eq!(t.rows[3][1].text, "0.78");
    }

    #[test]
    fn prose_is_not_a_table() {
        // Ordinary left-aligned paragraph lines: one wide run per line, no
        // second aligned column → no header band → no table.
        let cs: Vec<TextChunk> = vec![
            chunk("a line of ordinary prose text here", 10.0, 240.0, 200.0),
            chunk("another ordinary prose line follows", 10.0, 245.0, 185.0),
            chunk("and a third running line of body", 10.0, 235.0, 170.0),
            chunk("with a fourth to be sure of it", 10.0, 230.0, 155.0),
        ];
        let refs: Vec<&TextChunk> = cs.iter().collect();
        assert!(detect_cluster_tables(&refs, &[], 1).is_empty());
    }

    #[test]
    fn excluded_region_skipped() {
        let cs: Vec<TextChunk> = vec![
            chunk("Method", 10.0, 70.0, 200.0),
            chunk("Score", 110.0, 160.0, 200.0),
            chunk("alpha", 20.0, 55.0, 180.0),
            chunk("0.91", 120.0, 150.0, 180.0),
            chunk("beta", 22.0, 52.0, 165.0),
            chunk("0.85", 121.0, 149.0, 165.0),
        ];
        let refs: Vec<&TextChunk> = cs.iter().collect();
        let excl = [BBox { x0: 0.0, y0: 140.0, x1: 200.0, y1: 210.0 }];
        assert!(detect_cluster_tables(&refs, &excl, 1).is_empty());
    }
}
