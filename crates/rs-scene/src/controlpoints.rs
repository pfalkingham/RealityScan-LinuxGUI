//! Control points, their image measurements, and distance constraints.
//!
//! Read through `templates/controlpoints.tpl`. RealityScan has no command to add a
//! measurement directly; they are imported from a CSV (`importControlPointsMeasurements`),
//! which [`measurements_csv`] writes in the default `Image, Point, X, Y` format.

use serde::Deserialize;

use crate::Error;

pub const CONTROL_POINTS_TEMPLATE: &str = include_str!("../templates/controlpoints.tpl");

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ControlPointData {
    pub points: Vec<ControlPoint>,
    pub measurements: Vec<Measurement>,
    pub constraints: Vec<DistanceConstraint>,
}

impl ControlPointData {
    /// Measurements of one control point (by index).
    pub fn measurements_of(&self, point: u32) -> impl Iterator<Item = &Measurement> {
        self.measurements.iter().filter(move |m| m.point == point)
    }

    /// Measurements placed on an image (RealityScan path, as the scene lists images).
    pub fn measurements_on(&self, image_path: &str) -> impl Iterator<Item = &Measurement> {
        self.measurements.iter().filter(move |m| m.image.eq_ignore_ascii_case(image_path))
    }

    pub fn point_by_name(&self, name: &str) -> Option<&ControlPoint> {
        self.points.iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlPoint {
    pub index: u32,
    pub name: String,
    pub measurement_count: u32,
    /// 0 = tie point, 1 = ground control, 2 = ground test.
    pub calibration_type: u8,
    pub aligned: bool,
    /// Triangulated position (Euclidean/internal coordinates), when aligned.
    #[serde(default)]
    pub x: Option<f64>,
    #[serde(default)]
    pub y: Option<f64>,
    #[serde(default)]
    pub z: Option<f64>,
    /// Largest reprojection error over the point's measurements, in pixels.
    #[serde(default)]
    pub error: Option<f64>,
}

impl ControlPoint {
    pub fn position(&self) -> Option<[f64; 3]> {
        Some([self.x?, self.y?, self.z?])
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Measurement {
    /// Control point index.
    pub point: u32,
    pub point_name: String,
    /// Index of the measurement within its control point.
    pub measurement: u32,
    pub image_index: u32,
    /// Full RealityScan path of the image.
    pub image: String,
    /// Pixel coordinates in the full image, origin top-left.
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DistanceConstraint {
    pub name: String,
    pub a: String,
    pub b: String,
    pub enabled: bool,
    pub defined: f64,
    pub calculated: f64,
    pub error: f64,
}

#[derive(Deserialize)]
struct Raw {
    points: Vec<Option<ControlPoint>>,
    measurements: Vec<Option<Measurement>>,
    constraints: Vec<Option<DistanceConstraint>>,
}

pub fn parse_control_points(text: &str) -> Result<ControlPointData, Error> {
    // A point with too few measurements reports its position as `-nan(ind)`.
    let text = text
        .replace("-nan(ind)", "null")
        .replace("nan(ind)", "null")
        .replace(":inf", ":null")
        .replace(":-inf", ":null");
    let raw: Raw = serde_json::from_str(&rs_client::json::sanitize(&text))?;
    Ok(ControlPointData {
        points: raw.points.into_iter().flatten().collect(),
        measurements: raw.measurements.into_iter().flatten().collect(),
        constraints: raw.constraints.into_iter().flatten().collect(),
    })
}

pub async fn fetch_control_points(client: &rs_client::Client) -> Result<ControlPointData, Error> {
    let text = client.template(CONTROL_POINTS_TEMPLATE).await?;
    parse_control_points(&text)
}

/// One line of a measurements CSV in RealityScan's default import format
/// (`Image, Point, X, Y`). `image` is the image name as RealityScan matches it.
pub fn measurements_csv(rows: &[(String, String, f64, f64)]) -> String {
    let mut out = String::from("# image, control point, x, y\n");
    for (image, point, x, y) in rows {
        out.push_str(&format!("\"{}\",\"{}\",{x},{y}\n", image.replace('"', ""), point.replace('"', "")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_points_measurements_and_constraints() {
        let text = r#"{"points":[{"index":0,"name":"cp1","measurementCount":2,"calibrationType":0,"aligned":true,"x":1.5,"y":-2.0,"z":3.25},{"index":1,"name":"cp2","measurementCount":1,"calibrationType":0,"aligned":false},null],
"measurements":[{"point":0,"pointName":"cp1","measurement":0,"imageIndex":3,"image":"Z:\\photos\\a.jxl","x":100.5,"y":200.25},null],
"constraints":[{"name":"d1","a":"cp1","b":"cp2","enabled":true,"defined":100.0,"calculated":98.5,"error":-1.5},null]}"#;
        let d = parse_control_points(text).unwrap();
        assert_eq!(d.points.len(), 2);
        assert_eq!(d.points[0].position(), Some([1.5, -2.0, 3.25]));
        assert_eq!(d.points[1].position(), None);
        assert_eq!(d.measurements_of(0).count(), 1);
        assert_eq!(d.measurements_on(r"z:\photos\A.JXL").count(), 1);
        assert_eq!(d.constraints[0].error, -1.5);
    }

    #[test]
    fn nan_positions_become_none() {
        let d = parse_control_points(
            r#"{"points":[{"index":0,"name":"cp1","measurementCount":1,"calibrationType":0,"aligned":true,"x":-nan(ind),"y":-nan(ind),"z":-nan(ind)},null],"measurements":[null],"constraints":[null]}"#,
        )
        .unwrap();
        assert_eq!(d.points[0].position(), None);
    }

    #[test]
    fn empty_lists_parse() {
        let d =
            parse_control_points(r#"{"points":[null],"measurements":[null],"constraints":[null]}"#).unwrap();
        assert!(d.points.is_empty() && d.measurements.is_empty() && d.constraints.is_empty());
    }

    #[test]
    fn csv_has_header_and_quoted_fields() {
        let csv = measurements_csv(&[("image_001.jxl".into(), "cp1".into(), 10.0, 20.5)]);
        assert_eq!(csv, "# image, control point, x, y\n\"image_001.jxl\",\"cp1\",10,20.5\n");
    }
}
