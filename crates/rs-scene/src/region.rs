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

    /// Rotation matrix (row-major, local → world) from yaw/pitch/roll in degrees.
    ///
    /// Measured against RealityScan (rotated boxes made the ground plane, the sparse cloud's
    /// rotation compared): yaw turns about Y, pitch about X and roll about Z, each by the
    /// negated angle, composed as `Rz(−roll) · Ry(−yaw) · Rx(−pitch)`.
    pub fn rotation(&self) -> [[f64; 3]; 3] {
        let [yaw, pitch, roll] = self.ypr.map(f64::to_radians);
        zyx_matrix(-roll, -yaw, -pitch)
    }

    /// Set the orientation from a rotation matrix (row-major, local → world), the inverse of
    /// [`Region::rotation`].
    pub fn set_rotation(&mut self, m: [[f64; 3]; 3]) {
        let (z, y, x) = zyx_angles(m);
        self.ypr = [-y, -x, -z].map(f64::to_degrees);
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

/// `Rz(z) · Ry(y) · Rx(x)`, row-major.
fn zyx_matrix(z: f64, y: f64, x: f64) -> [[f64; 3]; 3] {
    let (sz, cz, sy, cy, sx, cx) = (z.sin(), z.cos(), y.sin(), y.cos(), x.sin(), x.cos());
    [
        [cz * cy, cz * sy * sx - sz * cx, cz * sy * cx + sz * sx],
        [sz * cy, sz * sy * sx + cz * cx, sz * sy * cx - cz * sx],
        [-sy, cy * sx, cy * cx],
    ]
}

/// Angles `(z, y, x)` with `zyx_matrix(z, y, x) == m`.
fn zyx_angles(m: [[f64; 3]; 3]) -> (f64, f64, f64) {
    let sy = (-m[2][0]).clamp(-1.0, 1.0);
    let y = sy.asin();
    if sy.abs() < 0.999_999 {
        (m[1][0].atan2(m[0][0]), y, m[2][1].atan2(m[2][2]))
    } else {
        // Gimbal lock: only z ± x is defined; put it all in z.
        ((-m[0][1]).atan2(m[1][1]), y, 0.0)
    }
}

fn mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn transpose(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| m[j][i]))
}

fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// RealityScan's ground plane: a rigid transform from its internal frame (in which `.rsbox`
/// files are read and written) to the frame everything else uses (exports, report templates,
/// the 3D view). `setGroundPlaneFromReconstructionRegion` makes the region's axes the world
/// axes and the centre of its bottom face (local −Z) the origin; `resetGround` restores the
/// identity.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GroundFrame {
    /// Internal → display rotation (row-major).
    pub rotation: [[f64; 3]; 3],
    /// The display origin, in internal coordinates.
    pub origin: [f64; 3],
}

impl Default for GroundFrame {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl GroundFrame {
    pub const IDENTITY: Self =
        Self { rotation: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], origin: [0.0; 3] };

    /// The frame `setGroundPlaneFromReconstructionRegion` sets for `region` (internal
    /// coordinates, as exported).
    pub fn from_region(region: &Region) -> Self {
        let rot = region.rotation();
        let half = apply(rot, [0.0, 0.0, region.size[2] * 0.5]);
        Self { rotation: transpose(rot), origin: [0, 1, 2].map(|k| region.centre[k] - half[k]) }
    }

    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// A point from the internal frame to the display frame.
    pub fn to_display_point(&self, p: [f64; 3]) -> [f64; 3] {
        apply(self.rotation, [0, 1, 2].map(|k| p[k] - self.origin[k]))
    }

    /// A point from the display frame to the internal frame.
    pub fn to_internal_point(&self, p: [f64; 3]) -> [f64; 3] {
        let q = apply(transpose(self.rotation), p);
        [0, 1, 2].map(|k| q[k] + self.origin[k])
    }

    /// A region read from RealityScan, as shown.
    pub fn to_display(&self, region: &Region) -> Region {
        let mut out = region.clone();
        out.centre = self.to_display_point(region.centre);
        out.set_rotation(mul(self.rotation, region.rotation()));
        out
    }

    /// A region as shown, for RealityScan.
    pub fn to_internal(&self, region: &Region) -> Region {
        let mut out = region.clone();
        out.centre = self.to_internal_point(region.centre);
        out.set_rotation(mul(transpose(self.rotation), region.rotation()));
        out
    }
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
        let r = Region::new([0.0; 3], [2.0, 2.0, 2.0], [0.0, 0.0, -90.0]);
        let c = r.corners();
        // Local (+1, -1, -1) turns into (1, 1, -1) after a roll of −90° (+90° about Z).
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
    fn set_rotation_inverts_rotation() {
        let mut r = Region::new([0.0; 3], [1.0; 3], [37.0, -21.0, 112.0]);
        let m = r.rotation();
        r.ypr = [0.0; 3];
        r.set_rotation(m);
        let back = r.rotation();
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - back[i][j]).abs() < 1e-9, "{:?} vs {:?}", m, back);
            }
        }
        assert!((r.ypr[0] - 37.0).abs() < 1e-9 && (r.ypr[1] + 21.0).abs() < 1e-9);
        // Gimbal lock still reproduces the matrix.
        let mut g = Region::new([0.0; 3], [1.0; 3], [90.0, 30.0, 0.0]);
        let m = g.rotation();
        g.set_rotation(m);
        let back = g.rotation();
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - back[i][j]).abs() < 1e-6);
            }
        }
    }

    /// RealityScan 2.2: `rotateReconstructionRegion 10 25 40` wrote these angles; making that
    /// box the ground plane rotated the sparse cloud by the transpose of this matrix and moved
    /// the box's bottom-face centre (−26.263, −11.040, 1.215) to the origin.
    #[test]
    fn rotation_matches_realityscan() {
        let r = Region::new(
            [-5.37788772583008, -19.7537899017334, 46.003589630127],
            [85.6397220790503, 73.4500732799741, 100.361207691645],
            [-12.8608105102135, -23.7223666786119, -40.489676975879],
        );
        let measured = [[0.7414, -0.5263, 0.4162], [0.633, 0.7544, -0.1736], [-0.2226, 0.3922, 0.8925]];
        let m = r.rotation();
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - measured[i][j]).abs() < 1e-3, "{m:?}");
            }
        }
        let g = GroundFrame::from_region(&r);
        let expected = [-26.26294063, -11.0400225, 1.21544836];
        for k in 0..3 {
            assert!((g.origin[k] - expected[k]).abs() < 1e-5, "{:?}", g.origin);
        }
        // The grounding region itself sits upright on the origin afterwards.
        let shown = g.to_display(&r);
        assert!(shown.ypr.iter().all(|a| a.abs() < 1e-6), "{:?}", shown.ypr);
        assert!(shown.centre[0].abs() < 1e-6 && shown.centre[1].abs() < 1e-6);
        assert!((shown.centre[2] - r.size[2] * 0.5).abs() < 1e-6);
        // And converting back gives the original.
        let back = g.to_internal(&shown);
        for k in 0..3 {
            assert!((back.centre[k] - r.centre[k]).abs() < 1e-6 && (back.ypr[k] - r.ypr[k]).abs() < 1e-6);
        }
    }

    #[test]
    fn single_axis_angles_match_realityscan() {
        // rotateReconstructionRegion 20 0 0 / 0 20 0 / 0 0 20 wrote pitch, yaw, roll = −20 and
        // turned the box +20° about X, Y and Z respectively.
        let s = 20f64.to_radians().sin();
        let pitch = Region::new([0.0; 3], [1.0; 3], [0.0, -20.0, 0.0]).rotation();
        assert!((pitch[2][1] - s).abs() < 1e-9 && pitch[0][0] == 1.0);
        let yaw = Region::new([0.0; 3], [1.0; 3], [-20.0, 0.0, 0.0]).rotation();
        assert!((yaw[0][2] - s).abs() < 1e-9 && yaw[1][1] == 1.0);
        let roll = Region::new([0.0; 3], [1.0; 3], [0.0, 0.0, -20.0]).rotation();
        assert!((roll[1][0] - s).abs() < 1e-9 && roll[2][2] == 1.0);
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
