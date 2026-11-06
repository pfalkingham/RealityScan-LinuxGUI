//! The reconstruction region (RealityScan's `.rsbox` files).
//!
//! RealityScan exports and imports the region as a small XML file
//! (`exportReconstructionRegion` / `setReconstructionRegion`). The box is an
//! oriented cuboid: a centre, a width/height/depth and yaw/pitch/roll angles.

use std::fmt::Write as _;

#[derive(Debug, thiserror::Error)]
pub enum RegionError {
    #[error("not a reconstruction region file")]
    NotRegion,
    #[error("malformed region file: {0}")]
    Malformed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// An oriented box in RealityScan's internal coordinate system.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub centre: [f64; 3],
    /// Width, height, depth (extent along the box's local X, Y, Z).
    pub size: [f64; 3],
    /// Yaw, pitch, roll in degrees, as RealityScan writes them.
    pub ypr: [f64; 3],
    /// Component that owns the region (`ownerId`), kept so a round trip stays valid.
    pub owner: String,
    /// `Residual` R/t/s attributes, copied verbatim.
    residual: Option<(String, String, String)>,
}

impl Region {
    pub fn new(centre: [f64; 3], size: [f64; 3], ypr: [f64; 3]) -> Self {
        Self { centre, size, ypr, owner: String::new(), residual: None }
    }

    /// Rotation matrix (row-major, local → world) from yaw/pitch/roll in degrees, applied
    /// as yaw about Z, then pitch about Y, then roll about X.
    pub fn rotation(&self) -> [[f64; 3]; 3] {
        let [y, p, r] = self.ypr.map(f64::to_radians);
        let (sy, cy, sp, cp, sr, cr) = (y.sin(), y.cos(), p.sin(), p.cos(), r.sin(), r.cos());
        [
            [cy * cp, cy * sp * sr - sy * cr, cy * sp * cr + sy * sr],
            [sy * cp, sy * sp * sr + cy * cr, sy * sp * cr - cy * sr],
            [-sp, cp * sr, cp * cr],
        ]
    }

    /// The eight corners in world space, ordered so that bit 0 = +X, bit 1 = +Y, bit 2 = +Z.
    pub fn corners(&self) -> [[f64; 3]; 8] {
        let rot = self.rotation();
        let mut out = [[0.0; 3]; 8];
        for (i, c) in out.iter_mut().enumerate() {
            let local = [
                if i & 1 != 0 { 0.5 } else { -0.5 } * self.size[0],
                if i & 2 != 0 { 0.5 } else { -0.5 } * self.size[1],
                if i & 4 != 0 { 0.5 } else { -0.5 } * self.size[2],
            ];
            for k in 0..3 {
                c[k] = self.centre[k] + rot[k][0] * local[0] + rot[k][1] * local[1] + rot[k][2] * local[2];
            }
        }
        out
    }

    /// The twelve edges as corner index pairs.
    pub const EDGES: [(usize, usize); 12] =
        [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];

    /// Serialise in RealityScan's `.rsbox` format.
    pub fn to_rsbox(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            r#"<ReconstructionRegion globalCoordinateSystem="NONE" globalCoordinateSystemWkt="NONE" globalCoordinateSystemName="NONE""#
        );
        let _ = writeln!(
            s,
            r#"   isGeoreferenced="0" isLatLon="0" yawPitchRoll="{}" widthHeightDepth="{}">"#,
            fmt3(self.ypr),
            fmt3(self.size)
        );
        let _ = writeln!(s, r#"  <Header magic="5395016" version="2"/>"#);
        let _ =
            writeln!(s, "  <CentreEuclid>\n    <centre>{}</centre>\n  </CentreEuclid>", fmt3(self.centre));
        let (r, t, sc) = self
            .residual
            .clone()
            .unwrap_or_else(|| ("1 0 0 0 1 0 0 0 1".to_owned(), "0 0 0".to_owned(), "1".to_owned()));
        let _ = writeln!(s, r#"  <Residual R="{r}" t="{t}" s="{sc}" ownerId="{}"/>"#, self.owner);
        s.push_str("</ReconstructionRegion>\n");
        s
    }
}

fn fmt3(v: [f64; 3]) -> String {
    format!("{} {} {}", v[0], v[1], v[2])
}

pub fn load_rsbox(path: &std::path::Path) -> Result<Region, RegionError> {
    parse_rsbox(&std::fs::read_to_string(path)?)
}

/// Parse an `.rsbox` file. Only the handful of attributes we use are read; the parser is
/// deliberately minimal (RealityScan's writer is the only producer).
pub fn parse_rsbox(text: &str) -> Result<Region, RegionError> {
    if !text.contains("<ReconstructionRegion") {
        return Err(RegionError::NotRegion);
    }
    // RealityScan writes `widthHeightDepth` (and friends) as attributes, except after a
    // `setReconstructionRegion` import, when they come back as child elements.
    let attr = |name: &str| -> Option<String> {
        let key = format!(" {name}=\"");
        if let Some(start) = text.find(&key).map(|i| i + key.len()) {
            let end = text[start..].find('"')? + start;
            return Some(text[start..end].to_owned());
        }
        let open = format!("<{name}>");
        let start = text.find(&open)? + open.len();
        let end = text[start..].find(&format!("</{name}>"))? + start;
        Some(text[start..end].trim().to_owned())
    };
    let triple = |name: &str| -> Result<[f64; 3], RegionError> {
        let v = attr(name).ok_or_else(|| RegionError::Malformed(format!("missing {name}")))?;
        parse3(&v).ok_or_else(|| RegionError::Malformed(format!("bad {name}: {v}")))
    };
    // RealityScan writes the centre as `<CentreEuclid><centre>x y z</centre></CentreEuclid>`
    // after a `setReconstructionRegion`, and as `<CentreEuclid centre="x y z"/>` after an
    // automatic region. Take either.
    let centre = {
        let attr = text.find("<CentreEuclid").and_then(|tag| {
            let rest = &text[tag..];
            let end = rest.find('>')?;
            let open = &rest[..end];
            let v = open.find("centre=\"")? + "centre=\"".len();
            let close = open[v..].find('"')? + v;
            parse3(&open[v..close])
        });
        match attr {
            Some(c) => c,
            None => {
                let start =
                    text.find("<centre>").ok_or_else(|| RegionError::Malformed("missing centre".into()))?
                        + "<centre>".len();
                let end =
                    text[start..].find("</centre>").ok_or_else(|| RegionError::Malformed("centre".into()))?
                        + start;
                parse3(&text[start..end]).ok_or_else(|| RegionError::Malformed("bad centre".into()))?
            }
        }
    };
    Ok(Region {
        centre,
        size: triple("widthHeightDepth")?,
        ypr: triple("yawPitchRoll")?,
        owner: attr("ownerId").unwrap_or_default(),
        residual: match (attr("R"), attr("t"), attr("s")) {
            (Some(r), Some(t), Some(s)) => Some((r, t, s)),
            _ => None,
        },
    })
}

fn parse3(s: &str) -> Option<[f64; 3]> {
    let mut it = s.split_whitespace().map(|v| v.parse::<f64>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_centre_attribute_form() {
        let text = r#"<ReconstructionRegion globalCoordinateSystem="NONE" isGeoreferenced="0" isLatLon="0" yawPitchRoll="0 -0 -0">
  <widthHeightDepth>12.67 15.18 40.70</widthHeightDepth>
  <Header magic="5395016" version="2"/>
  <CentreEuclid centre="2.47 2.49 18.90"/>
  <Residual R="1 0 0 0 1 0 0 0 1" t="0 0 0" s="1" ownerId="{48FF2E98-98DE-440A-AFEE-FB8FDF603C7D}"/>
</ReconstructionRegion>"#;
        let r = parse_rsbox(text).unwrap();
        assert!((r.centre[2] - 18.90).abs() < 1e-9 && (r.size[1] - 15.18).abs() < 1e-9, "{r:?}");
    }

    use super::*;

    const SAMPLE: &str = r#"<ReconstructionRegion globalCoordinateSystem="NONE" globalCoordinateSystemWkt="NONE" globalCoordinateSystemName="NONE"
   isGeoreferenced="0" isLatLon="0" yawPitchRoll="0 -0 -0" widthHeightDepth="13.135947227478 13.0347709655762 48.5750465393066">
  <Header magic="5395016" version="2"/>
  <CentreEuclid>
    <centre>-0.560859441757202 -1.2527642250061 22.4796028137207</centre>
  </CentreEuclid>
  <Residual R="1 0 0 0 1 0 0 0 1" t="0 0 0" s="1" ownerId="{DD95D010-5333-42FB-AEF0-FB224D04F2BB}"/>
</ReconstructionRegion>
"#;

    #[test]
    fn parses_realityscan_output_and_round_trips() {
        let r = parse_rsbox(SAMPLE).unwrap();
        assert_eq!(r.centre, [-0.560859441757202, -1.2527642250061, 22.4796028137207]);
        assert_eq!(r.size[2], 48.5750465393066);
        assert_eq!(r.owner, "{DD95D010-5333-42FB-AEF0-FB224D04F2BB}");
        let again = parse_rsbox(&r.to_rsbox()).unwrap();
        assert_eq!(again, r);
        assert!(r.to_rsbox().contains(r#"magic="5395016""#));
    }

    #[test]
    fn corners_follow_size_and_yaw() {
        let r = Region::new([1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 0.0]);
        let c = r.corners();
        assert_eq!(c[0], [0.0, 0.0, 0.0]);
        assert_eq!(c[7], [2.0, 4.0, 6.0]);
        let r = Region::new([0.0; 3], [2.0, 2.0, 2.0], [90.0, 0.0, 0.0]);
        let c = r.corners();
        // Local (+1, -1, -1) turns into (1, 1, -1) after a 90° yaw about Z.
        assert!(
            (c[1][0] - 1.0).abs() < 1e-9 && (c[1][1] - 1.0).abs() < 1e-9 && c[1][2] == -1.0,
            "{:?}",
            c[1]
        );
    }

    #[test]
    fn parses_element_form_written_after_import() {
        let text = r#"<ReconstructionRegion globalCoordinateSystem="NONE" isGeoreferenced="0" isLatLon="0" yawPitchRoll="0 -0 -0">
  <widthHeightDepth>24.2836933135986 13.0347709655762 48.5750465393066</widthHeightDepth>
  <Header magic="5395016" version="2"/>
  <CentreEuclid>
    <centre>5.01301383972168 -1.2527642250061 22.4796028137207</centre>
  </CentreEuclid>
  <Residual R="1 0 0 0 1 0 0 0 1" t="0 0 0" s="1" ownerId="{X}"/>
</ReconstructionRegion>"#;
        let r = parse_rsbox(text).unwrap();
        assert_eq!(r.size[0], 24.2836933135986);
        assert_eq!(r.centre[0], 5.01301383972168);
    }

    #[test]
    fn rotation_columns_are_unit_axes() {
        let r = Region::new([0.0; 3], [1.0; 3], [30.0, 10.0, -20.0]);
        let m = r.rotation();
        for (c0, c1, c2) in
            [(m[0][0], m[1][0], m[2][0]), (m[0][1], m[1][1], m[2][1]), (m[0][2], m[1][2], m[2][2])]
        {
            let len = (c0 * c0 + c1 * c1 + c2 * c2).sqrt();
            assert!((len - 1.0).abs() < 1e-12);
        }
        // Columns are orthogonal.
        let dot = m[0][0] * m[0][1] + m[1][0] * m[1][1] + m[2][0] * m[2][1];
        assert!(dot.abs() < 1e-12);
    }

    #[test]
    fn rejects_other_files() {
        assert!(matches!(parse_rsbox("<Model/>"), Err(RegionError::NotRegion)));
    }
}
