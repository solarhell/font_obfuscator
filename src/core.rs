use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rand::seq::SliceRandom;
use read_fonts::tables::cmap::Cmap;
use read_fonts::tables::glyf::{Glyf, Glyph as ReadGlyph};
use read_fonts::tables::loca::Loca;
use read_fonts::types::GlyphId;
use read_fonts::{FontRef, TableProvider};
use write_fonts::tables::cmap::Cmap as WriteCmap;
use write_fonts::tables::glyf::{
    Contour, Glyph as WriteGlyph, GlyfLocaBuilder, SimpleGlyph as WriteSimpleGlyph,
};
use write_fonts::tables::head::Head as WriteHead;
use write_fonts::tables::hhea::Hhea as WriteHhea;
use write_fonts::tables::hmtx::{Hmtx as WriteHmtx, LongMetric};
use write_fonts::tables::loca::LocaFormat;
use write_fonts::tables::maxp::Maxp as WriteMaxp;
use write_fonts::tables::name::Name as WriteName;
use write_fonts::tables::os2::Os2 as WriteOs2;
use write_fonts::tables::post::Post as WritePost;
use write_fonts::FontBuilder;

use crate::config::FontConfig;
use crate::utils::{deduplicate_str, str_has_emoji, str_has_whitespace};

#[derive(thiserror::Error, Debug)]
pub enum ObfuscateError {
    #[error("明文或阴书不允许含有空格")]
    HasWhitespace,
    #[error("明文或阴书不允许含有emoji")]
    HasEmoji,
    #[error("没有意义的混淆")]
    SameText,
    #[error("阴书的有效长度需与明文一致")]
    LengthMismatch,
    #[error("字库缺少'{0}'这个字")]
    MissingChar(char),
    #[error("字体读取错误: {0}")]
    FontRead(String),
    #[error("字体构建错误: {0}")]
    FontBuild(String),
    #[error("IO错误: {0}")]
    Io(#[from] std::io::Error),
}

/// Look up a Unicode codepoint in the cmap table, returning the GlyphId
fn cmap_lookup(cmap: &Cmap, codepoint: u32) -> Option<GlyphId> {
    cmap.encoding_records().iter().find_map(|record| {
        let subtable = record.subtable(cmap.offset_data()).ok()?;
        subtable.map_codepoint(codepoint)
    })
}

/// Convert a read-fonts SimpleGlyph to a write-fonts SimpleGlyph
fn convert_simple_glyph(g: &read_fonts::tables::glyf::SimpleGlyph) -> WriteSimpleGlyph {
    let mut contours = Vec::new();
    let end_pts: Vec<u16> = g.end_pts_of_contours().iter().map(|v| v.get()).collect();
    let points: Vec<read_fonts::tables::glyf::CurvePoint> = g.points().collect();
    let mut start = 0usize;
    for &end in &end_pts {
        let end_idx = end as usize + 1;
        let contour_points: Vec<read_fonts::tables::glyf::CurvePoint> =
            points[start..end_idx].to_vec();
        contours.push(Contour::from(contour_points));
        start = end_idx;
    }
    let instructions = g.instructions().to_vec();
    WriteSimpleGlyph {
        bbox: write_fonts::tables::glyf::Bbox {
            x_min: g.x_min(),
            y_min: g.y_min(),
            x_max: g.x_max(),
            y_max: g.y_max(),
        },
        contours,
        instructions,
    }
}

#[derive(Debug)]
pub struct ObfuscateResult {
    pub files: HashMap<String, PathBuf>,
}

#[derive(Debug)]
pub struct ObfuscatePlusResult {
    pub files: HashMap<String, PathBuf>,
    pub html_entities: HashMap<String, String>,
}

/// Core obfuscation: remap plain glyphs to shadow Unicode codepoints
pub fn obfuscate(
    plain_text: &str,
    shadow_text: &str,
    font_data: &[u8],
    font_config: &FontConfig,
    output_dir: &Path,
    filename: &str,
    only_ttf: bool,
) -> Result<ObfuscateResult, ObfuscateError> {
    if str_has_whitespace(plain_text) || str_has_whitespace(shadow_text) {
        return Err(ObfuscateError::HasWhitespace);
    }
    if str_has_emoji(plain_text) || str_has_emoji(shadow_text) {
        return Err(ObfuscateError::HasEmoji);
    }

    let plain_text = deduplicate_str(plain_text);
    let shadow_text = deduplicate_str(shadow_text);

    if plain_text == shadow_text {
        return Err(ObfuscateError::SameText);
    }

    let plain_chars: Vec<char> = plain_text.chars().collect();
    let shadow_chars: Vec<char> = shadow_text.chars().collect();

    if plain_chars.len() != shadow_chars.len() {
        return Err(ObfuscateError::LengthMismatch);
    }

    let font = FontRef::new(font_data).map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let cmap_table = font
        .cmap()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let head = font
        .head()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let hhea = font
        .hhea()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let hmtx = font
        .hmtx()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let loca = font
        .loca(if head.index_to_loc_format() == 1 { Some(true) } else { Some(false) })
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let glyf_table = font
        .glyf()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;

    // Verify all chars exist in cmap
    for &c in &plain_chars {
        if cmap_lookup(&cmap_table, c as u32).is_none() {
            return Err(ObfuscateError::MissingChar(c));
        }
    }
    for &c in &shadow_chars {
        if cmap_lookup(&cmap_table, c as u32).is_none() {
            return Err(ObfuscateError::MissingChar(c));
        }
    }

    // Build glyph data: .notdef + mapped glyphs
    let mut glyph_entries: Vec<(Option<WriteGlyph>, u16, i16)> = Vec::new();
    let mut cmap_mappings: Vec<(char, GlyphId)> = Vec::new();

    // .notdef glyph (glyph index 0)
    let notdef_id = GlyphId::new(0);
    let notdef_glyph = read_glyph_from_table(&glyf_table, &loca, notdef_id);
    let (notdef_aw, notdef_lsb) =
        read_hmtx_entry(&hmtx, notdef_id, hhea.number_of_h_metrics());
    glyph_entries.push((notdef_glyph, notdef_aw, notdef_lsb));

    for (i, (&plain_c, &shadow_c)) in plain_chars.iter().zip(shadow_chars.iter()).enumerate() {
        let plain_gid = cmap_lookup(&cmap_table, plain_c as u32).unwrap();
        let glyph = read_glyph_from_table(&glyf_table, &loca, plain_gid);
        let (aw, lsb) = read_hmtx_entry(&hmtx, plain_gid, hhea.number_of_h_metrics());
        glyph_entries.push((glyph, aw, lsb));
        cmap_mappings.push((shadow_c, GlyphId::new((i as u32) + 1)));
    }

    let ttf_bytes = build_font(
        &glyph_entries,
        &cmap_mappings,
        head.units_per_em(),
        hhea.ascender().to_i16(),
        hhea.descender().to_i16(),
        font_config,
    )?;

    std::fs::create_dir_all(output_dir)?;
    let ttf_path = output_dir.join(format!("{filename}.ttf"));
    std::fs::write(&ttf_path, &ttf_bytes)?;

    let mut files = HashMap::new();
    files.insert("ttf".into(), ttf_path);

    if !only_ttf {
        let woff2_path = output_dir.join(format!("{filename}.woff2"));
        convert_ttf_to_woff2(&ttf_bytes, &woff2_path)?;
        files.insert("woff2".into(), woff2_path);
    }

    Ok(ObfuscateResult { files })
}

/// Enhanced obfuscation using Private Use Area Unicode codepoints
pub fn obfuscate_plus(
    plain_text: &str,
    font_data: &[u8],
    font_config: &FontConfig,
    output_dir: &Path,
    filename: &str,
    only_ttf: bool,
) -> Result<ObfuscatePlusResult, ObfuscateError> {
    if str_has_whitespace(plain_text) {
        return Err(ObfuscateError::HasWhitespace);
    }
    if str_has_emoji(plain_text) {
        return Err(ObfuscateError::HasEmoji);
    }

    let plain_text = deduplicate_str(plain_text);
    let plain_chars: Vec<char> = plain_text.chars().collect();

    let font = FontRef::new(font_data).map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let cmap_table = font
        .cmap()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let head = font
        .head()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let hhea = font
        .hhea()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let hmtx = font
        .hmtx()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let loca = font
        .loca(if head.index_to_loc_format() == 1 { Some(true) } else { Some(false) })
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let glyf_table = font
        .glyf()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;

    for &c in &plain_chars {
        if cmap_lookup(&cmap_table, c as u32).is_none() {
            return Err(ObfuscateError::MissingChar(c));
        }
    }

    // Sample random Private Use Area codepoints
    let mut rng = rand::rng();
    let mut private_pool: Vec<u32> = (0xE000..=0xF8FF).collect();
    private_pool.shuffle(&mut rng);
    let private_codes: Vec<u32> = private_pool[..plain_chars.len()].to_vec();

    let mut glyph_entries: Vec<(Option<WriteGlyph>, u16, i16)> = Vec::new();
    let mut cmap_mappings: Vec<(char, GlyphId)> = Vec::new();
    let mut html_entities = HashMap::new();

    // .notdef
    let notdef_id = GlyphId::new(0);
    let notdef_glyph = read_glyph_from_table(&glyf_table, &loca, notdef_id);
    let (notdef_aw, notdef_lsb) =
        read_hmtx_entry(&hmtx, notdef_id, hhea.number_of_h_metrics());
    glyph_entries.push((notdef_glyph, notdef_aw, notdef_lsb));

    for (i, &plain_c) in plain_chars.iter().enumerate() {
        let plain_gid = cmap_lookup(&cmap_table, plain_c as u32).unwrap();
        let glyph = read_glyph_from_table(&glyf_table, &loca, plain_gid);
        let (aw, lsb) = read_hmtx_entry(&hmtx, plain_gid, hhea.number_of_h_metrics());
        glyph_entries.push((glyph, aw, lsb));

        let private_cp = private_codes[i];
        let private_char = char::from_u32(private_cp).unwrap();
        cmap_mappings.push((private_char, GlyphId::new((i as u32) + 1)));
        html_entities.insert(plain_c.to_string(), format!("&#x{:x}", private_cp));
    }

    let ttf_bytes = build_font(
        &glyph_entries,
        &cmap_mappings,
        head.units_per_em(),
        hhea.ascender().to_i16(),
        hhea.descender().to_i16(),
        font_config,
    )?;

    std::fs::create_dir_all(output_dir)?;
    let ttf_path = output_dir.join(format!("{filename}.ttf"));
    std::fs::write(&ttf_path, &ttf_bytes)?;

    let mut files = HashMap::new();
    files.insert("ttf".into(), ttf_path);

    if !only_ttf {
        let woff2_path = output_dir.join(format!("{filename}.woff2"));
        convert_ttf_to_woff2(&ttf_bytes, &woff2_path)?;
        files.insert("woff2".into(), woff2_path);
    }

    Ok(ObfuscatePlusResult {
        files,
        html_entities,
    })
}

/// Read a glyph from the glyf table, converting to write-fonts type
fn read_glyph_from_table(glyf: &Glyf, loca: &Loca, glyph_id: GlyphId) -> Option<WriteGlyph> {
    let glyph = loca.get_glyf(glyph_id, glyf).ok()??;
    match glyph {
        ReadGlyph::Simple(ref simple) => Some(WriteGlyph::Simple(convert_simple_glyph(simple))),
        ReadGlyph::Composite(_) => {
            // Most CJK characters are simple glyphs.
            // Composite glyphs would need glyph ID remapping.
            None
        }
    }
}

/// Read horizontal metrics for a glyph
fn read_hmtx_entry(
    hmtx: &read_fonts::tables::hmtx::Hmtx,
    glyph_id: GlyphId,
    num_long_metrics: u16,
) -> (u16, i16) {
    let gid = glyph_id.to_u32() as usize;
    let num_long = num_long_metrics as usize;
    if gid < num_long {
        let record = hmtx.h_metrics().get(gid).unwrap();
        (record.advance.get(), record.side_bearing.get())
    } else {
        let last_aw = hmtx
            .h_metrics()
            .get(num_long - 1)
            .unwrap()
            .advance
            .get();
        let lsb_idx = gid - num_long;
        let lsb = hmtx
            .left_side_bearings()
            .get(lsb_idx)
            .map(|v| v.get())
            .unwrap_or(0);
        (last_aw, lsb)
    }
}

/// Build a TrueType font from glyph data and mappings
fn build_font(
    glyph_entries: &[(Option<WriteGlyph>, u16, i16)],
    cmap_mappings: &[(char, GlyphId)],
    units_per_em: u16,
    ascender: i16,
    descender: i16,
    font_config: &FontConfig,
) -> Result<Vec<u8>, ObfuscateError> {
    let num_glyphs = glyph_entries.len() as u16;

    // Build glyf + loca
    let mut glyf_builder = GlyfLocaBuilder::new();
    for (glyph_opt, _, _) in glyph_entries {
        match glyph_opt {
            Some(g) => {
                glyf_builder
                    .add_glyph(g)
                    .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
            }
            None => {
                let empty = WriteSimpleGlyph::default();
                glyf_builder
                    .add_glyph(&empty)
                    .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
            }
        }
    }
    let (glyf_table, loca_table, loca_format) = glyf_builder.build();

    // Build cmap
    let cmap = WriteCmap::from_mappings(cmap_mappings.iter().map(|&(c, gid)| (c, gid)))
        .map_err(|e| ObfuscateError::FontBuild(format!("cmap: {e:?}")))?;

    // Build hmtx
    let h_metrics: Vec<LongMetric> = glyph_entries
        .iter()
        .map(|(_, aw, lsb)| LongMetric {
            advance: *aw,
            side_bearing: *lsb,
        })
        .collect();
    let hmtx = WriteHmtx::new(h_metrics, vec![]);

    // Build hhea
    let max_aw = glyph_entries
        .iter()
        .map(|(_, aw, _)| *aw)
        .max()
        .unwrap_or(0);
    let hhea = WriteHhea::new(
        ascender.into(),
        descender.into(),
        0i16.into(),
        max_aw.into(),
        0i16.into(),
        0i16.into(),
        0i16.into(),
        1,
        0,
        0,
        num_glyphs,
    );

    // Build head
    let index_to_loc_format = match loca_format {
        LocaFormat::Short => 0i16,
        LocaFormat::Long => 1i16,
    };
    let head = WriteHead::new(
        write_fonts::types::Fixed::from_f64(1.0),
        0,
        write_fonts::tables::head::Flags::from_bits(0x000B).unwrap_or_default(),
        units_per_em,
        write_fonts::types::LongDateTime::new(0),
        write_fonts::types::LongDateTime::new(0),
        0,
        0,
        0,
        0,
        write_fonts::tables::head::MacStyle::empty(),
        8,
        index_to_loc_format,
    );

    // Build maxp
    let maxp = WriteMaxp::new(num_glyphs);

    // Build name table
    let name = build_name_table(font_config);

    // Build OS/2
    let os2 = WriteOs2::default();

    // Build post
    let post = WritePost::new(
        write_fonts::types::Fixed::from_f64(0.0),
        (-100i16).into(),
        50i16.into(),
        0,
        0,
        0,
        0,
        0,
    );

    // Assemble font
    let mut builder = FontBuilder::new();
    builder
        .add_table(&head)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&hhea)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&maxp)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&os2)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&name)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&cmap)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&post)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&hmtx)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&glyf_table)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&loca_table)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;

    Ok(builder.build())
}

fn build_name_table(config: &FontConfig) -> WriteName {
    use write_fonts::tables::name::NameRecord;

    let ps_name = format!("{}-{}", config.family_name, config.style_name);
    let full_name = format!("{} {}", config.family_name, config.style_name);

    let entries: &[(u16, &str)] = &[
        (0, &config.copyright),
        (1, &config.family_name),
        (2, &config.style_name),
        (3, &ps_name),
        (4, &full_name),
        (5, &config.version),
        (6, &ps_name),
        (11, &config.vendor_url),
    ];

    let records: Vec<NameRecord> = entries
        .iter()
        .map(|(name_id, value)| {
            NameRecord::new(
                3,      // platformID: Windows
                1,      // encodingID: Unicode BMP
                0x0409, // languageID: English (US)
                (*name_id).into(),
                value.to_string().into(),
            )
        })
        .collect();

    WriteName::new(records)
}

/// Full-font obfuscation: keep all original glyphs, only swap specified pairs.
/// This preserves the complete character set of the original font.
pub fn obfuscate_full(
    plain_text: &str,
    shadow_text: &str,
    font_data: &[u8],
    output_dir: &Path,
    filename: &str,
    only_ttf: bool,
) -> Result<ObfuscateResult, ObfuscateError> {
    if str_has_whitespace(plain_text) || str_has_whitespace(shadow_text) {
        return Err(ObfuscateError::HasWhitespace);
    }
    if str_has_emoji(plain_text) || str_has_emoji(shadow_text) {
        return Err(ObfuscateError::HasEmoji);
    }

    let plain_text = deduplicate_str(plain_text);
    let shadow_text = deduplicate_str(shadow_text);

    if plain_text == shadow_text {
        return Err(ObfuscateError::SameText);
    }

    let plain_chars: Vec<char> = plain_text.chars().collect();
    let shadow_chars: Vec<char> = shadow_text.chars().collect();

    if plain_chars.len() != shadow_chars.len() {
        return Err(ObfuscateError::LengthMismatch);
    }

    let font = FontRef::new(font_data).map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let cmap_table = font
        .cmap()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let head = font
        .head()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let hhea = font
        .hhea()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let hmtx = font
        .hmtx()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let maxp = font
        .maxp()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let loca = font
        .loca(if head.index_to_loc_format() == 1 {
            Some(true)
        } else {
            Some(false)
        })
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;
    let glyf_table = font
        .glyf()
        .map_err(|e| ObfuscateError::FontRead(e.to_string()))?;

    for &c in &plain_chars {
        if cmap_lookup(&cmap_table, c as u32).is_none() {
            return Err(ObfuscateError::MissingChar(c));
        }
    }
    for &c in &shadow_chars {
        if cmap_lookup(&cmap_table, c as u32).is_none() {
            return Err(ObfuscateError::MissingChar(c));
        }
    }

    // Build swap map: shadow glyph ID -> plain glyph ID (replace shadow's glyph with plain's)
    let mut swap_map: HashMap<u32, u32> = HashMap::new();
    for (&plain_c, &shadow_c) in plain_chars.iter().zip(shadow_chars.iter()) {
        let plain_gid = cmap_lookup(&cmap_table, plain_c as u32).unwrap();
        let shadow_gid = cmap_lookup(&cmap_table, shadow_c as u32).unwrap();
        swap_map.insert(shadow_gid.to_u32(), plain_gid.to_u32());
    }

    let num_glyphs = maxp.num_glyphs() as u32;
    let num_h_metrics = hhea.number_of_h_metrics();

    // Rebuild glyf + loca with swapped glyphs
    let mut glyf_builder = GlyfLocaBuilder::new();
    let mut h_metrics: Vec<LongMetric> = Vec::with_capacity(num_glyphs as usize);

    for gid in 0..num_glyphs {
        // If this glyph should be swapped, use the source glyph instead
        let source_gid = swap_map.get(&gid).copied().unwrap_or(gid);
        let source_id = GlyphId::new(source_gid);
        let glyph = read_glyph_from_table(&glyf_table, &loca, source_id);
        match &glyph {
            Some(g) => {
                glyf_builder
                    .add_glyph(g)
                    .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
            }
            None => {
                let empty = WriteSimpleGlyph::default();
                glyf_builder
                    .add_glyph(&empty)
                    .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
            }
        }

        let (aw, lsb) = read_hmtx_entry(&hmtx, source_id, num_h_metrics);
        h_metrics.push(LongMetric {
            advance: aw,
            side_bearing: lsb,
        });
    }

    let (new_glyf, new_loca, loca_format) = glyf_builder.build();
    let new_hmtx = WriteHmtx::new(h_metrics, vec![]);

    // Build font: copy most tables from original, replace glyf/loca/hmtx
    let mut builder = FontBuilder::new();
    builder
        .add_table(&new_glyf)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&new_loca)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;
    builder
        .add_table(&new_hmtx)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;

    // Update head's index_to_loc_format
    let new_head = WriteHead::new(
        write_fonts::types::Fixed::from_f64(head.font_revision().to_f64()),
        0,
        write_fonts::tables::head::Flags::from_bits(head.flags().bits()).unwrap_or_default(),
        head.units_per_em(),
        head.created(),
        head.modified(),
        head.x_min(),
        head.y_min(),
        head.x_max(),
        head.y_max(),
        write_fonts::tables::head::MacStyle::from_bits(head.mac_style().bits()).unwrap_or_default(),
        head.lowest_rec_ppem(),
        match loca_format {
            LocaFormat::Short => 0i16,
            LocaFormat::Long => 1i16,
        },
    );
    builder
        .add_table(&new_head)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;

    // Update hhea with correct number_of_h_metrics
    let new_hhea = WriteHhea::new(
        hhea.ascender().to_i16().into(),
        hhea.descender().to_i16().into(),
        hhea.line_gap().to_i16().into(),
        hhea.advance_width_max().to_u16().into(),
        hhea.min_left_side_bearing().to_i16().into(),
        hhea.min_right_side_bearing().to_i16().into(),
        hhea.x_max_extent().to_i16().into(),
        hhea.caret_slope_rise(),
        hhea.caret_slope_run(),
        hhea.caret_offset(),
        num_glyphs as u16, // all glyphs have full metrics
    );
    builder
        .add_table(&new_hhea)
        .map_err(|e| ObfuscateError::FontBuild(e.to_string()))?;

    // Copy all other tables from the original font
    builder.copy_missing_tables(font);

    let ttf_bytes = builder.build();

    std::fs::create_dir_all(output_dir)?;
    let ttf_path = output_dir.join(format!("{filename}.ttf"));
    std::fs::write(&ttf_path, &ttf_bytes)?;

    let mut files = HashMap::new();
    files.insert("ttf".into(), ttf_path);

    if !only_ttf {
        let woff2_path = output_dir.join(format!("{filename}.woff2"));
        convert_ttf_to_woff2(&ttf_bytes, &woff2_path)?;
        files.insert("woff2".into(), woff2_path);
    }

    Ok(ObfuscateResult { files })
}

/// Convert TTF bytes to WOFF2 format
fn convert_ttf_to_woff2(ttf_bytes: &[u8], output_path: &Path) -> Result<(), ObfuscateError> {
    let woff2_bytes = ttf2woff2::encode(ttf_bytes, ttf2woff2::BrotliQuality::default())
        .map_err(|e| ObfuscateError::FontBuild(format!("woff2 encoding: {e}")))?;
    std::fs::write(output_path, woff2_bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FontConfig;

    fn load_base_font() -> Vec<u8> {
        std::fs::read("base-font/KaiGenGothicCN-Regular.ttf")
            .expect("base font must exist for tests (run from project root)")
    }

    fn default_config() -> FontConfig {
        FontConfig::default()
    }

    fn load_font(path: &str) -> Vec<u8> {
        std::fs::read(path)
            .unwrap_or_else(|e| panic!("font not found at {path}: {e}"))
    }

    // ── obfuscate validation tests ──

    #[test]
    fn obfuscate_rejects_whitespace_in_plain() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate("hello world", "abcdefghijk", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::HasWhitespace));
    }

    #[test]
    fn obfuscate_rejects_whitespace_in_shadow() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate("abcde", "a b c", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::HasWhitespace));
    }

    #[test]
    fn obfuscate_rejects_emoji() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate("hello😀", "abcdef", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::HasEmoji));
    }

    #[test]
    fn obfuscate_rejects_same_text() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate("abc", "abc", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::SameText));
    }

    #[test]
    fn obfuscate_rejects_length_mismatch() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate("abc", "de", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::LengthMismatch));
    }

    #[test]
    fn obfuscate_deduplicates_then_checks_length() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        // "aabb" dedup -> "ab", "cd" dedup -> "cd" — lengths match
        let result = obfuscate("aabb", "ccdd", &font, &default_config(), dir.path(), "t", true);
        assert!(result.is_ok());
    }

    // ── obfuscate success + font validity tests ──

    #[test]
    fn obfuscate_produces_valid_ttf() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font, &default_config(), dir.path(), "test", true,
        ).unwrap();

        let ttf_path = &result.files["ttf"];
        assert!(ttf_path.exists());

        // Parse the generated font and verify it's valid
        let generated = std::fs::read(ttf_path).unwrap();
        let gen_font = FontRef::new(&generated).expect("generated TTF should be parseable");

        // Should have required tables
        assert!(gen_font.head().is_ok());
        assert!(gen_font.hhea().is_ok());
        assert!(gen_font.maxp().is_ok());
        assert!(gen_font.cmap().is_ok());
        assert!(gen_font.glyf().is_ok());
    }

    #[test]
    fn obfuscate_cmap_maps_shadow_chars() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font, &default_config(), dir.path(), "test", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let cmap = gen_font.cmap().unwrap();

        // Shadow chars (x, y, z) should be in cmap
        assert!(cmap_lookup(&cmap, 'x' as u32).is_some());
        assert!(cmap_lookup(&cmap, 'y' as u32).is_some());
        assert!(cmap_lookup(&cmap, 'z' as u32).is_some());

        // Plain chars (a, b, c) should NOT be in cmap
        assert!(cmap_lookup(&cmap, 'a' as u32).is_none());
        assert!(cmap_lookup(&cmap, 'b' as u32).is_none());
        assert!(cmap_lookup(&cmap, 'c' as u32).is_none());
    }

    #[test]
    fn obfuscate_with_cjk_chars() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "真好", "假的", &font, &default_config(), dir.path(), "cjk", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let cmap = gen_font.cmap().unwrap();

        assert!(cmap_lookup(&cmap, '假' as u32).is_some());
        assert!(cmap_lookup(&cmap, '的' as u32).is_some());
        assert!(cmap_lookup(&cmap, '真' as u32).is_none());
        assert!(cmap_lookup(&cmap, '好' as u32).is_none());
    }

    #[test]
    fn obfuscate_with_digits() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "0123456789", "9876543210", &font, &default_config(), dir.path(), "digits", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let maxp = gen_font.maxp().unwrap();
        // .notdef + 10 digits = 11 glyphs
        assert_eq!(maxp.num_glyphs(), 11);
    }

    #[test]
    fn obfuscate_generates_woff2_when_requested() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font, &default_config(), dir.path(), "woff2test", false,
        ).unwrap();

        assert!(result.files.contains_key("ttf"));
        assert!(result.files.contains_key("woff2"));
        let woff2_path = &result.files["woff2"];
        assert!(woff2_path.exists());

        let woff2_data = std::fs::read(woff2_path).unwrap();
        // WOFF2 magic number: 'wOF2' = 0x774F4632
        assert_eq!(&woff2_data[..4], b"wOF2");
        // WOFF2 should be smaller than TTF
        let ttf_data = std::fs::read(&result.files["ttf"]).unwrap();
        assert!(woff2_data.len() < ttf_data.len());
    }

    #[test]
    fn obfuscate_only_ttf_skips_woff2() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font, &default_config(), dir.path(), "ttfonly", true,
        ).unwrap();

        assert!(result.files.contains_key("ttf"));
        assert!(!result.files.contains_key("woff2"));
    }

    // ── obfuscate_plus tests ──

    #[test]
    fn obfuscate_plus_rejects_whitespace() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate_plus("hello world", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::HasWhitespace));
    }

    #[test]
    fn obfuscate_plus_rejects_emoji() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate_plus("hello😀", &font, &default_config(), dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::HasEmoji));
    }

    #[test]
    fn obfuscate_plus_produces_valid_font_and_entities() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_plus(
            "价格998元", &font, &default_config(), dir.path(), "plus", true,
        ).unwrap();

        // Should have TTF file
        assert!(result.files["ttf"].exists());

        // Should have html_entities for each unique char
        let unique_chars: Vec<char> = deduplicate_str("价格998元").chars().collect();
        assert_eq!(result.html_entities.len(), unique_chars.len());
        for c in &unique_chars {
            let entity = &result.html_entities[&c.to_string()];
            assert!(entity.starts_with("&#x"), "entity should start with &#x: {entity}");
        }
    }

    #[test]
    fn obfuscate_plus_uses_private_use_area() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_plus(
            "abc", &font, &default_config(), dir.path(), "pua", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let cmap = gen_font.cmap().unwrap();

        // Regular ASCII chars should NOT be in cmap
        assert!(cmap_lookup(&cmap, 'a' as u32).is_none());
        assert!(cmap_lookup(&cmap, 'b' as u32).is_none());
        assert!(cmap_lookup(&cmap, 'c' as u32).is_none());

        // Parse the entity codepoints and verify they're in Private Use Area
        for (_, entity) in &result.html_entities {
            let hex_str = entity.trim_start_matches("&#x");
            let cp = u32::from_str_radix(hex_str, 16).unwrap();
            assert!((0xE000..=0xF8FF).contains(&cp), "codepoint {cp:#x} not in PUA");
            // And they should be in the font's cmap
            let ch = char::from_u32(cp).unwrap();
            assert!(cmap_lookup(&cmap, ch as u32).is_some());
        }
    }

    #[test]
    fn obfuscate_plus_generates_woff2() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_plus(
            "abc", &font, &default_config(), dir.path(), "pluswoff", false,
        ).unwrap();

        assert!(result.files.contains_key("woff2"));
        let woff2_data = std::fs::read(&result.files["woff2"]).unwrap();
        assert_eq!(&woff2_data[..4], b"wOF2");
    }

    #[test]
    fn obfuscate_plus_deduplicates_input() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_plus(
            "aaabbbccc", &font, &default_config(), dir.path(), "dedup", true,
        ).unwrap();

        // Should only have 3 entities (a, b, c)
        assert_eq!(result.html_entities.len(), 3);

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let maxp = gen_font.maxp().unwrap();
        // .notdef + 3 glyphs = 4
        assert_eq!(maxp.num_glyphs(), 4);
    }

    // ── font metadata tests ──

    #[test]
    fn obfuscate_preserves_metrics() {
        let font_data = load_base_font();
        let source = FontRef::new(&font_data).unwrap();
        let source_hhea = source.hhea().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font_data, &default_config(), dir.path(), "metrics", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let gen_hhea = gen_font.hhea().unwrap();

        assert_eq!(gen_hhea.ascender().to_i16(), source_hhea.ascender().to_i16());
        assert_eq!(gen_hhea.descender().to_i16(), source_hhea.descender().to_i16());

        let gen_head = gen_font.head().unwrap();
        let source_head = source.head().unwrap();
        assert_eq!(gen_head.units_per_em(), source_head.units_per_em());
    }

    // ── obfuscate_full tests ──

    #[test]
    fn obfuscate_full_produces_valid_ttf() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_full(
            "真好", "假的", &font, dir.path(), "full", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).expect("generated TTF should be parseable");
        assert!(gen_font.head().is_ok());
        assert!(gen_font.cmap().is_ok());
        assert!(gen_font.glyf().is_ok());
    }

    #[test]
    fn obfuscate_full_preserves_all_chars() {
        let font_data = load_base_font();
        let source = FontRef::new(&font_data).unwrap();
        let source_maxp = source.maxp().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_full(
            "真好", "假的", &font_data, dir.path(), "full_all", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let gen_maxp = gen_font.maxp().unwrap();

        // Full font should have the same number of glyphs as the original
        assert_eq!(gen_maxp.num_glyphs(), source_maxp.num_glyphs());
    }

    #[test]
    fn obfuscate_full_keeps_unrelated_chars_in_cmap() {
        let font_data = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_full(
            "真", "假", &font_data, dir.path(), "full_cmap", true,
        ).unwrap();

        let generated = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&generated).unwrap();
        let cmap = gen_font.cmap().unwrap();

        // Unrelated chars should still be in the font
        assert!(cmap_lookup(&cmap, 'a' as u32).is_some());
        assert!(cmap_lookup(&cmap, '0' as u32).is_some());
        assert!(cmap_lookup(&cmap, '好' as u32).is_some());
        // Both plain and shadow chars remain in cmap (original cmap preserved)
        assert!(cmap_lookup(&cmap, '真' as u32).is_some());
        assert!(cmap_lookup(&cmap, '假' as u32).is_some());
    }

    #[test]
    fn obfuscate_full_rejects_whitespace() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate_full("hello world", "abcdefghijk", &font, dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::HasWhitespace));
    }

    #[test]
    fn obfuscate_full_rejects_length_mismatch() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let err = obfuscate_full("abc", "de", &font, dir.path(), "t", true)
            .unwrap_err();
        assert!(matches!(err, ObfuscateError::LengthMismatch));
    }

    #[test]
    fn obfuscate_full_generates_woff2() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate_full(
            "ab", "xy", &font, dir.path(), "full_woff2", false,
        ).unwrap();

        assert!(result.files.contains_key("woff2"));
        let woff2_data = std::fs::read(&result.files["woff2"]).unwrap();
        assert_eq!(&woff2_data[..4], b"wOF2");
    }

    // ── missing char test ──

    #[test]
    fn obfuscate_rejects_missing_char() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let rare = '\u{10FFFD}';
        let err = obfuscate(
            &rare.to_string(), "a", &font, &default_config(), dir.path(), "t", true,
        ).unwrap_err();
        assert!(matches!(err, ObfuscateError::MissingChar(_)));
    }

    // ── TTF structural validity tests (related to #96) ──

    /// Verify the TTF file has a valid TrueType header (sfVersion = 0x00010000)
    #[test]
    fn ttf_has_valid_header() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font, &default_config(), dir.path(), "header", true,
        ).unwrap();

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        // TrueType sfVersion: 00 01 00 00
        assert!(data.len() > 12, "TTF file too small");
        assert_eq!(&data[0..4], &[0x00, 0x01, 0x00, 0x00], "invalid TrueType sfVersion");
    }

    /// Verify all required TrueType tables are present and parseable
    #[test]
    fn ttf_has_all_required_tables() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "真好棒", "假的坏", &font, &default_config(), dir.path(), "tables", true,
        ).unwrap();

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&data).unwrap();

        // All tables required for a valid TrueType font on Windows
        assert!(gen_font.head().is_ok(), "missing/invalid head table");
        assert!(gen_font.hhea().is_ok(), "missing/invalid hhea table");
        assert!(gen_font.maxp().is_ok(), "missing/invalid maxp table");
        assert!(gen_font.os2().is_ok(), "missing/invalid OS/2 table");
        assert!(gen_font.cmap().is_ok(), "missing/invalid cmap table");
        assert!(gen_font.glyf().is_ok(), "missing/invalid glyf table");
        assert!(gen_font.post().is_ok(), "missing/invalid post table");

        let head = gen_font.head().unwrap();
        let loca_is_long = head.index_to_loc_format() == 1;
        assert!(
            gen_font.loca(Some(loca_is_long)).is_ok(),
            "missing/invalid loca table"
        );
        assert!(gen_font.hmtx().is_ok(), "missing/invalid hmtx table");

        // Check name table has required entries
        let name = gen_font.name().expect("missing name table");
        let name_records: Vec<u16> = name.name_record().iter().map(|r| r.name_id().to_u16()).collect();
        assert!(name_records.contains(&1), "name table missing familyName (ID 1)");
        assert!(name_records.contains(&2), "name table missing styleName (ID 2)");
        assert!(name_records.contains(&4), "name table missing fullName (ID 4)");
        assert!(name_records.contains(&6), "name table missing psName (ID 6)");
    }

    /// Verify head table has valid magic number and unitsPerEm
    #[test]
    fn ttf_head_table_valid() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "ab", "xy", &font, &default_config(), dir.path(), "headcheck", true,
        ).unwrap();

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&data).unwrap();
        let head = gen_font.head().unwrap();

        assert_eq!(head.magic_number(), 0x5F0F3CF5, "invalid head magic number");
        assert!(head.units_per_em() >= 16 && head.units_per_em() <= 16384,
            "unitsPerEm out of valid range: {}", head.units_per_em());
        assert!(
            head.index_to_loc_format() == 0 || head.index_to_loc_format() == 1,
            "invalid index_to_loc_format: {}", head.index_to_loc_format()
        );
    }

    /// Verify maxp num_glyphs matches actual glyf entries
    #[test]
    fn ttf_maxp_matches_glyph_count() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abcde", "vwxyz", &font, &default_config(), dir.path(), "maxp", true,
        ).unwrap();

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&data).unwrap();
        let maxp = gen_font.maxp().unwrap();
        let head = gen_font.head().unwrap();
        let loca = gen_font.loca(Some(head.index_to_loc_format() == 1)).unwrap();

        // loca has num_glyphs + 1 entries
        assert_eq!(maxp.num_glyphs(), 6); // .notdef + 5 chars

        // Verify loca can resolve all glyph IDs
        let glyf = gen_font.glyf().unwrap();
        for gid in 0..maxp.num_glyphs() as u32 {
            // Should not panic - every glyph ID should be resolvable
            let _ = loca.get_glyf(GlyphId::new(gid), &glyf);
        }
    }

    /// Verify hhea.number_of_h_metrics matches hmtx
    #[test]
    fn ttf_hhea_hmtx_consistent() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "abc", "xyz", &font, &default_config(), dir.path(), "hmetrics", true,
        ).unwrap();

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let gen_font = FontRef::new(&data).unwrap();
        let hhea = gen_font.hhea().unwrap();
        let hmtx = gen_font.hmtx().unwrap();

        let num_h = hhea.number_of_h_metrics() as usize;
        let num_metrics = hmtx.h_metrics().len();
        assert_eq!(num_h, num_metrics,
            "hhea.number_of_h_metrics ({num_h}) != hmtx entries ({num_metrics})");
    }

    /// Verify the generated font can be re-read and re-written (round-trip)
    #[test]
    fn ttf_roundtrip_parseable() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        let result = obfuscate(
            "真0123456789好", "假6982075431的",
            &font, &default_config(), dir.path(), "roundtrip", true,
        ).unwrap();

        let data = std::fs::read(&result.files["ttf"]).unwrap();

        // First parse
        let font1 = FontRef::new(&data).unwrap();
        let cmap1 = font1.cmap().unwrap();

        // Verify cmap is consistent
        for shadow_c in "假6982075431的".chars() {
            assert!(cmap_lookup(&cmap1, shadow_c as u32).is_some(),
                "cmap missing shadow char '{shadow_c}' on first parse");
        }

        // Second parse (just ensure it doesn't panic)
        let font2 = FontRef::new(&data).unwrap();
        let cmap2 = font2.cmap().unwrap();
        for shadow_c in "假6982075431的".chars() {
            let gid1 = cmap_lookup(&cmap1, shadow_c as u32);
            let gid2 = cmap_lookup(&cmap2, shadow_c as u32);
            assert_eq!(gid1, gid2, "cmap inconsistent between parses for '{shadow_c}'");
        }
    }

    // ── Multi-font auto-detect tests ──
    //
    // Test pairs are defined per language group. For each font, we probe its
    // cmap to find which groups it supports, then run only those tests.
    // Adding a new font = one line in FONT_PATHS. No manual coverage mapping.

    /// A language group with (plaintext, shadowtext) test pairs and a probe char
    /// used to detect whether the font supports this group.
    struct LangGroup {
        label: &'static str,
        /// A char that must be in the font's cmap for this group to apply.
        probe: char,
        /// (plain, shadow) pairs to test with `obfuscate()`.
        pairs: &'static [(&'static str, &'static str)],
        /// Input strings to test with `obfuscate_plus()`.
        plus_inputs: &'static [&'static str],
    }

    const LANG_GROUPS: &[LangGroup] = &[
        LangGroup {
            label: "latin",
            probe: 'a',
            pairs: &[("abcdefgh", "stuvwxyz"), ("ABCDEFGH", "STUVWXYZ")],
            plus_inputs: &["HelloWorld"],
        },
        LangGroup {
            label: "digits",
            probe: '0',
            pairs: &[("0123456789", "9876543210")],
            plus_inputs: &["Rust2026"],
        },
        LangGroup {
            label: "hiragana",
            probe: 'あ',
            pairs: &[("あいうえお", "かきくけこ"), ("さしすせそ", "たちつてと")],
            plus_inputs: &["おはよう"],
        },
        LangGroup {
            label: "katakana",
            probe: 'ア',
            pairs: &[("アイウエオ", "カキクケコ"), ("サシスセソ", "タチツテト")],
            plus_inputs: &["テスト"],
        },
        LangGroup {
            label: "cjk_common",
            probe: '你',
            pairs: &[("你好世界", "他坏天地"), ("价格数量", "商品折扣")],
            plus_inputs: &["价格998元"],
        },
        LangGroup {
            label: "cjk_kanji",
            probe: '東',
            pairs: &[("東京大阪", "南北左右")],
            plus_inputs: &["東京大学"],
        },
        LangGroup {
            label: "hangul",
            probe: '가',
            pairs: &[("가나다라마", "바사아자차"), ("한글테스트", "대한민국인")],
            plus_inputs: &["서울부산"],
        },
    ];

    const FONT_PATHS: &[(&str, &str)] = &[
        ("KaiGenGothicCN",    "base-font/KaiGenGothicCN-Regular.ttf"),
        ("Roboto",            "base-font/Roboto-Regular.ttf"),
        ("NotoSansJP",        "base-font/NotoSansJP-Regular.ttf"),
        ("NotoSansKR",        "base-font/NotoSansKR-Regular.ttf"),
        ("NotoSansCJKsc",     "base-font/NotoSansCJKsc-Regular.ttf"),
        ("WenQuanYiMicroHei", "base-font/WenQuanYiMicroHei.ttf"),
        ("AlibabaPuHuiTi",    "base-font/Alibaba-PuHuiTi-Regular.ttf"),
        ("OPPOSans",          "base-font/OPPOSans-R.ttf"),
        ("MiSans",            "base-font/MiSans-Regular.ttf"),
    ];

    /// Probe font cmap to find supported language groups.
    fn detect_groups(font_data: &[u8]) -> Vec<&'static LangGroup> {
        let font = FontRef::new(font_data).expect("font parse failed in detect_groups");
        let cmap = font.cmap().expect("font has no cmap");
        LANG_GROUPS.iter()
            .filter(|g| cmap_lookup(&cmap, g.probe as u32).is_some())
            .collect()
    }

    // ── Assert helpers ──

    fn assert_obfuscate(font_data: &[u8], font_name: &str, plain: &str, shadow: &str, label: &str) {
        let dir = tempfile::tempdir().unwrap();
        let tag = format!("{font_name}_{label}");
        let result = obfuscate(plain, shadow, font_data, &default_config(), dir.path(), &tag, true)
            .unwrap_or_else(|e| panic!("{font_name}/{label}: obfuscate failed: {e}"));

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let parsed = FontRef::new(&data)
            .unwrap_or_else(|e| panic!("{font_name}/{label}: parse failed: {e}"));
        let cmap = parsed.cmap().unwrap();
        for c in shadow.chars() {
            assert!(cmap_lookup(&cmap, c as u32).is_some(),
                "{font_name}/{label}: shadow char '{c}' missing in cmap");
        }
    }

    fn assert_obfuscate_plus(font_data: &[u8], font_name: &str, input: &str, label: &str) {
        let dir = tempfile::tempdir().unwrap();
        let tag = format!("{font_name}_{label}_plus");
        let result = obfuscate_plus(input, font_data, &default_config(), dir.path(), &tag, true)
            .unwrap_or_else(|e| panic!("{font_name}/{label}/plus({input}): failed: {e}"));

        let unique: Vec<char> = deduplicate_str(input).chars().collect();
        assert_eq!(result.html_entities.len(), unique.len(),
            "{font_name}/{label}/plus: entity count mismatch");

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let parsed = FontRef::new(&data).unwrap();
        let cmap = parsed.cmap().unwrap();
        for (ch, entity) in &result.html_entities {
            let cp = u32::from_str_radix(entity.trim_start_matches("&#x"), 16)
                .unwrap_or_else(|_| panic!("{font_name}/plus: bad entity {entity}"));
            assert!((0xE000..=0xF8FF).contains(&cp),
                "{font_name}/plus: '{ch}' -> {cp:#x} not in PUA");
            assert!(cmap_lookup(&cmap, cp).is_some(),
                "{font_name}/plus: PUA {cp:#x} missing in cmap");
        }
    }

    fn assert_ttf_structure(font_data: &[u8], font_name: &str, plain: &str, shadow: &str) {
        let dir = tempfile::tempdir().unwrap();
        let tag = format!("{font_name}_struct");
        let result = obfuscate(plain, shadow, font_data, &default_config(), dir.path(), &tag, true)
            .unwrap_or_else(|e| panic!("{font_name}/struct: {e}"));

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        assert_eq!(&data[0..4], &[0x00, 0x01, 0x00, 0x00],
            "{font_name}: invalid TrueType sfVersion");
        let parsed = FontRef::new(&data).unwrap();
        for (table, ok) in [
            ("head", parsed.head().is_ok()), ("hhea", parsed.hhea().is_ok()),
            ("maxp", parsed.maxp().is_ok()), ("os2", parsed.os2().is_ok()),
            ("cmap", parsed.cmap().is_ok()), ("glyf", parsed.glyf().is_ok()),
            ("post", parsed.post().is_ok()), ("hmtx", parsed.hmtx().is_ok()),
        ] {
            assert!(ok, "{font_name}: missing/invalid {table}");
        }
        assert_eq!(parsed.head().unwrap().magic_number(), 0x5F0F3CF5,
            "{font_name}: bad head magic");
    }

    fn assert_preserves_metrics(font_data: &[u8], font_name: &str, plain: &str, shadow: &str) {
        let source = FontRef::new(font_data).unwrap();
        let src_head = source.head().unwrap();
        let src_hhea = source.hhea().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let tag = format!("{font_name}_metrics");
        let result = obfuscate(plain, shadow, font_data, &default_config(), dir.path(), &tag, true)
            .unwrap_or_else(|e| panic!("{font_name}/metrics: {e}"));

        let data = std::fs::read(&result.files["ttf"]).unwrap();
        let parsed = FontRef::new(&data).unwrap();
        assert_eq!(parsed.head().unwrap().units_per_em(), src_head.units_per_em(),
            "{font_name}: unitsPerEm mismatch");
        assert_eq!(parsed.hhea().unwrap().ascender().to_i16(), src_hhea.ascender().to_i16(),
            "{font_name}: ascender mismatch");
        assert_eq!(parsed.hhea().unwrap().descender().to_i16(), src_hhea.descender().to_i16(),
            "{font_name}: descender mismatch");
    }

    // ── Test entry points ──

    #[test]
    fn multi_font_obfuscate() {
        for &(name, path) in FONT_PATHS {
            let font_data = load_font(path);
            let groups = detect_groups(&font_data);
            assert!(!groups.is_empty(), "{name}: no language groups detected");
            for group in &groups {
                for &(plain, shadow) in group.pairs {
                    assert_obfuscate(&font_data, name, plain, shadow, group.label);
                }
            }
        }
    }

    #[test]
    fn multi_font_obfuscate_plus() {
        for &(name, path) in FONT_PATHS {
            let font_data = load_font(path);
            for group in detect_groups(&font_data) {
                for input in group.plus_inputs {
                    assert_obfuscate_plus(&font_data, name, input, group.label);
                }
            }
        }
    }

    #[test]
    fn multi_font_ttf_structure() {
        for &(name, path) in FONT_PATHS {
            let font_data = load_font(path);
            let groups = detect_groups(&font_data);
            let (plain, shadow) = groups[0].pairs[0];
            assert_ttf_structure(&font_data, name, plain, shadow);
        }
    }

    #[test]
    fn multi_font_preserves_metrics() {
        for &(name, path) in FONT_PATHS {
            let font_data = load_font(path);
            let groups = detect_groups(&font_data);
            let (plain, shadow) = groups[0].pairs[0];
            assert_preserves_metrics(&font_data, name, plain, shadow);
        }
    }

    #[test]
    fn multi_font_woff2() {
        for &(name, path) in FONT_PATHS {
            let font_data = load_font(path);
            let groups = detect_groups(&font_data);
            let (plain, shadow) = groups[0].pairs[0];
            let dir = tempfile::tempdir().unwrap();
            let tag = format!("{name}_woff2");
            let result = obfuscate(
                plain, shadow, &font_data, &default_config(), dir.path(), &tag, false,
            ).unwrap_or_else(|e| panic!("{name}/woff2: {e}"));

            assert!(result.files.contains_key("woff2"), "{name}: no woff2");
            let woff2 = std::fs::read(&result.files["woff2"]).unwrap();
            assert_eq!(&woff2[..4], b"wOF2", "{name}: bad woff2 magic");
        }
    }
}
