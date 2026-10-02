//! System fallback fonts for filenames.
//!
//! egui bundles Latin and emoji glyphs only, so Japanese (and Korean / Thai)
//! names render as empty boxes. At startup a background thread scans the
//! system fonts, picks the best match per script from a preference list, and
//! installs them as fallbacks behind egui's own fonts. The scan runs off the
//! UI thread (it can take a few hundred ms on a big font directory); names
//! show up as soon as it finishes.

use egui::{Context, FontData, FontDefinitions, FontFamily};

/// Preferred families per script, best first. Matched case-insensitively
/// against every family name a face carries.
const SCRIPTS: &[(&str, &[&str])] = &[
    (
        "cjk",
        &[
            "Noto Sans CJK JP",
            "Noto Sans JP",
            "Source Han Sans JP",
            "Yu Gothic UI",
            "Yu Gothic",
            "Meiryo UI",
            "Meiryo",
            "MS UI Gothic",
            "MS Gothic",
            "Hiragino Sans",
            "Hiragino Kaku Gothic ProN",
            "Hiragino Kaku Gothic Pro",
            "IPAexGothic",
            "IPAGothic",
            "IPAPGothic",
            "TakaoPGothic",
            "TakaoGothic",
            "VL PGothic",
            "Noto Sans CJK SC",
            "Noto Sans CJK",
            "Source Han Sans",
            "WenQuanYi Zen Hei",
            "WenQuanYi Micro Hei",
            "Microsoft YaHei",
            "PingFang SC",
            "Unifont JP",
            "Unifont",
        ],
    ),
    (
        "korean",
        &[
            "Noto Sans CJK KR",
            "Noto Sans KR",
            "Malgun Gothic",
            "Apple SD Gothic Neo",
            "NanumGothic",
            "Nanum Gothic",
        ],
    ),
    (
        "thai",
        &[
            "Noto Sans Thai",
            "Leelawadee UI",
            "Leelawadee",
            "Thonburi",
            "Sarabun",
            "Garuda",
            "Loma",
            "Waree",
            "Norasi",
            "Noto Serif Thai",
            "Tahoma",
        ],
    ),
];

/// Load the fallback fonts in the background and add them to `ctx`.
pub fn install_system_fallbacks(ctx: &Context) {
    let ctx = ctx.clone();
    let spawned = std::thread::Builder::new()
        .name("mmce-fonts".into())
        .spawn(move || {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            let found = pick_fonts(&db);
            if found.is_empty() {
                log::info!("no system CJK / Thai font found; those filenames will show as boxes");
                return;
            }
            let mut defs = FontDefinitions::default();
            for (name, data) in found {
                log::info!("fallback font: {name}");
                defs.font_data.insert(name.clone(), data);
                for family in [FontFamily::Proportional, FontFamily::Monospace] {
                    defs.families.entry(family).or_default().push(name.clone());
                }
            }
            ctx.set_fonts(defs);
            ctx.request_repaint();
        });
    if let Err(e) = spawned {
        log::warn!("font loader thread: {e}");
    }
}

/// One font per script: the first preferred family present, regular weight
/// where available. Returns (unique name, data) pairs.
fn pick_fonts(db: &fontdb::Database) -> Vec<(String, FontData)> {
    let mut out = Vec::new();
    for (script, families) in SCRIPTS {
        let pick = families.iter().find_map(|want| {
            let mut faces: Vec<&fontdb::FaceInfo> = db
                .faces()
                .filter(|f| f.families.iter().any(|(n, _)| n.eq_ignore_ascii_case(want)))
                .collect();
            // Regular upright first; otherwise whatever the family has.
            faces.sort_by_key(|f| {
                (
                    f.style != fontdb::Style::Normal,
                    f.weight.0.abs_diff(fontdb::Weight::NORMAL.0),
                )
            });
            faces.first().map(|f| (*want, f.id))
        });
        let Some((family, id)) = pick else { continue };
        let loaded = db.with_face_data(id, |bytes, index| {
            let mut data = FontData::from_owned(bytes.to_vec());
            data.index = index;
            data
        });
        if let Some(data) = loaded {
            out.push((format!("mmce-{script}-{family}"), data));
        }
    }
    out
}
