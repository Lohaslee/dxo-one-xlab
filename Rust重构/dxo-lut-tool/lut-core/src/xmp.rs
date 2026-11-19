//! Lightroom / Camera Raw global settings baked into an approximate RGB LUT.
//! This is deliberately not Adobe's RAW/process-version rendering engine.
use crate::cube::{cube_lut_from_flat, CubeLUT};
use std::collections::BTreeMap;

const CRS: &str = "http://ns.adobe.com/camera-raw-settings/1.0/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
const COLORS: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];
const CENTERS: [f32; 8] = [0., 30., 60., 120., 180., 240., 270., 300.];

type Curve = Vec<(f32, f32)>;

pub struct XmpLut {
    pub lut: CubeLUT,
    pub omitted: Vec<String>,
}

#[derive(Default)]
struct Settings {
    exposure: f32,
    contrast: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    saturation: f32,
    vibrance: f32,
    hsl: [[f32; 3]; 8],
    curves: [Curve; 4],
}

fn number(name: &str, value: &str, min: f32, max: f32) -> Result<f32, String> {
    let n: f32 = value
        .parse()
        .map_err(|_| format!("XMP {name} 不是有效数字"))?;
    if !n.is_finite() || n < min || n > max {
        return Err(format!("XMP {name} 超出范围 {min}…{max}"));
    }
    Ok(n)
}

fn curve(name: &str, value: &str) -> Result<Curve, String> {
    let mut out = Vec::new();
    for pair in value.split(';') {
        let parts: Vec<_> = pair.split(',').map(str::trim).collect();
        if parts.len() != 2 || out.len() >= 256 {
            return Err(format!("XMP {name} 曲线点无效"));
        }
        let x = number(name, parts[0], 0., 255.)? / 255.;
        let y = number(name, parts[1], 0., 255.)? / 255.;
        if out.last().is_some_and(|&(prev, _)| x <= prev) {
            return Err(format!("XMP {name} 曲线横坐标必须递增"));
        }
        out.push((x, y));
    }
    if out.len() < 2 || out[0].0 != 0. || out.last().unwrap().0 != 1. {
        return Err(format!("XMP {name} 曲线须包含 0 和 255 端点"));
    }
    Ok(out)
}

fn metadata(name: &str) -> bool {
    matches!(
        name,
        "Version"
            | "ProcessVersion"
            | "PresetType"
            | "Cluster"
            | "UUID"
            | "Name"
            | "ShortName"
            | "SortName"
            | "Group"
            | "Description"
            | "Copyright"
            | "ContactInfo"
            | "CameraModelRestriction"
            | "HasSettings"
            | "HasCrop"
            | "AlreadyApplied"
            | "OverrideLookVignette"
            | "RequiresRGBTables"
            | "SupportsAmount"
            | "SupportsColor"
            | "SupportsMonochrome"
            | "SupportsHighDynamicRange"
            | "SupportsNormalDynamicRange"
            | "SupportsSceneReferred"
            | "SupportsOutputReferred"
    )
}

/// Parse namespace-qualified RDF attributes or properties. Nested correction settings
/// are never promoted to global settings. DTDs/entities are disabled by roxmltree.
pub fn parse_xmp(text: &str) -> Result<XmpLut, String> {
    if text.len() > MAX_BYTES {
        return Err("XMP 文件过大（上限 4MB）".into());
    }
    let doc = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 50_000,
            ..Default::default()
        },
    )
    .map_err(|e| format!("XMP XML 无效: {e}"))?;
    // Adobe profile tables are encoded payloads, not ordinary develop settings.
    if doc.descendants().any(|n| {
        (n.tag_name().namespace() == Some(CRS)
            && matches!(
                n.tag_name().name(),
                "Look" | "LookTable" | "RGBTable" | "Table" | "Profile"
            ))
            || n.attributes().any(|a| {
                a.namespace() == Some(CRS)
                    && matches!(
                        a.name(),
                        "Look" | "LookTable" | "RGBTable" | "Table" | "Profile"
                    )
            })
    }) {
        return Err("XMP 含 Adobe Look/Profile 内嵌配置，暂不支持转换".into());
    }
    let mut props = BTreeMap::<String, String>::new();
    let mut add = |name: &str, value: String| -> Result<(), String> {
        if props.insert(name.into(), value).is_some() {
            return Err(format!("XMP 全局参数重复: {name}"));
        }
        Ok(())
    };
    for desc in doc
        .descendants()
        .filter(|n| n.has_tag_name((RDF, "Description")))
    {
        // Only outer descriptions: a description under a CRS property is local/profile data.
        if desc
            .ancestors()
            .skip(1)
            .any(|n| n.tag_name().namespace() == Some(CRS))
        {
            continue;
        }
        for a in desc.attributes().filter(|a| a.namespace() == Some(CRS)) {
            add(a.name(), a.value().trim().into())?;
        }
        for child in desc
            .children()
            .filter(|n| n.tag_name().namespace() == Some(CRS))
        {
            let name = child.tag_name().name();
            let value = if name.starts_with("ToneCurve") && !name.starts_with("ToneCurveName") {
                child
                    .descendants()
                    .filter(|n| n.has_tag_name((RDF, "li")))
                    .map(|n| n.text().unwrap_or("").trim())
                    .collect::<Vec<_>>()
                    .join(";")
            } else if child.children().any(|n| n.is_element()) {
                // Report nested masks/resources as a whole, including numeric zero defaults.
                "[复合参数]".into()
            } else {
                child.text().unwrap_or("").trim().into()
            };
            add(name, value)?;
        }
    }
    if props
        .get("PresetType")
        .is_some_and(|v| v.eq_ignore_ascii_case("Look"))
    {
        return Err("XMP 为 Adobe Look/Profile，暂不支持转换".into());
    }
    let mut s = Settings::default();
    let mut omitted = Vec::new();
    let mut supported = false;
    for (name, value) in &props {
        let scalar = match name.as_str() {
            "Exposure2012" => Some((&mut s.exposure, -5., 5.)),
            "Contrast2012" => Some((&mut s.contrast, -100., 100.)),
            "Highlights2012" => Some((&mut s.highlights, -100., 100.)),
            "Shadows2012" => Some((&mut s.shadows, -100., 100.)),
            "Whites2012" => Some((&mut s.whites, -100., 100.)),
            "Blacks2012" => Some((&mut s.blacks, -100., 100.)),
            "Saturation" => Some((&mut s.saturation, -100., 100.)),
            "Vibrance" => Some((&mut s.vibrance, -100., 100.)),
            _ => None,
        };
        if let Some((dest, min, max)) = scalar {
            *dest = number(name, value, min, max)?;
            supported = true;
            continue;
        }
        let mut hsl = false;
        for (i, color) in COLORS.iter().enumerate() {
            for (j, prefix) in [
                "HueAdjustment",
                "SaturationAdjustment",
                "LuminanceAdjustment",
            ]
            .iter()
            .enumerate()
            {
                if *name == format!("{prefix}{color}") {
                    s.hsl[i][j] = number(name, value, -100., 100.)?;
                    supported = true;
                    hsl = true;
                }
            }
        }
        if hsl {
            continue;
        }
        let mut is_curve = false;
        for (i, suffix) in ["", "Red", "Green", "Blue"].iter().enumerate() {
            let modern = format!("ToneCurvePV2012{suffix}");
            let legacy = format!("ToneCurve{suffix}");
            if *name == modern || *name == legacy {
                let parsed = curve(name, value)?;
                // Modern points take precedence if both generations are present.
                if *name == modern || !props.contains_key(&modern) {
                    s.curves[i] = parsed;
                }
                supported = true;
                is_curve = true;
            }
        }
        if is_curve || metadata(name) {
            continue;
        }
        // Named curves are represented only when explicit corresponding points exist.
        if (name == "ToneCurveName2012" && props.contains_key("ToneCurvePV2012"))
            || (name == "ToneCurveName" && props.contains_key("ToneCurve"))
        {
            continue;
        }
        if !value.is_empty()
            && !value.eq_ignore_ascii_case("false")
            && !value.eq_ignore_ascii_case("none")
            && !value.eq_ignore_ascii_case("linear")
            && value.parse::<f32>().map_or(true, |n| n != 0.)
        {
            omitted.push(name.clone());
        }
    }
    if !supported {
        return Err(format!(
            "XMP 未找到可转换的全局调色参数{}",
            if omitted.is_empty() {
                String::new()
            } else {
                format!("；不支持: {}", omitted.join("、"))
            }
        ));
    }
    let mut flat = Vec::with_capacity(16 * 16 * 16);
    let mut changed = false;
    for b in 0..16 {
        for g in 0..16 {
            for r in 0..16 {
                let input = [r as f32 / 15., g as f32 / 15., b as f32 / 15.];
                let output = s.apply(input);
                changed |= input.iter().zip(output).any(|(a, b)| (a - b).abs() > 1e-5);
                flat.push(output);
            }
        }
    }
    if !changed && !omitted.is_empty() {
        return Err(format!(
            "XMP 可转换参数没有有效调色变化；未转换: {}",
            omitted.join("、")
        ));
    }
    Ok(XmpLut {
        lut: cube_lut_from_flat(16, &flat)?,
        omitted,
    })
}

fn apply_curve(points: &Curve, x: f32) -> f32 {
    if points.is_empty() {
        return x;
    }
    for pair in points.windows(2) {
        let [(x0, y0), (x1, y1)] = [pair[0], pair[1]];
        if x <= x1 {
            return y0 + (y1 - y0) * ((x - x0) / (x1 - x0)).clamp(0., 1.);
        }
    }
    points.last().unwrap().1
}

impl Settings {
    fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut c = rgb.map(|v| {
            let x = (v * self.exposure.exp2()).clamp(0., 1.);
            // Smooth tonal masks approximate global controls on display RGB.
            let shadows = (1. - x).powi(2) * x * 2.;
            let highlights = x.powi(2) * (1. - x) * 2.;
            let y = x
                + self.shadows / 100. * shadows * 0.5
                + self.highlights / 100. * highlights * 0.5
                + self.whites / 100. * x.powi(3) * 0.25
                + self.blacks / 100. * (1. - x).powi(3) * 0.25;
            let y = ((y - 0.5) * (1. + self.contrast / 100. * 0.8) + 0.5).clamp(0., 1.);
            apply_curve(&self.curves[0], y)
        });
        for (i, channel) in c.iter_mut().enumerate() {
            *channel = apply_curve(&self.curves[i + 1], *channel);
        }
        let max = c.into_iter().fold(0f32, f32::max);
        let min = c.into_iter().fold(1f32, f32::min);
        let chroma = max - min;
        let light = (max + min) * 0.5;
        let mut sat = if chroma < 1e-6 {
            0.
        } else {
            chroma / (1. - (2. * light - 1.).abs()).max(1e-6)
        };
        let mut hue = if chroma < 1e-6 {
            0.
        } else if max == c[0] {
            60. * ((c[1] - c[2]) / chroma).rem_euclid(6.)
        } else if max == c[1] {
            60. * ((c[2] - c[0]) / chroma + 2.)
        } else {
            60. * ((c[0] - c[1]) / chroma + 4.)
        };
        let mut adjust = [0.; 3];
        if chroma >= 1e-6 {
            // Linear interpolation between neighboring color sectors; weights sum to one.
            let i = (0..8).rev().find(|&i| hue >= CENTERS[i]).unwrap_or(0);
            let j = (i + 1) % 8;
            let end = if j == 0 { 360. } else { CENTERS[j] };
            let t = (hue - CENTERS[i]) / (end - CENTERS[i]);
            for (k, a) in adjust.iter_mut().enumerate() {
                *a = self.hsl[i][k] * (1. - t) + self.hsl[j][k] * t;
            }
        }
        hue = (hue + adjust[0] * 0.3).rem_euclid(360.);
        sat *= (1. + adjust[1] / 100.)
            * (1. + self.saturation / 100.)
            * (1. + self.vibrance / 100. * (1. - sat));
        sat = sat.clamp(0., 1.);
        let light = (light + adjust[2] / 100. * 0.25 * chroma).clamp(0., 1.);
        let ch = (1. - (2. * light - 1.).abs()) * sat;
        let x = ch * (1. - ((hue / 60.) % 2. - 1.).abs());
        let sector = match (hue / 60.) as usize {
            0 => [ch, x, 0.],
            1 => [x, ch, 0.],
            2 => [0., ch, x],
            3 => [0., x, ch],
            4 => [x, 0., ch],
            _ => [ch, 0., x],
        };
        sector.map(|v| (v + light - ch * 0.5).clamp(0., 1.))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn xml(attrs: &str, children: &str) -> String {
        format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><r:RDF xmlns:r="{RDF}"><r:Description xmlns:c="{CRS}" {attrs}>{children}</r:Description></r:RDF></x:xmpmeta>"#
        )
    }
    #[test]
    fn namespace_alias_identity_and_persistence() {
        let p = parse_xmp(&xml(r#"c:Exposure2012="0""#, "")).unwrap();
        let restored = CubeLUT::from_bytes(&p.lut.to_bytes()).unwrap();
        for r in 0..16 {
            for g in 0..16 {
                for b in 0..16 {
                    for (i, expected) in [r, g, b].iter().enumerate() {
                        assert!(
                            (restored.values[r][g][b][i] - *expected as f32 / 15.).abs() < 1e-5
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn exposure_curve_hsl_and_saturation_change_pixels() {
        let p = parse_xmp(&xml("", "<c:Exposure2012>1</c:Exposure2012>")).unwrap();
        assert!((p.lut.values[3][3][3][0] - 0.4).abs() < 1e-5);
        let p = parse_xmp(&xml(r#"c:Saturation="-100""#, "")).unwrap();
        let red = p.lut.values[15][0][0];
        assert!((red[0] - red[1]).abs() < 1e-6 && (red[1] - red[2]).abs() < 1e-6);
        let p = parse_xmp(&xml(r#"c:HueAdjustmentRed="100""#, "")).unwrap();
        assert!(p.lut.values[15][0][0][1] > 0.4);
        let p = parse_xmp(&xml("", "<c:ToneCurvePV2012Red><r:Seq><r:li>0, 0</r:li><r:li>128, 200</r:li><r:li>255, 255</r:li></r:Seq></c:ToneCurvePV2012Red>")).unwrap();
        assert!(p.lut.values[8][8][8][0] > p.lut.values[8][8][8][1] + 0.2);
    }
    #[test]
    fn omission_and_local_settings_are_visible() {
        let p = parse_xmp(&xml(r#"c:Exposure2012="0.5" c:Temperature="6000" c:Clarity2012="25" c:Version="15.0""#,
            "<c:MaskGroupBasedCorrections><r:Seq><r:li><r:Description c:Exposure2012='4'/></r:li></r:Seq></c:MaskGroupBasedCorrections>")).unwrap();
        assert_eq!(
            p.omitted,
            ["Clarity2012", "MaskGroupBasedCorrections", "Temperature"]
        );
        assert!(p.lut.values[3][3][3][0] < 0.3);
        let err = parse_xmp(&xml(r#"c:Temperature="6000""#, ""))
            .err()
            .unwrap();
        assert!(err.contains("Temperature"));
        // Neutral defaults must not disguise an unsupported-only preset as success.
        assert!(parse_xmp(&xml(r#"c:Exposure2012="0" c:Clarity2012="25""#, "")).is_err());
    }
    #[test]
    fn tonal_controls_are_bounded_and_have_effect() {
        for name in [
            "Contrast2012",
            "Highlights2012",
            "Shadows2012",
            "Whites2012",
            "Blacks2012",
            "Vibrance",
            "SaturationAdjustmentRed",
            "LuminanceAdjustmentRed",
        ] {
            let low = parse_xmp(&xml(&format!("c:{name}='-100'"), "")).unwrap();
            let high = parse_xmp(&xml(&format!("c:{name}='100'"), "")).unwrap();
            assert_ne!(low.lut.to_bytes(), high.lut.to_bytes(), "{name}");
            for lut in [low.lut, high.lut] {
                assert!(lut
                    .values
                    .iter()
                    .flatten()
                    .flatten()
                    .flatten()
                    .all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            }
        }
    }

    #[test]
    fn invalid_or_unsafe_inputs_fail() {
        for a in [
            r#"c:Exposure2012="NaN""#,
            r#"c:Saturation="101""#,
            r#"c:Exposure2012="-6""#,
        ] {
            assert!(parse_xmp(&xml(a, "")).is_err());
        }
        for body in ["<c:Exposure2012>1</c:Exposure2012><c:Exposure2012>2</c:Exposure2012>",
            "<c:ToneCurvePV2012><r:Seq><r:li>255, 0</r:li><r:li>0, 255</r:li></r:Seq></c:ToneCurvePV2012>",
            "<c:Look><r:Description/></c:Look>"] {
            assert!(parse_xmp(&xml("", body)).is_err());
        }
        assert!(parse_xmp("<!DOCTYPE a [<!ENTITY x 'payload'>]><a>&x;</a>").is_err());
        assert!(parse_xmp("<broken>").is_err());
        assert!(parse_xmp(&" ".repeat(MAX_BYTES + 1)).is_err());
        assert!(parse_xmp("<a Exposure2012='1'/>").is_err());
    }
}
