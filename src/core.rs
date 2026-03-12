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

    // ── missing char test ──

    #[test]
    fn obfuscate_rejects_missing_char() {
        let font = load_base_font();
        let dir = tempfile::tempdir().unwrap();
        // U+FFFD is unlikely to have a glyph mapped in a CJK font's cmap
        // Use a rare char that's almost certainly not in this font
        let rare = '\u{10FFFD}'; // last valid unicode codepoint in supplementary private use area
        let err = obfuscate(
            &rare.to_string(), "a", &font, &default_config(), dir.path(), "t", true,
        ).unwrap_err();
        assert!(matches!(err, ObfuscateError::MissingChar(_)));
    }
}
