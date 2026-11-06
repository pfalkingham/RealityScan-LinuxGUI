//! Sparse point cloud and camera poses of the selected component, via templates.

use serde::Deserialize;

use crate::Error;

/// Emits one `x y z r g b trackLength` line per registered point.
pub const POINTS_TEMPLATE: &str = include_str!("../templates/points.tpl");
/// Emits a JSON array of registered cameras (null-terminated like the state template).
pub const CAMERAS_TEMPLATE: &str = include_str!("../templates/cameras.tpl");

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SparseCloud {
    pub positions: Vec<[f32; 3]>,
    pub colors: Vec<[u8; 3]>,
    pub track_lengths: Vec<u16>,
}

impl SparseCloud {
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }
}

/// A registered camera in the selected component.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CameraPose {
    /// Camera index within the component.
    pub index: u32,
    /// RealityScan's input index of the image it was computed from (import order). This, not
    /// the name, is what tells same-named files in different folders apart.
    #[serde(rename = "imageIndex")]
    pub image_index: u32,
    /// Image file name without extension.
    pub image: String,
    pub width: u32,
    pub height: u32,
    /// Camera centre in the output coordinate system.
    pub position: [f64; 3],
    /// Row-major 3×3 rotation as RealityScan reports it (`R00 R01 R02 R10 …`).
    pub rotation: [f64; 9],
    /// Focal length in units of the 36 mm film width (RealityScan's normalised form;
    /// multiply by 36 for the 35 mm-equivalent value). Applies along the image's longer side.
    pub focal: f64,
    /// Principal point offset, relative to the image centre, as a fraction of the longer side.
    pub principal: [f64; 2],
    /// Brown radial distortion coefficients `k1..k4`, on unit-focal-plane coordinates.
    #[serde(default)]
    pub radial: [f64; 4],
    /// Brown tangential distortion coefficients `t1, t2`.
    #[serde(default)]
    pub tangential: [f64; 2],
}

/// The registered camera of an image, if it is aligned in the selected component: matched by
/// RealityScan's input index, or by name when that name is unique among the cameras.
pub fn camera_for_image<'a>(cameras: &'a [CameraPose], img: &crate::Image) -> Option<&'a CameraPose> {
    cameras.iter().find(|c| c.image_index == img.index).or_else(|| {
        let mut named = cameras.iter().filter(|c| c.image.eq_ignore_ascii_case(&img.name));
        match (named.next(), named.next()) {
            (Some(c), None) => Some(c),
            _ => None,
        }
    })
}

impl CameraPose {
    /// Where a world point lands in this image, in pixels from the top-left corner, if it is
    /// in front of the camera: RealityScan's registered calibration, i.e. a pinhole followed
    /// by Brown radial and tangential distortion on unit-focal-plane coordinates. The result
    /// reproduces RealityScan's own reprojection of a control point to within its reported
    /// error (checked on a probe: 0.3–0.8 px, against 3–5 px for a plain pinhole).
    pub fn project(&self, p: [f64; 3]) -> Option<(f64, f64)> {
        let d = [p[0] - self.position[0], p[1] - self.position[1], p[2] - self.position[2]];
        let r = &self.rotation;
        let x = r[0] * d[0] + r[1] * d[1] + r[2] * d[2];
        let y = r[3] * d[0] + r[4] * d[1] + r[5] * d[2];
        let z = r[6] * d[0] + r[7] * d[1] + r[8] * d[2];
        if z <= 1e-9 {
            return None;
        }
        let (xn, yn) = (x / z, y / z);
        let r2 = xn * xn + yn * yn;
        let [k1, k2, k3, k4] = self.radial;
        let [t1, t2] = self.tangential;
        let s = 1.0 + r2 * (k1 + r2 * (k2 + r2 * (k3 + r2 * k4)));
        let xd = xn * s + 2.0 * t1 * xn * yn + t2 * (r2 + 2.0 * xn * xn);
        let yd = yn * s + t1 * (r2 + 2.0 * yn * yn) + 2.0 * t2 * xn * yn;
        let long = self.width.max(self.height) as f64;
        let f = self.focal * long;
        Some((
            self.width as f64 * 0.5 + xd * f + self.principal[0] * long,
            self.height as f64 * 0.5 + yd * f + self.principal[1] * long,
        ))
    }

    /// Unit vector from a world point towards the camera centre.
    pub fn direction_from(&self, p: [f64; 3]) -> [f64; 3] {
        let d = [self.position[0] - p[0], self.position[1] - p[1], self.position[2] - p[2]];
        let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
        if n <= 1e-12 { [0.0; 3] } else { [d[0] / n, d[1] / n, d[2] / n] }
    }

    /// `project` restricted to points that land inside the image.
    pub fn project_inside(&self, p: [f64; 3]) -> Option<(f64, f64)> {
        self.project(p)
            .filter(|(x, y)| *x >= 0.0 && *y >= 0.0 && *x < self.width as f64 && *y < self.height as f64)
    }
}

pub fn parse_points(text: &str) -> SparseCloud {
    let mut cloud = SparseCloud::default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(x), Some(y), Some(z)) = (it.next(), it.next(), it.next()) else { continue };
        let (Ok(x), Ok(y), Ok(z)) = (x.parse::<f32>(), y.parse::<f32>(), z.parse::<f32>()) else { continue };
        let mut color = [255u8; 3];
        for c in &mut color {
            if let Some(v) = it.next().and_then(|s| s.parse::<f32>().ok()) {
                *c = v.round().clamp(0.0, 255.0) as u8;
            }
        }
        let track = it.next().and_then(|s| s.parse::<u16>().ok()).unwrap_or(0);
        cloud.positions.push([x, y, z]);
        cloud.colors.push(color);
        cloud.track_lengths.push(track);
    }
    cloud
}

/// Parse [`CAMERAS_TEMPLATE`] output.
pub fn parse_cameras(text: &str) -> Result<Vec<CameraPose>, Error> {
    let raw: Vec<Option<CameraPose>> = serde_json::from_str(&rs_client::json::sanitize(text))?;
    Ok(raw.into_iter().flatten().collect())
}

pub async fn fetch_points(client: &rs_client::Client) -> Result<SparseCloud, Error> {
    Ok(parse_points(&client.template(POINTS_TEMPLATE).await?))
}

pub async fn fetch_cameras(client: &rs_client::Client) -> Result<Vec<CameraPose>, Error> {
    parse_cameras(&client.template(CAMERAS_TEMPLATE).await?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinhole_projection_matches_the_frustum_convention() {
        // Camera at the origin looking along +Z, 36 mm-equivalent focal on a 3000×2000 image.
        let cam = CameraPose {
            index: 0,
            image_index: 0,
            image: "a".into(),
            width: 3000,
            height: 2000,
            position: [0.0; 3],
            rotation: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            focal: 1.0,
            principal: [0.0, 0.0],
            radial: [0.0; 4],
            tangential: [0.0; 2],
        };
        assert_eq!(cam.project([0.0, 0.0, 10.0]), Some((1500.0, 1000.0)));
        let (x, y) = cam.project([1.0, -0.5, 10.0]).unwrap();
        assert!((x - 1800.0).abs() < 1e-9 && (y - 850.0).abs() < 1e-9, "{x} {y}: x right, y down");
        assert!(cam.project([0.0, 0.0, -1.0]).is_none(), "behind the camera");
        assert!(cam.project_inside([10.0, 0.0, 10.0]).is_none(), "outside the frame");
    }

    use super::*;

    #[test]
    fn brown_distortion_pushes_points_outwards_on_unit_focal_coordinates() {
        let mut cam = CameraPose {
            index: 0,
            image_index: 0,
            image: "a".into(),
            width: 3000,
            height: 2000,
            position: [0.0; 3],
            rotation: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            focal: 1.0,
            principal: [0.0, 0.0],
            radial: [0.1, 0.0, 0.0, 0.0],
            tangential: [0.0; 2],
        };
        // Normalised (0.3, 0): r² = 0.09, scale 1.009 → 0.3027 × 3000 px.
        let (x, y) = cam.project([0.3, 0.0, 1.0]).unwrap();
        assert!((x - (1500.0 + 0.3027 * 3000.0)).abs() < 1e-9 && (y - 1000.0).abs() < 1e-9, "{x} {y}");
        // The axis is unaffected, and a tangential term shifts a point on the axis too.
        assert_eq!(cam.project([0.0, 0.0, 1.0]), Some((1500.0, 1000.0)));
        cam.tangential = [0.0, 0.01];
        let (x, _) = cam.project([0.3, 0.0, 1.0]).unwrap();
        assert!((x - (1500.0 + (0.3027 + 0.01 * (0.09 + 0.18)) * 3000.0)).abs() < 1e-9, "{x}");
        assert_eq!(cam.direction_from([0.0, 0.0, 2.0]), [0.0, 0.0, -1.0]);
    }

    #[test]
    fn parses_point_lines_and_skips_junk() {
        let cloud = parse_points("1.5 -2 3 255 128 0 4\n\nnot a point\n0 0 0 1 2 3 2\n");
        assert_eq!(cloud.len(), 2);
        assert_eq!(cloud.positions[0], [1.5, -2.0, 3.0]);
        assert_eq!(cloud.colors[0], [255, 128, 0]);
        assert_eq!(cloud.track_lengths, vec![4, 2]);
    }

    #[test]
    fn parses_cameras_with_sentinel() {
        let text = r#"[{"index":0,"imageIndex":7,"image":"img_001","width":5504,"height":8256,"position":[1,2,3],"rotation":[1,0,0,0,1,0,0,0,1],"focal":28.5,"principal":[0.001,-0.002],"radial":[0.16,1.8,-4.5,0],"tangential":[0,0]},{"index":1,"imageIndex":8,"image":"old","width":4,"height":3,"position":[0,0,0],"rotation":[1,0,0,0,1,0,0,0,1],"focal":1,"principal":[0,0]},null]"#;
        let cams = parse_cameras(text).unwrap();
        assert_eq!(cams.len(), 2);
        assert_eq!(cams[0].radial, [0.16, 1.8, -4.5, 0.0]);
        assert_eq!(cams[1].radial, [0.0; 4], "missing coefficients mean no distortion");
        assert_eq!(cams[0].image, "img_001");
        assert_eq!(cams[0].image_index, 7);
        assert_eq!(cams[0].rotation[4], 1.0);
    }

    #[test]
    fn templates_have_null_sentinels_or_line_format() {
        assert!(CAMERAS_TEMPLATE.contains(",)null]"));
        assert!(POINTS_TEMPLATE.contains("$ExportPoints("));
    }
}
